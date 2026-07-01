use std::collections::HashMap;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::port::types::UsbDevice;

pub const DEFAULT_CHANNEL_CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(pub String);

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<&str> for SessionId {
    fn from(s: &str) -> Self {
        SessionId(s.to_string())
    }
}

impl From<String> for SessionId {
    fn from(s: String) -> Self {
        SessionId(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BusId(pub String);

impl std::fmt::Display for BusId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<&str> for BusId {
    fn from(s: &str) -> Self {
        BusId(s.to_string())
    }
}

impl From<String> for BusId {
    fn from(s: String) -> Self {
        BusId(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceState {
    Idle,
    Bound,
    Forwarded(SessionId),
    Disconnected,
}

#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub device: UsbDevice,
    pub state: DeviceState,
}

#[derive(Debug, Clone, Default)]
pub struct RegistrySnapshot {
    pub devices: HashMap<BusId, DeviceEntry>,
    pub forwardings: HashMap<SessionId, Vec<BusId>>,
}

#[derive(Debug, Clone)]
pub enum RegistryCommand {
    AddDevice(UsbDevice),
    RemoveDevice(BusId),
    TransitionState(BusId, DeviceState),
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum RegistryError {
    #[error("channel closed")]
    ChannelClosed,
}

#[derive(Clone)]
pub struct RegistryHandle {
    tx: mpsc::Sender<RegistryCommand>,
}

impl RegistryHandle {
    pub async fn send(&self, cmd: RegistryCommand) -> Result<(), RegistryError> {
        self.tx.send(cmd).await.map_err(|_| RegistryError::ChannelClosed)
    }
}

pub struct DeviceRegistry {
    handle: RegistryHandle,
    watch_tx: watch::Sender<Arc<RegistrySnapshot>>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    _task: JoinHandle<()>,
}

impl DeviceRegistry {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CHANNEL_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(capacity);
        let (watch_tx, _) = watch::channel(Arc::new(RegistrySnapshot::default()));
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        let handle = RegistryHandle { tx: cmd_tx };
        let watch_tx_clone = watch_tx.clone();

        let task = tokio::spawn(async move {
            run_registry(cmd_rx, watch_tx_clone, shutdown_rx).await;
        });

        DeviceRegistry {
            handle,
            watch_tx,
            shutdown_tx: Some(shutdown_tx),
            _task: task,
        }
    }

    pub fn handle(&self) -> &RegistryHandle {
        &self.handle
    }

    pub fn snapshot(&self) -> watch::Receiver<Arc<RegistrySnapshot>> {
        self.watch_tx.subscribe()
    }

    pub async fn shutdown(mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), &mut self._task).await;
    }
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DeviceRegistry {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
    }
}

async fn run_registry(
    mut cmd_rx: mpsc::Receiver<RegistryCommand>,
    snap_tx: watch::Sender<Arc<RegistrySnapshot>>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let mut state = RegistrySnapshot::default();

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    break;
                };
                handle_command(cmd, &mut state);
                let _ = snap_tx.send(Arc::new(state.clone()));
            }
            _ = &mut shutdown_rx => {
                tracing::info!("registry shutting down gracefully");
                break;
            }
        }
    }
}

fn handle_command(cmd: RegistryCommand, state: &mut RegistrySnapshot) {
    match cmd {
        RegistryCommand::AddDevice(device) => {
            let busid = BusId(device.busid.clone());
            use std::collections::hash_map::Entry;
            match state.devices.entry(busid) {
                Entry::Occupied(mut entry) => {
                    if entry.get().state == DeviceState::Disconnected {
                        tracing::info!("device re-added: {:?}", entry.key());
                        entry.insert(DeviceEntry {
                            device,
                            state: DeviceState::Idle,
                        });
                    } else {
                        tracing::warn!("duplicate AddDevice for busid {:?}, skipping", entry.key());
                    }
                }
                Entry::Vacant(entry) => {
                    let busid_str = entry.key().0.clone();
                    tracing::info!("device added: {busid_str}");
                    entry.insert(DeviceEntry {
                        device,
                        state: DeviceState::Idle,
                    });
                }
            }
        }
        RegistryCommand::RemoveDevice(busid) => {
            if let Some(entry) = state.devices.remove(&busid) {
                tracing::info!("device removed: {:?}", busid);
                if let DeviceState::Forwarded(ref sid) = entry.state
                    && let Some(buses) = state.forwardings.get_mut(sid)
                {
                    buses.retain(|b| *b != busid);
                    if buses.is_empty() {
                        state.forwardings.remove(sid);
                    }
                }
            } else {
                tracing::warn!("RemoveDevice for unknown busid {:?}, ignoring", busid);
            }
        }
        RegistryCommand::TransitionState(busid, new_state) => {
            let Some(entry) = state.devices.get_mut(&busid) else {
                tracing::warn!("TransitionState for unknown busid {:?}, ignoring", busid);
                return;
            };

            let old_state = entry.state.clone();
            if !is_valid_transition(&old_state, &new_state) {
                tracing::warn!(
                    "invalid transition for {:?}: {:?} -> {:?}, ignoring",
                    busid,
                    old_state,
                    new_state
                );
                return;
            }

            if let DeviceState::Forwarded(ref sid) = old_state
                && let Some(buses) = state.forwardings.get_mut(sid)
            {
                buses.retain(|b| *b != busid);
                if buses.is_empty() {
                    state.forwardings.remove(sid);
                }
            }

            if let DeviceState::Forwarded(ref sid) = new_state {
                let buses = state.forwardings.entry(sid.clone()).or_default();
                if !buses.contains(&busid) {
                    buses.push(busid.clone());
                }
            }

            entry.state = new_state;
            tracing::info!("device {:?} transitioned to {:?}", busid, entry.state);
        }
    }
}

fn is_valid_transition(from: &DeviceState, to: &DeviceState) -> bool {
    use DeviceState::*;
    match (from, to) {
        (Idle, Bound) => true,
        (Idle, Disconnected) => true,
        (Bound, Forwarded(_)) => true,
        (Bound, Disconnected) => true,
        (Forwarded(_), Idle) => true,
        (Forwarded(_), Disconnected) => true,
        (Disconnected, _) => false,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::watch;
    use tokio::time::{Duration, timeout};

    use super::*;
    use crate::port::types::UsbDevice;
    use crate::protocol::UsbDeviceSpeed;

    fn make_device(busid: &str, vid: u16, pid: u16) -> UsbDevice {
        UsbDevice {
            busid: busid.to_string(),
            vid,
            pid,
            manufacturer: None,
            product: None,
            busnum: 1,
            devnum: 1,
            speed: UsbDeviceSpeed::HighSpeed,
            b_device_class: 0,
            b_device_sub_class: 0,
            b_device_protocol: 0,
        }
    }

    async fn wait_for(
        rx: &mut watch::Receiver<Arc<RegistrySnapshot>>,
        predicate: impl Fn(&RegistrySnapshot) -> bool,
    ) -> bool {
        timeout(Duration::from_secs(2), async {
            loop {
                if predicate(&rx.borrow_and_update()) {
                    return true;
                }
                if rx.changed().await.is_err() {
                    return false;
                }
            }
        })
        .await
        .unwrap_or(false)
    }

    fn busid(s: &str) -> BusId {
        BusId(s.to_string())
    }

    fn session_id(s: &str) -> SessionId {
        SessionId(s.to_string())
    }

    #[tokio::test]
    async fn registry_starts_empty() {
        let registry = DeviceRegistry::new();
        let rx = registry.snapshot();
        let snap = rx.borrow();
        assert!(snap.devices.is_empty());
        assert!(snap.forwardings.is_empty());
    }

    #[tokio::test]
    async fn add_device_appears_in_snapshot() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry
            .handle()
            .send(RegistryCommand::AddDevice(dev.clone()))
            .await
            .unwrap();

        let found = wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;
        assert!(found, "device should appear in snapshot");

        let snap = rx.borrow();
        let entry = snap.devices.get(&busid("1-1")).unwrap();
        assert_eq!(entry.device.busid, "1-1");
        assert_eq!(entry.device.vid, 0x1234);
        assert_eq!(entry.device.pid, 0x5678);
        assert_eq!(entry.state, DeviceState::Idle);
    }

    #[tokio::test]
    async fn add_duplicate_device_skipped() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev1 = make_device("1-1", 0x1234, 0x5678);
        let dev2 = make_device("1-1", 0xAAAA, 0xBBBB);

        registry.handle().send(RegistryCommand::AddDevice(dev1)).await.unwrap();
        let found = wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;
        assert!(found);

        registry.handle().send(RegistryCommand::AddDevice(dev2)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let snap = rx.borrow();
        assert_eq!(snap.devices.len(), 1);
        let entry = snap.devices.get(&busid("1-1")).unwrap();
        assert_eq!(entry.device.vid, 0x1234, "original device should be kept");
    }

    #[tokio::test]
    async fn remove_device_cleans_up() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        let found = wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;
        assert!(found);

        registry
            .handle()
            .send(RegistryCommand::RemoveDevice(busid("1-1")))
            .await
            .unwrap();
        let removed = wait_for(&mut rx, |s| !s.devices.contains_key(&busid("1-1"))).await;
        assert!(removed, "device should be removed from snapshot");

        let snap = rx.borrow();
        assert!(snap.devices.is_empty());
    }

    #[tokio::test]
    async fn remove_unknown_device_no_panic() {
        let registry = DeviceRegistry::new();
        let result = registry
            .handle()
            .send(RegistryCommand::RemoveDevice(busid("nonexistent")));
        assert!(
            result.await.is_ok(),
            "sending remove for unknown device should not error on send"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn transition_idle_to_bound() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        let found = wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;
        assert!(found);

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Bound))
            .await
            .unwrap();

        let transitioned = wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;
        assert!(transitioned, "device should transition to Bound");
    }

    #[tokio::test]
    async fn transition_bound_to_forwarded_updates_forwardings() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Bound))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                busid("1-1"),
                DeviceState::Forwarded(session_id("s1")),
            ))
            .await
            .unwrap();

        let forwarded = wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == &session_id("s1")))
        })
        .await;
        assert!(forwarded, "device should be Forwarded(s1)");

        let snap = rx.borrow();
        let fwd = snap.forwardings.get(&session_id("s1")).unwrap();
        assert!(fwd.contains(&busid("1-1")), "forwardings[s1] should contain busid 1-1");
    }

    #[tokio::test]
    async fn transition_forwarded_to_idle_removes_from_forwardings() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Bound))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                busid("1-1"),
                DeviceState::Forwarded(session_id("s1")),
            ))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == &session_id("s1")))
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Idle))
            .await
            .unwrap();

        let back_to_idle = wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Idle)
        })
        .await;
        assert!(back_to_idle, "device should return to Idle");

        let snap = rx.borrow();
        let fwd_empty = snap
            .forwardings
            .get(&session_id("s1"))
            .is_none_or(|buses| buses.is_empty());
        assert!(fwd_empty, "forwardings for s1 should be empty");
    }

    #[tokio::test]
    async fn transition_cycle() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Bound))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                busid("1-1"),
                DeviceState::Forwarded(session_id("s1")),
            ))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == &session_id("s1")))
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Idle))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Idle)
        })
        .await;

        let snap = rx.borrow();
        let fwd_empty = snap
            .forwardings
            .get(&session_id("s1"))
            .is_none_or(|buses| buses.is_empty());
        assert!(fwd_empty, "forwardings should be cleaned up after cycle");
    }

    #[tokio::test]
    async fn invalid_transition_idle_to_forwarded_rejected() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                busid("1-1"),
                DeviceState::Forwarded(session_id("s1")),
            ))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;

        let snap = rx.borrow();
        let entry = snap.devices.get(&busid("1-1")).unwrap();
        assert_eq!(
            entry.state,
            DeviceState::Idle,
            "state should remain Idle after invalid transition"
        );
    }

    #[tokio::test]
    async fn unknown_device_transition_no_panic() {
        let registry = DeviceRegistry::new();
        let result = registry.handle().send(RegistryCommand::TransitionState(
            busid("nonexistent"),
            DeviceState::Bound,
        ));
        assert!(result.await.is_ok(), "send should not error");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn concurrent_commands_no_deadlock() {
        let registry = DeviceRegistry::new();
        let handle = registry.handle().clone();
        let mut rx = registry.snapshot();

        let mut join_handles = Vec::new();
        for i in 0..5 {
            let h = handle.clone();
            join_handles.push(tokio::spawn(async move {
                for j in 0..10 {
                    let busid_str = format!("{}-{}", i, j);
                    let dev = make_device(&busid_str, 0x1000 + i as u16, 0x2000 + j as u16);
                    let _ = h.send(RegistryCommand::AddDevice(dev)).await;
                    let _ = h
                        .send(RegistryCommand::TransitionState(busid(&busid_str), DeviceState::Bound))
                        .await;
                    let _ = h
                        .send(RegistryCommand::TransitionState(busid(&busid_str), DeviceState::Idle))
                        .await;
                }
            }));
        }

        for jh in join_handles {
            jh.await.unwrap();
        }

        let all_processed = wait_for(&mut rx, |s| s.devices.len() == 50).await;
        assert!(
            all_processed,
            "all 50 devices should be registered, got {}",
            rx.borrow().devices.len()
        );

        let snap = rx.borrow();
        for entry in snap.devices.values() {
            assert!(
                matches!(entry.state, DeviceState::Idle | DeviceState::Bound),
                "each device should be in a valid state"
            );
        }
    }

    #[tokio::test]
    async fn multiple_readers_via_snapshot() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        let found = wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;
        assert!(found, "device should appear in snapshot");

        let rx1 = registry.snapshot();
        let rx2 = registry.snapshot();

        let snap1 = rx1.borrow();
        let snap2 = rx2.borrow();

        assert_eq!(snap1.devices.len(), 1);
        assert_eq!(snap2.devices.len(), 1);
        assert_eq!(
            snap1.devices.get(&busid("1-1")).unwrap().device.vid,
            snap2.devices.get(&busid("1-1")).unwrap().device.vid
        );
    }

    #[tokio::test]
    async fn handle_clone_and_send_from_multiple_tasks() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();

        let h1 = registry.handle().clone();
        let h2 = registry.handle().clone();
        let h3 = registry.handle().clone();

        let t1 = tokio::spawn(async move {
            let dev = make_device("1-1", 0x1111, 0x1111);
            h1.send(RegistryCommand::AddDevice(dev)).await.unwrap();
        });
        let t2 = tokio::spawn(async move {
            let dev = make_device("2-2", 0x2222, 0x2222);
            h2.send(RegistryCommand::AddDevice(dev)).await.unwrap();
        });
        let t3 = tokio::spawn(async move {
            let dev = make_device("3-3", 0x3333, 0x3333);
            h3.send(RegistryCommand::AddDevice(dev)).await.unwrap();
        });

        t1.await.unwrap();
        t2.await.unwrap();
        t3.await.unwrap();

        let all_present = wait_for(&mut rx, |s| {
            s.devices.contains_key(&busid("1-1"))
                && s.devices.contains_key(&busid("2-2"))
                && s.devices.contains_key(&busid("3-3"))
        })
        .await;
        assert!(
            all_present,
            "all three devices from different tasks should be registered"
        );
    }

    #[tokio::test]
    async fn disconnect_removes_from_forwardings() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Bound))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                busid("1-1"),
                DeviceState::Forwarded(session_id("s1")),
            ))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == &session_id("s1")))
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                busid("1-1"),
                DeviceState::Disconnected,
            ))
            .await
            .unwrap();

        let disconnected = wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Disconnected)
        })
        .await;
        assert!(disconnected, "device should be Disconnected");

        let snap = rx.borrow();
        assert!(
            snap.forwardings
                .get(&session_id("s1"))
                .is_none_or(|buses| buses.is_empty()),
            "s1 should be removed from forwardings after disconnect"
        );
    }

    #[tokio::test]
    async fn registry_handle_send_after_drop_returns_channel_closed() {
        let registry = DeviceRegistry::new();
        let handle = registry.handle().clone();

        drop(registry);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let dev = make_device("1-1", 0x1234, 0x5678);
        let result = handle.send(RegistryCommand::AddDevice(dev)).await;
        assert!(
            matches!(result, Err(RegistryError::ChannelClosed)),
            "send after registry drop should return ChannelClosed, got {result:?}"
        );
    }

    #[tokio::test]
    async fn remove_forwarded_device_cleans_forwardings() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Bound))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                busid("1-1"),
                DeviceState::Forwarded(session_id("s1")),
            ))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == &session_id("s1")))
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::RemoveDevice(busid("1-1")))
            .await
            .unwrap();

        let removed = wait_for(&mut rx, |s| !s.devices.contains_key(&busid("1-1"))).await;
        assert!(removed, "device should be removed");

        let snap = rx.borrow();
        let not_in_fwd = snap
            .forwardings
            .get(&session_id("s1"))
            .is_none_or(|buses| !buses.contains(&busid("1-1")));
        assert!(not_in_fwd, "device should be removed from forwardings");
    }

    #[tokio::test]
    async fn transition_idle_to_disconnected() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                busid("1-1"),
                DeviceState::Disconnected,
            ))
            .await
            .unwrap();

        let ok = wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Disconnected)
        })
        .await;
        assert!(ok, "device should transition to Disconnected");
    }

    #[tokio::test]
    async fn transition_bound_to_disconnected() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Bound))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                busid("1-1"),
                DeviceState::Disconnected,
            ))
            .await
            .unwrap();

        let ok = wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Disconnected)
        })
        .await;
        assert!(ok, "device should transition from Bound to Disconnected");
    }

    #[tokio::test]
    async fn disconnected_is_terminal() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                busid("1-1"),
                DeviceState::Disconnected,
            ))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Disconnected)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Idle))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let snap = rx.borrow();
        assert_eq!(
            snap.devices.get(&busid("1-1")).unwrap().state,
            DeviceState::Disconnected,
            "state should remain Disconnected (terminal)"
        );
    }

    #[tokio::test]
    async fn multiple_devices_same_session_partial_disconnect() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev1 = make_device("1-1", 0x1111, 0x1111);
        let dev2 = make_device("2-2", 0x2222, 0x2222);

        for (dev, busid_str) in [(dev1, "1-1"), (dev2, "2-2")] {
            let bid = busid(busid_str);
            registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
            wait_for(&mut rx, |s| s.devices.contains_key(&bid)).await;

            registry
                .handle()
                .send(RegistryCommand::TransitionState(bid.clone(), DeviceState::Bound))
                .await
                .unwrap();
            wait_for(&mut rx, |s| {
                s.devices.get(&bid).is_some_and(|e| e.state == DeviceState::Bound)
            })
            .await;

            registry
                .handle()
                .send(RegistryCommand::TransitionState(
                    bid.clone(),
                    DeviceState::Forwarded(session_id("s1")),
                ))
                .await
                .unwrap();
            wait_for(&mut rx, |s| {
                s.devices
                    .get(&bid)
                    .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == &session_id("s1")))
            })
            .await;
        }

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                busid("1-1"),
                DeviceState::Disconnected,
            ))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Disconnected)
        })
        .await;

        let snap = rx.borrow();
        assert_eq!(snap.forwardings.get(&session_id("s1")).map(|b| b.len()), Some(1));
        assert!(
            snap.forwardings
                .get(&session_id("s1"))
                .is_some_and(|buses| buses.contains(&busid("2-2"))),
            "2-2 should still be in forwardings"
        );
    }

    #[tokio::test]
    async fn invalid_transition_bound_to_idle_rejected() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Bound))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Idle))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let snap = rx.borrow();
        assert_eq!(
            snap.devices.get(&busid("1-1")).unwrap().state,
            DeviceState::Bound,
            "Bound -> Idle should be rejected"
        );
    }

    #[tokio::test]
    async fn forwarded_to_different_forwarded_rejected() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Bound))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                busid("1-1"),
                DeviceState::Forwarded(session_id("s1")),
            ))
            .await
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get(&busid("1-1"))
                .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == &session_id("s1")))
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                busid("1-1"),
                DeviceState::Forwarded(session_id("s2")),
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let snap = rx.borrow();
        let entry = snap.devices.get(&busid("1-1")).unwrap();
        assert!(
            matches!(&entry.state, DeviceState::Forwarded(sid) if sid == &session_id("s1")),
            "Forwarded(s1) -> Forwarded(s2) should be rejected"
        );
    }

    #[tokio::test]
    async fn shutdown_drains_gracefully() {
        let registry = DeviceRegistry::new();
        let dev = make_device("1-1", 0x1234, 0x5678);
        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        registry.shutdown().await;
    }

    #[tokio::test]
    async fn at_most_once_delivery_per_transition() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).await.unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key(&busid("1-1"))).await;

        let mut transition_count = 0u32;
        let mut snapshot_rx = registry.snapshot();

        registry
            .handle()
            .send(RegistryCommand::TransitionState(busid("1-1"), DeviceState::Bound))
            .await
            .unwrap();

        loop {
            if timeout(Duration::from_secs(2), snapshot_rx.changed()).await.is_err() {
                break;
            }
            transition_count += 1;
            if snapshot_rx
                .borrow()
                .devices
                .get(&busid("1-1"))
                .is_some_and(|e| e.state == DeviceState::Bound)
            {
                break;
            }
        }

        assert!(
            transition_count > 0,
            "at least one snapshot transition should be delivered"
        );
        assert!(
            transition_count <= 3,
            "should not receive excessive transitions for a single command, got {transition_count}"
        );
    }

    #[tokio::test]
    async fn bounded_channel_backpressure_prevents_unbounded_growth() {
        let registry = DeviceRegistry::with_capacity(1);
        let handle = registry.handle().clone();

        let dev1 = make_device("1-1", 0x1234, 0x5678);
        let dev2 = make_device("2-2", 0xAAAA, 0xBBBB);
        let dev3 = make_device("3-3", 0xCCCC, 0xDDDD);

        // Fill the single-slot buffer synchronously before consumer can drain
        // tokio::sync::mpsc send().await blocks when full, providing backpressure
        let t1 = handle.send(RegistryCommand::AddDevice(dev1)).await;
        assert!(t1.is_ok(), "first send should succeed");

        // Send blocks until consumer drains — spawn it so we can observe
        let handle2 = handle.clone();
        let jh = tokio::spawn(async move { handle2.send(RegistryCommand::AddDevice(dev2)).await });

        // Consumer will drain the first item, unblocking the second send
        let mut rx = registry.snapshot();
        let both_present = wait_for(&mut rx, |s| s.devices.len() >= 2).await;
        assert!(
            both_present,
            "both devices should be registered after backpressure resolves"
        );

        let t2_result = timeout(Duration::from_secs(2), jh).await.unwrap().unwrap();
        assert!(t2_result.is_ok(), "second send should succeed after consumer drains");

        // Third send — regular backpressure behavior
        let t3 = handle.send(RegistryCommand::AddDevice(dev3)).await;
        assert!(t3.is_ok(), "third send should succeed");
    }

    #[test]
    fn session_id_hash_and_ord() {
        let a = session_id("a");
        let b = session_id("b");
        let a2 = session_id("a");
        assert_eq!(a, a2);
        assert_ne!(a, b);
        assert!(a < b);

        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(a.clone());
        assert!(set.contains(&a2));
        assert!(!set.contains(&b));
    }

    #[test]
    fn bus_id_hash_and_ord() {
        let a = busid("1-1");
        let b = busid("2-2");
        let a2 = busid("1-1");
        assert_eq!(a, a2);
        assert_ne!(a, b);
        assert!(a < b);

        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(a.clone());
        assert!(set.contains(&a2));
        assert!(!set.contains(&b));
    }

    #[tokio::test]
    async fn shutdown_drains_pending_commands_before_stop() {
        let registry = DeviceRegistry::with_capacity(8);
        let handle = registry.handle().clone();

        for i in 0..4 {
            let busid_str = format!("{}-{}", i, i);
            let dev = make_device(&busid_str, 0x1000 + i as u16, 0x2000 + i as u16);
            handle.send(RegistryCommand::AddDevice(dev)).await.unwrap();
        }

        registry.shutdown().await;

        let result = handle
            .send(RegistryCommand::AddDevice(make_device("99-99", 0x9999, 0x9999)))
            .await;
        assert!(matches!(result, Err(RegistryError::ChannelClosed)));
    }
}
