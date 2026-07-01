use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::port::host::{UsbHost, UsbHostError};
use crate::port::types::{UsbDevice, UsbEvent};

pub struct WatchEventStream {
    rx: UnboundedReceiver<UsbEvent>,
}

impl Stream for WatchEventStream {
    type Item = UsbEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

#[derive(Clone)]
pub struct MockUsbHost {
    devices: std::sync::Arc<Mutex<Vec<UsbDevice>>>,
    bound: std::sync::Arc<Mutex<HashSet<String>>>,
    tx: UnboundedSender<UsbEvent>,
}

impl MockUsbHost {
    pub fn new(initial_devices: Vec<UsbDevice>) -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        MockUsbHost {
            devices: std::sync::Arc::new(Mutex::new(initial_devices)),
            bound: std::sync::Arc::new(Mutex::new(HashSet::new())),
            tx,
        }
    }

    pub fn inject_event(&self, event: UsbEvent) {
        self.tx.send(event).ok();
    }

    pub fn inject_connect(&self, device: UsbDevice) {
        let busid = device.busid.clone();
        {
            let mut devices = self.devices.lock().unwrap();
            if !devices.iter().any(|d| d.busid == busid) {
                devices.push(device.clone());
            }
        }
        self.inject_event(UsbEvent::Connected { device });
    }

    pub fn inject_disconnect(&self, busid: &str) {
        {
            let mut devices = self.devices.lock().unwrap();
            devices.retain(|d| d.busid != busid);
        }
        self.inject_event(UsbEvent::Disconnected {
            busid: busid.to_string(),
        });
    }
}

#[async_trait]
impl UsbHost for MockUsbHost {
    type Error = UsbHostError;
    type WatchStream = WatchEventStream;

    async fn list(&self) -> Result<Vec<UsbDevice>, Self::Error> {
        let devices = self.devices.lock().unwrap();
        Ok(devices.clone())
    }

    async fn bind(&mut self, busid: &str) -> Result<(), Self::Error> {
        if busid.is_empty() {
            return Err(UsbHostError::DeviceNotFound {
                busid: busid.to_string(),
            });
        }
        let devices = self.devices.lock().unwrap();
        if !devices.iter().any(|d| d.busid == busid) {
            return Err(UsbHostError::DeviceNotFound {
                busid: busid.to_string(),
            });
        }
        drop(devices);

        let mut bound = self.bound.lock().unwrap();
        if bound.contains(busid) {
            return Err(UsbHostError::DeviceInUse {
                busid: busid.to_string(),
            });
        }
        bound.insert(busid.to_string());
        tracing::info!("device bound: {busid}");
        Ok(())
    }

    async fn unbind(&mut self, busid: &str) -> Result<(), Self::Error> {
        if busid.is_empty() {
            return Err(UsbHostError::DeviceNotFound {
                busid: busid.to_string(),
            });
        }
        let mut bound = self.bound.lock().unwrap();
        if !bound.remove(busid) {
            return Err(UsbHostError::DeviceNotFound {
                busid: busid.to_string(),
            });
        }
        tracing::info!("device unbound: {busid}");
        Ok(())
    }

    fn watch(&mut self) -> Self::WatchStream {
        let (tx, rx) = mpsc::unbounded_channel();
        self.tx = tx;
        WatchEventStream { rx }
    }
}
