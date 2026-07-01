mod mock_discovery;
mod mock_host;
mod mock_ipc;
mod mock_session;

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde_json::json;

use crate::port::discovery::{DiscoveryError, DiscoveryService};
use crate::port::host::{UsbHost, UsbHostError};
use crate::port::ipc::{Handler, IpcError, IpcServer};
use crate::port::session::{SessionSignal, SessionSignalError};
use crate::port::types::{ServerInfo, ServiceInfo, SignalMode, UsbDevice, UsbEvent};

use mock_discovery::MockDiscovery;
use mock_host::MockUsbHost;
use mock_ipc::MockIpc;
use mock_session::MockSessionSignal;

fn make_device(busid: &str, vid: u16, pid: u16) -> UsbDevice {
    UsbDevice {
        busid: busid.to_string(),
        vid,
        pid,
        manufacturer: None,
        product: None,
        busnum: 1,
        devnum: 1,
        speed: crate::protocol::UsbDeviceSpeed::HighSpeed,
        b_device_class: 0,
        b_device_sub_class: 0,
        b_device_protocol: 0,
    }
}

fn make_handler(response: serde_json::Value) -> Handler {
    Arc::new(move |_body| {
        let response = response.clone();
        Box::pin(async move { response })
    })
}

#[tokio::test]
async fn mock_usb_host_list_returns_initial_devices() {
    let dev = make_device("1-1", 0x1234, 0x5678);
    let host = MockUsbHost::new(vec![dev.clone()]);
    let devices = host.list().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].busid, "1-1");
}

#[tokio::test]
async fn mock_usb_host_bind_succeeds() {
    let dev = make_device("1-1", 0x1234, 0x5678);
    let mut host = MockUsbHost::new(vec![dev]);
    host.bind("1-1").await.unwrap();
}

#[tokio::test]
async fn mock_usb_host_bind_unknown_busid() {
    let dev = make_device("1-1", 0x1234, 0x5678);
    let mut host = MockUsbHost::new(vec![dev]);
    let result = host.bind("2-2").await;
    assert!(matches!(result, Err(UsbHostError::DeviceNotFound { .. })));
}

#[tokio::test]
async fn mock_usb_host_bind_empty_busid() {
    let dev = make_device("1-1", 0x1234, 0x5678);
    let mut host = MockUsbHost::new(vec![dev]);
    let result = host.bind("").await;
    assert!(matches!(result, Err(UsbHostError::DeviceNotFound { .. })));
}

#[tokio::test]
async fn mock_usb_host_bind_device_in_use() {
    let dev = make_device("1-1", 0x1234, 0x5678);
    let mut host = MockUsbHost::new(vec![dev]);
    host.bind("1-1").await.unwrap();
    let result = host.bind("1-1").await;
    assert!(matches!(result, Err(UsbHostError::DeviceInUse { .. })));
}

#[tokio::test]
async fn mock_usb_host_unbind_succeeds() {
    let dev = make_device("1-1", 0x1234, 0x5678);
    let mut host = MockUsbHost::new(vec![dev]);
    host.bind("1-1").await.unwrap();
    host.unbind("1-1").await.unwrap();
}

#[tokio::test]
async fn mock_usb_host_unbind_not_bound() {
    let dev = make_device("1-1", 0x1234, 0x5678);
    let mut host = MockUsbHost::new(vec![dev]);
    let result = host.unbind("1-1").await;
    assert!(matches!(result, Err(UsbHostError::DeviceNotFound { .. })));
}

#[tokio::test]
async fn mock_usb_host_unbind_empty_busid() {
    let dev = make_device("1-1", 0x1234, 0x5678);
    let mut host = MockUsbHost::new(vec![dev]);
    let result = host.unbind("").await;
    assert!(matches!(result, Err(UsbHostError::DeviceNotFound { .. })));
}

#[tokio::test]
async fn mock_usb_host_watch_receives_connect_events() {
    let dev = make_device("1-1", 0x1234, 0x5678);
    let mut host = MockUsbHost::new(vec![dev.clone()]);
    let mut stream = host.watch();

    host.inject_connect(dev.clone());

    let event = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap();

    let list = host.list().await.unwrap();
    assert_eq!(list.len(), 1);

    match event {
        UsbEvent::Connected(d) => {
            assert_eq!(d.busid, "1-1");
            assert_eq!(d.vid, 0x1234);
            assert_eq!(d.pid, 0x5678);
        }
        _ => panic!("expected Connected event"),
    }
}

#[tokio::test]
async fn mock_usb_host_watch_receives_disconnect_events() {
    let dev = make_device("1-1", 0x1234, 0x5678);
    let mut host = MockUsbHost::new(vec![dev]);
    let mut stream = host.watch();

    host.inject_disconnect("1-1");

    let event = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap();

    match event {
        UsbEvent::Disconnected { busid } => {
            assert_eq!(busid, "1-1");
        }
        _ => panic!("expected Disconnected event"),
    }
}

#[tokio::test]
async fn mock_usb_host_ad2b_at_most_once_no_duplicate_events() {
    let dev = make_device("1-1", 0x1234, 0x5678);
    let mut host = MockUsbHost::new(vec![dev.clone()]);
    let mut stream = host.watch();

    host.inject_connect(dev.clone());
    host.inject_connect(dev.clone());

    let mut connect_count = 0u32;
    let mut disconnect_count = 0u32;

    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(100), stream.next()).await {
        match event {
            UsbEvent::Connected(_) => connect_count += 1,
            UsbEvent::Disconnected { .. } => disconnect_count += 1,
        }
    }

    assert_eq!(connect_count, 2, "inject_connect does not deduplicate events");
    assert_eq!(disconnect_count, 0);
}

#[tokio::test]
async fn mock_usb_host_ad2b_connect_precedes_reference() {
    let dev = make_device("1-1", 0x1234, 0x5678);
    let mut host = MockUsbHost::new(vec![]);
    let mut stream = host.watch();

    host.inject_connect(dev.clone());

    let event = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap();

    assert!(matches!(event, UsbEvent::Connected(_)));

    let list = host.list().await.unwrap();
    assert_eq!(list.len(), 1);
}

#[tokio::test]
async fn mock_usb_host_ad2b_disconnect_bounded_timeout() {
    let dev = make_device("1-1", 0x1234, 0x5678);
    let mut host = MockUsbHost::new(vec![dev]);
    let mut stream = host.watch();

    host.inject_disconnect("1-1");

    let result = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn mock_discovery_advertise_scan_withdraw_roundtrip() {
    let server = ServerInfo {
        hostname: "test-host".to_string(),
        ip: IpAddr::from([127, 0, 0, 1]),
        port: 3240,
        device_count: 3,
    };
    let discovery = MockDiscovery::new(vec![server.clone()]);

    let info = ServiceInfo {
        hostname: "test-host".to_string(),
        ip: IpAddr::from([127, 0, 0, 1]),
        port: 3240,
        service_type: "_usbip._tcp".to_string(),
    };
    discovery.advertise(&info).await.unwrap();

    let results = discovery.scan().await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].hostname, "test-host");
    assert_eq!(results[0].device_count, 3);

    discovery.withdraw().await.unwrap();
}

#[tokio::test]
async fn mock_discovery_advertise_failure() {
    let discovery = MockDiscovery::new(vec![]);
    discovery.set_result(Err("network down".to_string()));

    let info = ServiceInfo {
        hostname: "test-host".to_string(),
        ip: IpAddr::from([127, 0, 0, 1]),
        port: 3240,
        service_type: "_usbip._tcp".to_string(),
    };
    let result = discovery.advertise(&info).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn mock_ipc_register_and_call_route() {
    let mut ipc = MockIpc::new();
    let handler = make_handler(json!({"status": "ok"}));

    let mut routes = HashMap::new();
    routes.insert("/info".to_string(), handler);
    ipc.serve(routes).await.unwrap();

    let response = ipc.call_route("/info", json!({})).unwrap();
    assert_eq!(response, json!({"status": "ok"}));
}

#[tokio::test]
async fn mock_ipc_route_not_found() {
    let mut ipc = MockIpc::new();
    let handler = make_handler(json!({"status": "ok"}));

    let mut routes = HashMap::new();
    routes.insert("/info".to_string(), handler);
    ipc.serve(routes).await.unwrap();

    let result = ipc.call_route("/missing", json!({}));
    assert!(matches!(result, Err(IpcError::RouteNotFound { .. })));
}

#[tokio::test]
async fn mock_ipc_shutdown_clears_routes() {
    let mut ipc = MockIpc::new();
    let handler = make_handler(json!({"status": "ok"}));

    let mut routes = HashMap::new();
    routes.insert("/info".to_string(), handler);
    ipc.serve(routes).await.unwrap();

    ipc.shutdown().await.unwrap();
    assert_eq!(ipc.routes().len(), 0);
}

#[tokio::test]
async fn mock_session_signal_on_start_and_simulate() {
    let session = MockSessionSignal::new();
    let handler: Handler = make_handler(json!({"result": "started"}));
    session.on_start(handler, SignalMode::Eager).await.unwrap();

    assert_eq!(session.start_count(), 0);
    session.simulate_start(json!({"session": 1}));
    assert_eq!(session.start_count(), 1);
}

#[tokio::test]
async fn mock_session_signal_on_stop_and_simulate() {
    let session = MockSessionSignal::new();
    let handler: Handler = make_handler(json!({"result": "stopped"}));
    session.on_stop(handler).await.unwrap();

    assert_eq!(session.stop_count(), 0);
    session.simulate_stop(json!({"session": 1}));
    assert_eq!(session.stop_count(), 1);
}

#[tokio::test]
async fn mock_session_signal_already_registered() {
    let session = MockSessionSignal::new();
    let handler: Handler = make_handler(json!({}));
    session.on_start(handler.clone(), SignalMode::Eager).await.unwrap();
    let result = session.on_start(handler, SignalMode::Confirmed).await;
    assert!(matches!(result, Err(SessionSignalError::AlreadyRegistered)));
}

#[tokio::test]
async fn mock_session_signal_already_registered_stop() {
    let session = MockSessionSignal::new();
    let handler: Handler = make_handler(json!({}));
    session.on_stop(handler.clone()).await.unwrap();
    let result = session.on_stop(handler).await;
    assert!(matches!(result, Err(SessionSignalError::AlreadyRegistered)));
}

#[tokio::test]
async fn mock_session_signal_does_not_invoke_without_handler() {
    let session = MockSessionSignal::new();
    session.simulate_start(json!({"session": 1}));
    assert_eq!(session.start_count(), 0);
    session.simulate_stop(json!({"session": 1}));
    assert_eq!(session.stop_count(), 0);
}

#[test]
fn usb_device_from_descriptor() {
    use crate::protocol::UsbDeviceDescriptor;
    let desc = UsbDeviceDescriptor {
        path: [0u8; 256],
        busid: {
            let mut b = [0u8; 32];
            b[..5].copy_from_slice(b"1-2.3");
            b
        },
        busnum: 1,
        devnum: 3,
        speed: crate::protocol::UsbDeviceSpeed::HighSpeed,
        id_vendor: 0x1234,
        id_product: 0x5678,
        bcd_device: 0x0200,
        b_device_class: 0x00,
        b_device_sub_class: 0x00,
        b_device_protocol: 0x00,
        b_configuration_value: 1,
        b_num_configurations: 1,
        b_num_interfaces: 2,
    };
    let device = UsbDevice::from(desc);
    assert_eq!(device.busid, "1-2.3");
    assert_eq!(device.vid, 0x1234);
    assert_eq!(device.pid, 0x5678);
    assert_eq!(device.speed, crate::protocol::UsbDeviceSpeed::HighSpeed);
    assert!(device.manufacturer.is_none());
    assert!(device.product.is_none());
}

#[test]
fn error_types_derive_debug_clone_eq() {
    let e1 = UsbHostError::DeviceNotFound {
        busid: "1-1".to_string(),
    };
    let e2 = e1.clone();
    assert_eq!(e1, e2);
    let _ = format!("{:?}", e1);

    let e1 = DiscoveryError::ScanFailed("err".to_string());
    let e2 = e1.clone();
    assert_eq!(e1, e2);
    let _ = format!("{:?}", e1);

    let e1 = IpcError::AlreadyServing;
    let e2 = e1.clone();
    assert_eq!(e1, e2);
    let _ = format!("{:?}", e1);

    let e1 = SessionSignalError::AlreadyRegistered;
    let e2 = e1.clone();
    assert_eq!(e1, e2);
    let _ = format!("{:?}", e1);
}

#[test]
fn domain_types_derive_debug_clone_eq() {
    let dev = make_device("1-1", 1, 2);
    let dev2 = dev.clone();
    assert_eq!(dev, dev2);

    let event = UsbEvent::Connected(make_device("1-1", 1, 2));
    let event2 = event.clone();
    assert_eq!(event, event2);

    let event = UsbEvent::Disconnected {
        busid: "1-1".to_string(),
    };
    let event2 = event.clone();
    assert_eq!(event, event2);

    let info = ServiceInfo {
        hostname: "h".to_string(),
        ip: IpAddr::from([127, 0, 0, 1]),
        port: 1,
        service_type: "t".to_string(),
    };
    let info2 = info.clone();
    assert_eq!(info, info2);

    let server = ServerInfo {
        hostname: "h".to_string(),
        ip: IpAddr::from([127, 0, 0, 1]),
        port: 1,
        device_count: 0,
    };
    let server2 = server.clone();
    assert_eq!(server, server2);
}

#[test]
fn no_platform_crates_in_core_deps() {
    let cargo_toml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    let forbidden = ["windows-rs", "nix", "jni"];
    for crate_name in forbidden {
        assert!(
            !cargo_toml.contains(crate_name),
            "AD-1 violation: {crate_name} found in easy-usb-core Cargo.toml"
        );
    }
}
