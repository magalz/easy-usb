use std::net::IpAddr;

use crate::protocol::UsbDeviceDescriptor;
use crate::protocol::UsbDeviceSpeed;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignalMode {
    Eager,
    Confirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UsbEvent {
    Connected { device: UsbDevice },
    Disconnected { busid: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UsbDevice {
    pub busid: String,
    pub vid: u16,
    pub pid: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub busnum: u32,
    pub devnum: u32,
    pub speed: UsbDeviceSpeed,
    pub b_device_class: u8,
    pub b_device_sub_class: u8,
    pub b_device_protocol: u8,
}

impl From<UsbDeviceDescriptor> for UsbDevice {
    fn from(d: UsbDeviceDescriptor) -> Self {
        let busid = String::from_utf8_lossy(&d.busid).trim_end_matches('\0').to_string();
        UsbDevice {
            busid,
            vid: d.id_vendor,
            pid: d.id_product,
            manufacturer: None,
            product: None,
            busnum: d.busnum,
            devnum: d.devnum,
            speed: d.speed,
            b_device_class: d.b_device_class,
            b_device_sub_class: d.b_device_sub_class,
            b_device_protocol: d.b_device_protocol,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInfo {
    pub hostname: String,
    pub ip: IpAddr,
    pub port: u16,
    pub service_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub hostname: String,
    pub ip: IpAddr,
    pub port: u16,
    pub device_count: u32,
}
