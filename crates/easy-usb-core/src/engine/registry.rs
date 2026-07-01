use std::collections::HashMap;

use thiserror::Error;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::port::types::UsbDevice;

pub type SessionId = String;
pub type BusId = String;

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
    #[error("device not found: {busid}")]
    DeviceNotFound { busid: BusId },
    #[error("invalid transition from {from:?} to {to:?} for {busid}")]
    InvalidTransition {
        busid: BusId,
        from: DeviceState,
        to: DeviceState,
    },
}

#[derive(Clone)]
pub struct RegistryHandle {
    tx: mpsc::UnboundedSender<RegistryCommand>,
}

impl RegistryHandle {
    pub fn send(&self, cmd: RegistryCommand) -> Result<(), RegistryError> {
        self.tx.send(cmd).map_err(|_| RegistryError::ChannelClosed)
    }
}

pub struct DeviceRegistry {
    handle: RegistryHandle,
    watch_tx: watch::Sender<RegistrySnapshot>,
    _task: JoinHandle<()>,
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceRegistry {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (watch_tx, _) = watch::channel(RegistrySnapshot::default());

        let handle = RegistryHandle { tx: cmd_tx };
        let watch_tx_clone = watch_tx.clone();

        let task = tokio::spawn(async move {
            run_registry(cmd_rx, watch_tx_clone).await;
        });

        DeviceRegistry {
            handle,
            watch_tx,
            _task: task,
        }
    }

    pub fn handle(&self) -> &RegistryHandle {
        &self.handle
    }

    pub fn snapshot(&self) -> watch::Receiver<RegistrySnapshot> {
        self.watch_tx.subscribe()
    }
}

impl Drop for DeviceRegistry {
    fn drop(&mut self) {
        self._task.abort();
    }
}

async fn run_registry(mut cmd_rx: mpsc::UnboundedReceiver<RegistryCommand>, snap_tx: watch::Sender<RegistrySnapshot>) {
    let mut state = RegistrySnapshot::default();

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            RegistryCommand::AddDevice(device) => {
                let busid = device.busid.clone();
                use std::collections::hash_map::Entry;
                match state.devices.entry(busid) {
                    Entry::Occupied(mut entry) => {
                        if entry.get().state == DeviceState::Disconnected {
                            tracing::info!("device re-added: {}", entry.key());
                            entry.insert(DeviceEntry {
                                device,
                                state: DeviceState::Idle,
                            });
                        } else {
                            tracing::warn!("duplicate AddDevice for busid {}, skipping", entry.key());
                            continue;
                        }
                    }
                    Entry::Vacant(entry) => {
                        let busid = entry.key().clone();
                        tracing::info!("device added: {busid}");
                        entry.insert(DeviceEntry {
                            device,
                            state: DeviceState::Idle,
                        });
                    }
                }
            }
            RegistryCommand::RemoveDevice(busid) => {
                if let Some(entry) = state.devices.remove(&busid) {
                    tracing::info!("device removed: {busid}");
                    if let DeviceState::Forwarded(ref sid) = entry.state
                        && let Some(buses) = state.forwardings.get_mut(sid)
                    {
                        buses.retain(|b| *b != busid);
                        if buses.is_empty() {
                            state.forwardings.remove(sid);
                        }
                    }
                } else {
                    tracing::warn!("RemoveDevice for unknown busid {busid}, ignoring");
                    continue;
                }
            }
            RegistryCommand::TransitionState(busid, new_state) => {
                let Some(entry) = state.devices.get_mut(&busid) else {
                    tracing::warn!("TransitionState for unknown busid {busid}, ignoring");
                    continue;
                };

                let old_state = entry.state.clone();
                if !is_valid_transition(&old_state, &new_state) {
                    tracing::warn!("invalid transition for {busid}: {old_state:?} -> {new_state:?}, ignoring");
                    continue;
                }

                // Remove from old forwardings if leaving Forwarded state
                if let DeviceState::Forwarded(ref sid) = old_state
                    && let Some(buses) = state.forwardings.get_mut(sid)
                {
                    buses.retain(|b| *b != busid);
                    if buses.is_empty() {
                        state.forwardings.remove(sid);
                    }
                }

                // Add to new forwardings if entering Forwarded state (defensive: no duplicates)
                if let DeviceState::Forwarded(ref sid) = new_state {
                    let buses = state.forwardings.entry(sid.clone()).or_default();
                    if !buses.contains(&busid) {
                        buses.push(busid.clone());
                    }
                }

                entry.state = new_state;
                tracing::info!("device {busid} transitioned to {:?}", entry.state);
            }
        }

        let _ = snap_tx.send(state.clone());
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
    use tokio::time::Duration;

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
        rx: &mut watch::Receiver<RegistrySnapshot>,
        predicate: impl Fn(&RegistrySnapshot) -> bool,
    ) -> bool {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if predicate(&*rx.borrow_and_update()) {
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

        registry.handle().send(RegistryCommand::AddDevice(dev.clone())).unwrap();

        let found = wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;
        assert!(found, "device should appear in snapshot");

        let snap = rx.borrow();
        let entry = snap.devices.get("1-1").unwrap();
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

        registry.handle().send(RegistryCommand::AddDevice(dev1)).unwrap();
        let found = wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;
        assert!(found);

        registry.handle().send(RegistryCommand::AddDevice(dev2)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let snap = rx.borrow();
        assert_eq!(snap.devices.len(), 1);
        let entry = snap.devices.get("1-1").unwrap();
        assert_eq!(entry.device.vid, 0x1234, "original device should be kept");
    }

    #[tokio::test]
    async fn remove_device_cleans_up() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        let found = wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;
        assert!(found);

        registry
            .handle()
            .send(RegistryCommand::RemoveDevice("1-1".to_string()))
            .unwrap();
        let removed = wait_for(&mut rx, |s| !s.devices.contains_key("1-1")).await;
        assert!(removed, "device should be removed from snapshot");

        let snap = rx.borrow();
        assert!(snap.devices.is_empty());
    }

    #[tokio::test]
    async fn remove_unknown_device_no_panic() {
        let registry = DeviceRegistry::new();
        let result = registry
            .handle()
            .send(RegistryCommand::RemoveDevice("nonexistent".to_string()));
        assert!(
            result.is_ok(),
            "sending remove for unknown device should not error on send"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn transition_idle_to_bound() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        let found = wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;
        assert!(found);

        registry
            .handle()
            .send(RegistryCommand::TransitionState("1-1".to_string(), DeviceState::Bound))
            .unwrap();

        let transitioned = wait_for(&mut rx, |s| {
            s.devices.get("1-1").is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;
        assert!(transitioned, "device should transition to Bound");
    }

    #[tokio::test]
    async fn transition_bound_to_forwarded_updates_forwardings() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState("1-1".to_string(), DeviceState::Bound))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices.get("1-1").is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                "1-1".to_string(),
                DeviceState::Forwarded("s1".to_string()),
            ))
            .unwrap();

        let forwarded = wait_for(&mut rx, |s| {
            s.devices
                .get("1-1")
                .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == "s1"))
        })
        .await;
        assert!(forwarded, "device should be Forwarded(s1)");

        let snap = rx.borrow();
        let fwd = snap.forwardings.get("s1").unwrap();
        assert!(
            fwd.contains(&"1-1".to_string()),
            "forwardings[s1] should contain busid 1-1"
        );
    }

    #[tokio::test]
    async fn transition_forwarded_to_idle_removes_from_forwardings() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState("1-1".to_string(), DeviceState::Bound))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices.get("1-1").is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                "1-1".to_string(),
                DeviceState::Forwarded("s1".to_string()),
            ))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get("1-1")
                .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == "s1"))
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState("1-1".to_string(), DeviceState::Idle))
            .unwrap();

        let back_to_idle = wait_for(&mut rx, |s| {
            s.devices.get("1-1").is_some_and(|e| e.state == DeviceState::Idle)
        })
        .await;
        assert!(back_to_idle, "device should return to Idle");

        let snap = rx.borrow();
        let fwd_empty = snap
            .forwardings
            .get("s1")
            .is_none_or(|buses| !buses.contains(&"1-1".to_string()));
        assert!(fwd_empty, "forwardings[s1] should no longer contain 1-1");
    }

    #[tokio::test]
    async fn transition_cycle() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;

        // Idle -> Bound
        registry
            .handle()
            .send(RegistryCommand::TransitionState("1-1".to_string(), DeviceState::Bound))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices.get("1-1").is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        // Bound -> Forwarded("s1")
        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                "1-1".to_string(),
                DeviceState::Forwarded("s1".to_string()),
            ))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get("1-1")
                .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == "s1"))
        })
        .await;

        // Forwarded("s1") -> Idle
        registry
            .handle()
            .send(RegistryCommand::TransitionState("1-1".to_string(), DeviceState::Idle))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices.get("1-1").is_some_and(|e| e.state == DeviceState::Idle)
        })
        .await;

        let snap = rx.borrow();
        let fwd_empty = snap.forwardings.get("s1").is_none_or(|buses| buses.is_empty());
        assert!(fwd_empty, "forwardings should be empty after full cycle");
    }

    #[tokio::test]
    async fn invalid_transition_idle_to_forwarded_rejected() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;

        // Idle -> Forwarded (invalid, must go through Bound first)
        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                "1-1".to_string(),
                DeviceState::Forwarded("s1".to_string()),
            ))
            .unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;

        let snap = rx.borrow();
        let entry = snap.devices.get("1-1").unwrap();
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
            "nonexistent".to_string(),
            DeviceState::Bound,
        ));
        assert!(result.is_ok(), "send should not error");
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
                    let busid = format!("{}-{}", i, j);
                    let dev = make_device(&busid, 0x1000 + i as u16, 0x2000 + j as u16);
                    let _ = h.send(RegistryCommand::AddDevice(dev));
                    let _ = h.send(RegistryCommand::TransitionState(busid.clone(), DeviceState::Bound));
                    let _ = h.send(RegistryCommand::TransitionState(busid, DeviceState::Idle));
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

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        let found = wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;
        assert!(found, "device should appear in snapshot");

        let rx1 = registry.snapshot();
        let rx2 = registry.snapshot();

        let snap1 = rx1.borrow();
        let snap2 = rx2.borrow();

        assert_eq!(snap1.devices.len(), 1);
        assert_eq!(snap2.devices.len(), 1);
        assert_eq!(
            snap1.devices.get("1-1").unwrap().device.vid,
            snap2.devices.get("1-1").unwrap().device.vid
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
            h1.send(RegistryCommand::AddDevice(dev)).unwrap();
        });
        let t2 = tokio::spawn(async move {
            let dev = make_device("2-2", 0x2222, 0x2222);
            h2.send(RegistryCommand::AddDevice(dev)).unwrap();
        });
        let t3 = tokio::spawn(async move {
            let dev = make_device("3-3", 0x3333, 0x3333);
            h3.send(RegistryCommand::AddDevice(dev)).unwrap();
        });

        t1.await.unwrap();
        t2.await.unwrap();
        t3.await.unwrap();

        let all_present = wait_for(&mut rx, |s| {
            s.devices.contains_key("1-1") && s.devices.contains_key("2-2") && s.devices.contains_key("3-3")
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

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState("1-1".to_string(), DeviceState::Bound))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices.get("1-1").is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                "1-1".to_string(),
                DeviceState::Forwarded("s1".to_string()),
            ))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get("1-1")
                .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == "s1"))
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                "1-1".to_string(),
                DeviceState::Disconnected,
            ))
            .unwrap();

        let disconnected = wait_for(&mut rx, |s| {
            s.devices
                .get("1-1")
                .is_some_and(|e| e.state == DeviceState::Disconnected)
        })
        .await;
        assert!(disconnected, "device should be Disconnected");

        let snap = rx.borrow();
        let not_in_fwd = snap
            .forwardings
            .get("s1")
            .is_none_or(|buses| !buses.contains(&"1-1".to_string()));
        assert!(not_in_fwd, "disconnected device should be removed from forwardings[s1]");
    }

    #[tokio::test]
    async fn registry_handle_send_after_drop_returns_channel_closed() {
        let registry = DeviceRegistry::new();
        let handle = registry.handle().clone();

        drop(registry);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let dev = make_device("1-1", 0x1234, 0x5678);
        let result = handle.send(RegistryCommand::AddDevice(dev));
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

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState("1-1".to_string(), DeviceState::Bound))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices.get("1-1").is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                "1-1".to_string(),
                DeviceState::Forwarded("s1".to_string()),
            ))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get("1-1")
                .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == "s1"))
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::RemoveDevice("1-1".to_string()))
            .unwrap();

        let removed = wait_for(&mut rx, |s| !s.devices.contains_key("1-1")).await;
        assert!(removed, "device should be removed");

        let snap = rx.borrow();
        let not_in_fwd = snap
            .forwardings
            .get("s1")
            .is_none_or(|buses| !buses.contains(&"1-1".to_string()));
        assert!(
            not_in_fwd,
            "removed forwarded device should be cleaned from forwardings[s1]"
        );
    }

    #[tokio::test]
    async fn transition_idle_to_disconnected() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                "1-1".to_string(),
                DeviceState::Disconnected,
            ))
            .unwrap();

        let ok = wait_for(&mut rx, |s| {
            s.devices
                .get("1-1")
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

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState("1-1".to_string(), DeviceState::Bound))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices.get("1-1").is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                "1-1".to_string(),
                DeviceState::Disconnected,
            ))
            .unwrap();

        let ok = wait_for(&mut rx, |s| {
            s.devices
                .get("1-1")
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

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                "1-1".to_string(),
                DeviceState::Disconnected,
            ))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get("1-1")
                .is_some_and(|e| e.state == DeviceState::Disconnected)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState("1-1".to_string(), DeviceState::Idle))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let snap = rx.borrow();
        assert_eq!(
            snap.devices.get("1-1").unwrap().state,
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

        for (dev, busid) in [(dev1, "1-1"), (dev2, "2-2")] {
            registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
            wait_for(&mut rx, |s| s.devices.contains_key(busid)).await;

            registry
                .handle()
                .send(RegistryCommand::TransitionState(busid.to_string(), DeviceState::Bound))
                .unwrap();
            wait_for(&mut rx, |s| {
                s.devices.get(busid).is_some_and(|e| e.state == DeviceState::Bound)
            })
            .await;

            registry
                .handle()
                .send(RegistryCommand::TransitionState(
                    busid.to_string(),
                    DeviceState::Forwarded("s1".to_string()),
                ))
                .unwrap();
            wait_for(&mut rx, |s| {
                s.devices
                    .get(busid)
                    .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == "s1"))
            })
            .await;
        }

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                "1-1".to_string(),
                DeviceState::Disconnected,
            ))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get("1-1")
                .is_some_and(|e| e.state == DeviceState::Disconnected)
        })
        .await;

        let snap = rx.borrow();
        assert!(
            snap.forwardings
                .get("s1")
                .is_some_and(|buses| !buses.contains(&"1-1".to_string()) && buses.contains(&"2-2".to_string())),
            "after disconnecting 1-1, forwardings[s1] should still contain 2-2 but not 1-1"
        );
    }

    #[tokio::test]
    async fn invalid_transition_bound_to_idle_rejected() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState("1-1".to_string(), DeviceState::Bound))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices.get("1-1").is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState("1-1".to_string(), DeviceState::Idle))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let snap = rx.borrow();
        assert_eq!(
            snap.devices.get("1-1").unwrap().state,
            DeviceState::Bound,
            "Bound -> Idle should be rejected"
        );
    }

    #[tokio::test]
    async fn forwarded_to_different_forwarded_rejected() {
        let registry = DeviceRegistry::new();
        let mut rx = registry.snapshot();
        let dev = make_device("1-1", 0x1234, 0x5678);

        registry.handle().send(RegistryCommand::AddDevice(dev)).unwrap();
        wait_for(&mut rx, |s| s.devices.contains_key("1-1")).await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState("1-1".to_string(), DeviceState::Bound))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices.get("1-1").is_some_and(|e| e.state == DeviceState::Bound)
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                "1-1".to_string(),
                DeviceState::Forwarded("s1".to_string()),
            ))
            .unwrap();
        wait_for(&mut rx, |s| {
            s.devices
                .get("1-1")
                .is_some_and(|e| matches!(&e.state, DeviceState::Forwarded(sid) if sid == "s1"))
        })
        .await;

        registry
            .handle()
            .send(RegistryCommand::TransitionState(
                "1-1".to_string(),
                DeviceState::Forwarded("s2".to_string()),
            ))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let snap = rx.borrow();
        let entry = snap.devices.get("1-1").unwrap();
        assert!(
            matches!(&entry.state, DeviceState::Forwarded(sid) if sid == "s1"),
            "Forwarded(s1) -> Forwarded(s2) should be rejected, got {:?}",
            entry.state
        );
    }
}
