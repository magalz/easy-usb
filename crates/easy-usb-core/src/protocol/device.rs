#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum UsbDeviceSpeed {
    LowSpeed = 1,
    FullSpeed = 2,
    HighSpeed = 3,
    SuperSpeed = 4,
}

impl TryFrom<u32> for UsbDeviceSpeed {
    type Error = crate::protocol::codec::ProtocolError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(UsbDeviceSpeed::LowSpeed),
            2 => Ok(UsbDeviceSpeed::FullSpeed),
            3 => Ok(UsbDeviceSpeed::HighSpeed),
            4 => Ok(UsbDeviceSpeed::SuperSpeed),
            _ => Err(crate::protocol::codec::ProtocolError::InvalidDeviceSpeed { value }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDeviceDescriptor {
    pub path: [u8; 256],
    pub busid: [u8; 32],
    pub busnum: u32,
    pub devnum: u32,
    pub speed: UsbDeviceSpeed,
    pub id_vendor: u16,
    pub id_product: u16,
    pub bcd_device: u16,
    pub b_device_class: u8,
    pub b_device_sub_class: u8,
    pub b_device_protocol: u8,
    pub b_configuration_value: u8,
    pub b_num_configurations: u8,
    pub b_num_interfaces: u8,
}

impl From<crate::protocol::wire::OpRepImport> for UsbDeviceDescriptor {
    fn from(rep: crate::protocol::wire::OpRepImport) -> Self {
        let speed = match UsbDeviceSpeed::try_from(rep.speed) {
            Ok(s) => s,
            Err(_) => {
                tracing::warn!(
                    "unknown USB device speed value {}, falling back to HighSpeed",
                    rep.speed
                );
                UsbDeviceSpeed::HighSpeed
            }
        };
        UsbDeviceDescriptor {
            path: rep.path,
            busid: rep.busid,
            busnum: rep.busnum,
            devnum: rep.devnum,
            speed,
            id_vendor: rep.id_vendor,
            id_product: rep.id_product,
            bcd_device: rep.bcd_device,
            b_device_class: rep.b_device_class,
            b_device_sub_class: rep.b_device_sub_class,
            b_device_protocol: rep.b_device_protocol,
            b_configuration_value: rep.b_configuration_value,
            b_num_configurations: rep.b_num_configurations,
            b_num_interfaces: rep.b_num_interfaces,
        }
    }
}
