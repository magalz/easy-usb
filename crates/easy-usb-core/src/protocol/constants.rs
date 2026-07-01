pub const USBIP_VERSION: u16 = 0x0111;

pub const OP_REQ_DEVLIST: u32 = 0x8005;
pub const OP_REP_DEVLIST: u32 = 0x0005;
pub const OP_REQ_IMPORT: u32 = 0x8003;
pub const OP_REP_IMPORT: u32 = 0x0003;
pub const OP_REQ_EXPORT: u32 = 0x8006;
pub const OP_REP_EXPORT: u32 = 0x0006;
pub const CMD_SUBMIT: u32 = 0x0001;
pub const RET_SUBMIT: u32 = 0x0003;
pub const CMD_UNLINK: u32 = 0x0002;
pub const RET_UNLINK: u32 = 0x0004;

pub const USBIP_DIR_OUT: u32 = 0x00;
pub const USBIP_DIR_IN: u32 = 0x01;

pub const USBIP_URB_SHORT_NOT_OK: u32 = 0x0001;
pub const USBIP_URB_ISO_ASAP: u32 = 0x0002;
pub const USBIP_URB_NO_TRANSFER_DMA_MAP: u32 = 0x0004;
pub const USBIP_URB_ZERO_PACKET: u32 = 0x0008;
pub const USBIP_URB_NO_INTERRUPT: u32 = 0x0010;
pub const USBIP_URB_FREE_BUFFER: u32 = 0x0020;
pub const USBIP_URB_DIR_IN: u32 = 0x0040;
pub const USBIP_URB_DIR_OUT: u32 = 0x0080;
