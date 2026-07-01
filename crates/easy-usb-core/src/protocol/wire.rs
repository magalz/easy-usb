use crate::protocol::constants;
use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct UsbipHeaderBasic {
    pub command: u32,
    pub seqnum: u32,
    pub devid: u32,
    pub direction: u32,
    pub ep: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct UsbipHeaderCmdSubmit {
    pub transfer_flags: u32,
    pub transfer_buffer_length: i32,
    pub start_frame: i32,
    pub number_of_packets: i32,
    pub interval: i32,
    pub setup: [u8; 8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct UsbipHeaderRetSubmit {
    pub status: i32,
    pub actual_length: i32,
    pub start_frame: i32,
    pub number_of_packets: i32,
    pub error_count: i32,
    pub setup: [u8; 8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct UsbipHeaderCmdUnlink {
    pub seqnum: u32,
    pub __padding: [u32; 6],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct UsbipHeaderRetUnlink {
    pub status: i32,
    pub __padding: [u32; 6],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct OpReqImport {
    pub status: u32,
    pub path: [u8; 256],
    pub busid: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct OpRepImport {
    pub status: u32,
    pub path: [u8; 256],
    pub busid: [u8; 32],
    pub busnum: u32,
    pub devnum: u32,
    pub speed: u32,
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

#[derive(Clone, Copy)]
#[repr(C)]
pub struct UsbipHeader {
    pub base: UsbipHeaderBasic,
    pub u: UsbipHeaderUnion,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub union UsbipHeaderUnion {
    pub cmd_submit: UsbipHeaderCmdSubmit,
    pub ret_submit: UsbipHeaderRetSubmit,
    pub cmd_unlink: UsbipHeaderCmdUnlink,
    pub ret_unlink: UsbipHeaderRetUnlink,
}

impl UsbipHeader {
    pub fn cmd_submit(&self) -> Option<&UsbipHeaderCmdSubmit> {
        if self.base.command == constants::CMD_SUBMIT {
            Some(unsafe { &self.u.cmd_submit })
        } else {
            None
        }
    }

    pub fn ret_submit(&self) -> Option<&UsbipHeaderRetSubmit> {
        if self.base.command == constants::RET_SUBMIT {
            Some(unsafe { &self.u.ret_submit })
        } else {
            None
        }
    }

    pub fn cmd_unlink(&self) -> Option<&UsbipHeaderCmdUnlink> {
        if self.base.command == constants::CMD_UNLINK {
            Some(unsafe { &self.u.cmd_unlink })
        } else {
            None
        }
    }

    pub fn ret_unlink(&self) -> Option<&UsbipHeaderRetUnlink> {
        if self.base.command == constants::RET_UNLINK {
            Some(unsafe { &self.u.ret_unlink })
        } else {
            None
        }
    }
}

impl fmt::Debug for UsbipHeaderUnion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsbipHeaderUnion").finish_non_exhaustive()
    }
}

impl fmt::Debug for UsbipHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsbipHeader")
            .field("base", &self.base)
            .field("u", &self.u)
            .finish()
    }
}

impl PartialEq for UsbipHeaderUnion {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for UsbipHeaderUnion {}

impl PartialEq for UsbipHeader {
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base
    }
}

impl Eq for UsbipHeader {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSpecific {
    CmdSubmit(UsbipHeaderCmdSubmit),
    RetSubmit(UsbipHeaderRetSubmit),
    CmdUnlink(UsbipHeaderCmdUnlink),
    RetUnlink(UsbipHeaderRetUnlink),
}

impl CommandSpecific {
    pub fn from_command(command: u32) -> Option<Self> {
        match command {
            constants::CMD_SUBMIT => Some(CommandSpecific::CmdSubmit(UsbipHeaderCmdSubmit {
                transfer_flags: 0,
                transfer_buffer_length: 0,
                start_frame: 0,
                number_of_packets: 0,
                interval: 0,
                setup: [0u8; 8],
            })),
            constants::RET_SUBMIT => Some(CommandSpecific::RetSubmit(UsbipHeaderRetSubmit {
                status: 0,
                actual_length: 0,
                start_frame: 0,
                number_of_packets: 0,
                error_count: 0,
                setup: [0u8; 8],
            })),
            constants::CMD_UNLINK => Some(CommandSpecific::CmdUnlink(UsbipHeaderCmdUnlink {
                seqnum: 0,
                __padding: [0u32; 6],
            })),
            constants::RET_UNLINK => Some(CommandSpecific::RetUnlink(UsbipHeaderRetUnlink {
                status: 0,
                __padding: [0u32; 6],
            })),
            _ => None,
        }
    }
}

pub fn header_command(basic: &UsbipHeaderBasic) -> u32 {
    basic.command
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn size_assertions() {
        assert_eq!(size_of::<UsbipHeaderBasic>(), 20, "UsbipHeaderBasic size mismatch");
        assert_eq!(
            size_of::<UsbipHeaderCmdSubmit>(),
            28,
            "UsbipHeaderCmdSubmit size mismatch"
        );
        assert_eq!(
            size_of::<UsbipHeaderRetSubmit>(),
            28,
            "UsbipHeaderRetSubmit size mismatch"
        );
        assert_eq!(
            size_of::<UsbipHeaderCmdUnlink>(),
            28,
            "UsbipHeaderCmdUnlink size mismatch"
        );
        assert_eq!(
            size_of::<UsbipHeaderRetUnlink>(),
            28,
            "UsbipHeaderRetUnlink size mismatch"
        );
        assert_eq!(size_of::<UsbipHeader>(), 48, "UsbipHeader size mismatch");
        assert_eq!(size_of::<OpReqImport>(), 292, "OpReqImport size mismatch");
        assert_eq!(size_of::<OpRepImport>(), 316, "OpRepImport size mismatch");
    }

    #[test]
    fn constants_match_opcodes() {
        assert_eq!(constants::USBIP_VERSION, 0x0111);
        assert_eq!(constants::OP_REQ_IMPORT, 0x8003u32);
        assert_eq!(constants::OP_REP_IMPORT, 0x0003u32);
        assert_eq!(constants::CMD_SUBMIT, 0x0001u32);
        assert_eq!(constants::CMD_UNLINK, 0x0002u32);
        assert_eq!(constants::RET_SUBMIT, 0x0003u32);
        assert_eq!(constants::RET_UNLINK, 0x0004u32);
    }

    #[test]
    fn safe_accessors_return_none_for_wrong_command() {
        use crate::protocol::wire::UsbipHeader as H;
        use crate::protocol::wire::UsbipHeaderBasic as B;
        use crate::protocol::wire::UsbipHeaderCmdSubmit as Cmd;
        use crate::protocol::wire::UsbipHeaderUnion as U;

        let header = H {
            base: B {
                command: constants::CMD_SUBMIT,
                seqnum: 0,
                devid: 0,
                direction: constants::USBIP_DIR_IN,
                ep: 0x81,
            },
            u: U {
                cmd_submit: Cmd {
                    transfer_flags: 0,
                    transfer_buffer_length: 0,
                    start_frame: 0,
                    number_of_packets: 0,
                    interval: 0,
                    setup: [0u8; 8],
                },
            },
        };

        assert!(header.cmd_submit().is_some());
        assert!(header.ret_submit().is_none());
        assert!(header.cmd_unlink().is_none());
        assert!(header.ret_unlink().is_none());
    }

    #[test]
    fn safe_accessor_matches_all_commands() {
        use crate::protocol::wire::UsbipHeader as H;
        use crate::protocol::wire::UsbipHeaderBasic as B;
        use crate::protocol::wire::UsbipHeaderUnion as U;

        let test_cases = [
            (constants::CMD_SUBMIT, "cmd_submit"),
            (constants::RET_SUBMIT, "ret_submit"),
            (constants::CMD_UNLINK, "cmd_unlink"),
            (constants::RET_UNLINK, "ret_unlink"),
        ];

        for (command, name) in test_cases {
            let header = H {
                base: B {
                    command,
                    seqnum: 0,
                    devid: 0,
                    direction: 0,
                    ep: 0,
                },
                u: U {
                    cmd_submit: unsafe { std::mem::zeroed() },
                },
            };

            match name {
                "cmd_submit" => assert!(header.cmd_submit().is_some(), "cmd_submit for CMD_SUBMIT"),
                "ret_submit" => assert!(header.ret_submit().is_some(), "ret_submit for RET_SUBMIT"),
                "cmd_unlink" => assert!(header.cmd_unlink().is_some(), "cmd_unlink for CMD_UNLINK"),
                "ret_unlink" => assert!(header.ret_unlink().is_some(), "ret_unlink for RET_UNLINK"),
                _ => unreachable!(),
            }
        }
    }
}
