use core::mem::size_of;

use byteorder::{NetworkEndian, ReadBytesExt};
use bytes::{BufMut, BytesMut};
use thiserror::Error;

use crate::protocol::constants;
use crate::protocol::wire::{
    UsbipHeader, UsbipHeaderBasic, UsbipHeaderCmdSubmit, UsbipHeaderCmdUnlink, UsbipHeaderRetSubmit,
    UsbipHeaderRetUnlink, UsbipHeaderUnion,
};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("incomplete buffer: need {needed} bytes, have {available}")]
    IncompleteBuffer { needed: usize, available: usize },

    #[error("invalid command: 0x{command:08x}")]
    InvalidCommand { command: u32 },

    #[error("invalid device speed: {value}")]
    InvalidDeviceSpeed { value: u32 },

    #[error("buffer too large: {size} bytes exceeds maximum {max}")]
    BufferTooLarge { size: usize, max: usize },

    #[error("isochronous transfers not supported in v1")]
    IsochronousNotSupported,

    #[error("encoding error: {0}")]
    EncodingError(String),

    #[error("I/O error: {0}")]
    IoError(String),
}

pub(crate) const MAX_PAYLOAD_SIZE: usize = 1_048_576;

const BASIC_HEADER_SIZE: usize = size_of::<UsbipHeaderBasic>();
const CMD_HEADER_SIZE: usize = size_of::<UsbipHeaderCmdSubmit>();
const PAYLOAD_OFFSET: usize = BASIC_HEADER_SIZE + CMD_HEADER_SIZE;

pub fn encode_header(header: &UsbipHeader, payload: Option<&[u8]>) -> Result<BytesMut, ProtocolError> {
    let mut buf = BytesMut::new();

    encode_basic(header.base, &mut buf)?;

    match header.base.command {
        constants::CMD_SUBMIT => {
            // SAFETY: command is CMD_SUBMIT, so union holds cmd_submit variant
            let cmd = unsafe { header.u.cmd_submit };
            encode_cmd_submit(&cmd, &mut buf)?;
            if header.base.direction == constants::USBIP_DIR_OUT && cmd.transfer_buffer_length > 0 {
                let payload = payload.ok_or_else(|| {
                    ProtocolError::EncodingError("CMD_SUBMIT with transfer_buffer_length > 0 requires payload".into())
                })?;
                if payload.len() > MAX_PAYLOAD_SIZE {
                    return Err(ProtocolError::BufferTooLarge {
                        size: payload.len(),
                        max: MAX_PAYLOAD_SIZE,
                    });
                }
                buf.put_slice(payload);
            }
        }
        constants::RET_SUBMIT => {
            // SAFETY: command is RET_SUBMIT, so union holds ret_submit variant
            let ret = unsafe { header.u.ret_submit };
            encode_ret_submit(&ret, &mut buf)?;
            if ret.actual_length > 0 {
                let payload = payload.ok_or_else(|| {
                    ProtocolError::EncodingError("RET_SUBMIT with actual_length > 0 requires payload".into())
                })?;
                if payload.len() > MAX_PAYLOAD_SIZE {
                    return Err(ProtocolError::BufferTooLarge {
                        size: payload.len(),
                        max: MAX_PAYLOAD_SIZE,
                    });
                }
                buf.put_slice(payload);
            }
        }
        constants::CMD_UNLINK => {
            // SAFETY: command is CMD_UNLINK, so union holds cmd_unlink variant
            let cmd = unsafe { header.u.cmd_unlink };
            encode_cmd_unlink(&cmd, &mut buf)?;
        }
        constants::RET_UNLINK => {
            // SAFETY: command is RET_UNLINK, so union holds ret_unlink variant
            let ret = unsafe { header.u.ret_unlink };
            encode_ret_unlink(&ret, &mut buf)?;
        }
        _ => {
            return Err(ProtocolError::InvalidCommand {
                command: header.base.command,
            });
        }
    }

    Ok(buf)
}

pub fn decode_header(buf: &[u8]) -> Result<(UsbipHeader, Option<Vec<u8>>), ProtocolError> {
    let basic = decode_basic(buf)?;

    if buf.len() < PAYLOAD_OFFSET {
        return Err(ProtocolError::IncompleteBuffer {
            needed: PAYLOAD_OFFSET,
            available: buf.len(),
        });
    }

    let header: UsbipHeader;
    let mut payload: Option<Vec<u8>> = None;
    let cmd_start = BASIC_HEADER_SIZE;
    let cmd_end = PAYLOAD_OFFSET;

    match basic.command {
        constants::CMD_SUBMIT => {
            let cmd = decode_cmd_submit(&buf[cmd_start..cmd_end])?;
            if cmd.transfer_buffer_length > 0 && basic.direction == constants::USBIP_DIR_OUT {
                let payload_len = cmd.transfer_buffer_length as usize;
                if buf.len() < PAYLOAD_OFFSET + payload_len {
                    return Err(ProtocolError::IncompleteBuffer {
                        needed: PAYLOAD_OFFSET + payload_len,
                        available: buf.len(),
                    });
                }
                if payload_len > MAX_PAYLOAD_SIZE {
                    return Err(ProtocolError::BufferTooLarge {
                        size: payload_len,
                        max: MAX_PAYLOAD_SIZE,
                    });
                }
                payload = Some(buf[PAYLOAD_OFFSET..PAYLOAD_OFFSET + payload_len].to_vec());
            }
            header = UsbipHeader {
                base: basic,
                u: UsbipHeaderUnion { cmd_submit: cmd },
            };
        }
        constants::RET_SUBMIT => {
            let ret = decode_ret_submit(&buf[cmd_start..cmd_end])?;
            if ret.actual_length > 0 {
                let payload_len = ret.actual_length as usize;
                if buf.len() < PAYLOAD_OFFSET + payload_len {
                    return Err(ProtocolError::IncompleteBuffer {
                        needed: PAYLOAD_OFFSET + payload_len,
                        available: buf.len(),
                    });
                }
                if payload_len > MAX_PAYLOAD_SIZE {
                    return Err(ProtocolError::BufferTooLarge {
                        size: payload_len,
                        max: MAX_PAYLOAD_SIZE,
                    });
                }
                payload = Some(buf[PAYLOAD_OFFSET..PAYLOAD_OFFSET + payload_len].to_vec());
            }
            header = UsbipHeader {
                base: basic,
                u: UsbipHeaderUnion { ret_submit: ret },
            };
        }
        constants::CMD_UNLINK => {
            let cmd = decode_cmd_unlink(&buf[cmd_start..cmd_end])?;
            header = UsbipHeader {
                base: basic,
                u: UsbipHeaderUnion { cmd_unlink: cmd },
            };
        }
        constants::RET_UNLINK => {
            let ret = decode_ret_unlink(&buf[cmd_start..cmd_end])?;
            header = UsbipHeader {
                base: basic,
                u: UsbipHeaderUnion { ret_unlink: ret },
            };
        }
        _ => {
            return Err(ProtocolError::InvalidCommand { command: basic.command });
        }
    }

    Ok((header, payload))
}

pub fn encode_op_req_import(req: &crate::protocol::wire::OpReqImport) -> Result<BytesMut, ProtocolError> {
    let capacity = size_of::<crate::protocol::wire::OpReqImport>();
    let mut buf = BytesMut::with_capacity(capacity);
    buf.put_u32(req.status);
    buf.put_slice(&req.path);
    buf.put_slice(&req.busid);
    Ok(buf)
}

pub fn decode_op_req_import(buf: &[u8]) -> Result<crate::protocol::wire::OpReqImport, ProtocolError> {
    let expected = size_of::<crate::protocol::wire::OpReqImport>();
    if buf.len() < expected {
        return Err(ProtocolError::IncompleteBuffer {
            needed: expected,
            available: buf.len(),
        });
    }
    let mut offset = 0;
    let status = (&buf[offset..offset + 4])
        .read_u32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read status: {e}")))?;
    offset += 4;
    let mut path = [0u8; 256];
    path.copy_from_slice(&buf[offset..offset + 256]);
    offset += 256;
    let mut busid = [0u8; 32];
    busid.copy_from_slice(&buf[offset..offset + 32]);

    Ok(crate::protocol::wire::OpReqImport { status, path, busid })
}

pub fn encode_op_rep_import(rep: &crate::protocol::wire::OpRepImport) -> Result<BytesMut, ProtocolError> {
    let capacity = size_of::<crate::protocol::wire::OpRepImport>();
    let mut buf = BytesMut::with_capacity(capacity);
    buf.put_u32(rep.status);
    buf.put_slice(&rep.path);
    buf.put_slice(&rep.busid);
    buf.put_u32(rep.busnum);
    buf.put_u32(rep.devnum);
    buf.put_u32(rep.speed);
    buf.put_u16(rep.id_vendor);
    buf.put_u16(rep.id_product);
    buf.put_u16(rep.bcd_device);
    buf.put_u8(rep.b_device_class);
    buf.put_u8(rep.b_device_sub_class);
    buf.put_u8(rep.b_device_protocol);
    buf.put_u8(rep.b_configuration_value);
    buf.put_u8(rep.b_num_configurations);
    buf.put_u8(rep.b_num_interfaces);
    Ok(buf)
}

pub fn decode_op_rep_import(buf: &[u8]) -> Result<crate::protocol::wire::OpRepImport, ProtocolError> {
    let expected = size_of::<crate::protocol::wire::OpRepImport>();
    if buf.len() < expected {
        return Err(ProtocolError::IncompleteBuffer {
            needed: expected,
            available: buf.len(),
        });
    }
    let mut offset = 0;
    let status = (&buf[offset..offset + 4])
        .read_u32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read status: {e}")))?;
    offset += 4;
    let mut path = [0u8; 256];
    path.copy_from_slice(&buf[offset..offset + 256]);
    offset += 256;
    let mut busid = [0u8; 32];
    busid.copy_from_slice(&buf[offset..offset + 32]);
    offset += 32;
    let busnum = (&buf[offset..offset + 4])
        .read_u32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read busnum: {e}")))?;
    offset += 4;
    let devnum = (&buf[offset..offset + 4])
        .read_u32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read devnum: {e}")))?;
    offset += 4;
    let speed = (&buf[offset..offset + 4])
        .read_u32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read speed: {e}")))?;
    offset += 4;
    let id_vendor = (&buf[offset..offset + 2])
        .read_u16::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read id_vendor: {e}")))?;
    offset += 2;
    let id_product = (&buf[offset..offset + 2])
        .read_u16::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read id_product: {e}")))?;
    offset += 2;
    let bcd_device = (&buf[offset..offset + 2])
        .read_u16::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read bcd_device: {e}")))?;
    offset += 2;
    let b_device_class = buf[offset];
    offset += 1;
    let b_device_sub_class = buf[offset];
    offset += 1;
    let b_device_protocol = buf[offset];
    offset += 1;
    let b_configuration_value = buf[offset];
    offset += 1;
    let b_num_configurations = buf[offset];
    offset += 1;
    let b_num_interfaces = buf[offset];

    Ok(crate::protocol::wire::OpRepImport {
        status,
        path,
        busid,
        busnum,
        devnum,
        speed,
        id_vendor,
        id_product,
        bcd_device,
        b_device_class,
        b_device_sub_class,
        b_device_protocol,
        b_configuration_value,
        b_num_configurations,
        b_num_interfaces,
    })
}

fn encode_basic(basic: UsbipHeaderBasic, buf: &mut BytesMut) -> Result<(), ProtocolError> {
    if basic.direction != constants::USBIP_DIR_OUT && basic.direction != constants::USBIP_DIR_IN {
        return Err(ProtocolError::EncodingError(format!(
            "invalid direction value: {}",
            basic.direction
        )));
    }
    buf.put_u32(basic.command);
    buf.put_u32(basic.seqnum);
    buf.put_u32(basic.devid);
    buf.put_u32(basic.direction);
    buf.put_u32(basic.ep);
    Ok(())
}

fn decode_basic(buf: &[u8]) -> Result<UsbipHeaderBasic, ProtocolError> {
    if buf.len() < BASIC_HEADER_SIZE {
        return Err(ProtocolError::IncompleteBuffer {
            needed: BASIC_HEADER_SIZE,
            available: buf.len(),
        });
    }
    let mut slice = &buf[0..BASIC_HEADER_SIZE];
    let command = slice
        .read_u32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read command: {e}")))?;
    let seqnum = slice
        .read_u32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read seqnum: {e}")))?;
    let devid = slice
        .read_u32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read devid: {e}")))?;
    let direction = slice
        .read_u32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read direction: {e}")))?;
    let ep = slice
        .read_u32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read ep: {e}")))?;

    if direction != constants::USBIP_DIR_OUT && direction != constants::USBIP_DIR_IN {
        return Err(ProtocolError::EncodingError(format!(
            "invalid direction value: {direction}"
        )));
    }

    Ok(UsbipHeaderBasic {
        command,
        seqnum,
        devid,
        direction,
        ep,
    })
}

fn encode_cmd_submit(cmd: &UsbipHeaderCmdSubmit, buf: &mut BytesMut) -> Result<(), ProtocolError> {
    if cmd.number_of_packets != 0 {
        return Err(ProtocolError::IsochronousNotSupported);
    }
    buf.put_u32(cmd.transfer_flags);
    buf.put_i32(cmd.transfer_buffer_length);
    buf.put_i32(cmd.start_frame);
    buf.put_i32(cmd.number_of_packets);
    buf.put_i32(cmd.interval);
    buf.put_slice(&cmd.setup);
    Ok(())
}

fn decode_cmd_submit(buf: &[u8]) -> Result<UsbipHeaderCmdSubmit, ProtocolError> {
    if buf.len() < CMD_HEADER_SIZE {
        return Err(ProtocolError::IncompleteBuffer {
            needed: CMD_HEADER_SIZE,
            available: buf.len(),
        });
    }
    let mut slice = &buf[0..CMD_HEADER_SIZE];
    let transfer_flags = slice
        .read_u32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read transfer_flags: {e}")))?;
    let transfer_buffer_length = slice
        .read_i32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read transfer_buffer_length: {e}")))?;
    let start_frame = slice
        .read_i32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read start_frame: {e}")))?;
    let number_of_packets = slice
        .read_i32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read number_of_packets: {e}")))?;
    let interval = slice
        .read_i32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read interval: {e}")))?;
    let mut setup = [0u8; 8];
    setup.copy_from_slice(&slice[..8]);
    Ok(UsbipHeaderCmdSubmit {
        transfer_flags,
        transfer_buffer_length,
        start_frame,
        number_of_packets,
        interval,
        setup,
    })
}

fn encode_ret_submit(ret: &UsbipHeaderRetSubmit, buf: &mut BytesMut) -> Result<(), ProtocolError> {
    if ret.number_of_packets != 0 {
        return Err(ProtocolError::IsochronousNotSupported);
    }
    buf.put_i32(ret.status);
    buf.put_i32(ret.actual_length);
    buf.put_i32(ret.start_frame);
    buf.put_i32(ret.number_of_packets);
    buf.put_i32(ret.error_count);
    buf.put_slice(&ret.setup);
    Ok(())
}

fn decode_ret_submit(buf: &[u8]) -> Result<UsbipHeaderRetSubmit, ProtocolError> {
    if buf.len() < CMD_HEADER_SIZE {
        return Err(ProtocolError::IncompleteBuffer {
            needed: CMD_HEADER_SIZE,
            available: buf.len(),
        });
    }
    let mut slice = &buf[0..CMD_HEADER_SIZE];
    let status = slice
        .read_i32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read status: {e}")))?;
    let actual_length = slice
        .read_i32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read actual_length: {e}")))?;
    let start_frame = slice
        .read_i32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read start_frame: {e}")))?;
    let number_of_packets = slice
        .read_i32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read number_of_packets: {e}")))?;
    let error_count = slice
        .read_i32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read error_count: {e}")))?;
    let mut setup = [0u8; 8];
    setup.copy_from_slice(&slice[..8]);
    Ok(UsbipHeaderRetSubmit {
        status,
        actual_length,
        start_frame,
        number_of_packets,
        error_count,
        setup,
    })
}

fn encode_cmd_unlink(cmd: &UsbipHeaderCmdUnlink, buf: &mut BytesMut) -> Result<(), ProtocolError> {
    buf.put_u32(cmd.seqnum);
    for i in 0..6 {
        buf.put_u32(cmd.__padding[i]);
    }
    Ok(())
}

fn decode_cmd_unlink(buf: &[u8]) -> Result<UsbipHeaderCmdUnlink, ProtocolError> {
    if buf.len() < CMD_HEADER_SIZE {
        return Err(ProtocolError::IncompleteBuffer {
            needed: CMD_HEADER_SIZE,
            available: buf.len(),
        });
    }
    let mut slice = &buf[0..CMD_HEADER_SIZE];
    let seqnum = slice
        .read_u32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read seqnum: {e}")))?;
    let mut __padding = [0u32; 6];
    for word in &mut __padding {
        *word = slice
            .read_u32::<NetworkEndian>()
            .map_err(|e| ProtocolError::EncodingError(format!("failed to read padding: {e}")))?;
    }
    Ok(UsbipHeaderCmdUnlink { seqnum, __padding })
}

fn encode_ret_unlink(ret: &UsbipHeaderRetUnlink, buf: &mut BytesMut) -> Result<(), ProtocolError> {
    buf.put_i32(ret.status);
    for i in 0..6 {
        buf.put_u32(ret.__padding[i]);
    }
    Ok(())
}

fn decode_ret_unlink(buf: &[u8]) -> Result<UsbipHeaderRetUnlink, ProtocolError> {
    if buf.len() < CMD_HEADER_SIZE {
        return Err(ProtocolError::IncompleteBuffer {
            needed: CMD_HEADER_SIZE,
            available: buf.len(),
        });
    }
    let mut slice = &buf[0..CMD_HEADER_SIZE];
    let status = slice
        .read_i32::<NetworkEndian>()
        .map_err(|e| ProtocolError::EncodingError(format!("failed to read status: {e}")))?;
    let mut __padding = [0u32; 6];
    for word in &mut __padding {
        *word = slice
            .read_u32::<NetworkEndian>()
            .map_err(|e| ProtocolError::EncodingError(format!("failed to read padding: {e}")))?;
    }
    Ok(UsbipHeaderRetUnlink { status, __padding })
}

impl UsbipHeaderCmdSubmit {
    pub fn direction(&self) -> u32 {
        if self.transfer_flags & constants::USBIP_URB_DIR_IN != 0 {
            constants::USBIP_DIR_IN
        } else {
            constants::USBIP_DIR_OUT
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::wire;

    fn make_control_cmd_submit() -> UsbipHeader {
        UsbipHeader {
            base: UsbipHeaderBasic {
                command: constants::CMD_SUBMIT,
                seqnum: 1,
                devid: 0,
                direction: constants::USBIP_DIR_OUT,
                ep: 0,
            },
            u: UsbipHeaderUnion {
                cmd_submit: UsbipHeaderCmdSubmit {
                    transfer_flags: 0,
                    transfer_buffer_length: 8,
                    start_frame: 0,
                    number_of_packets: 0,
                    interval: 0,
                    setup: [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00],
                },
            },
        }
    }

    fn make_intr_cmd_submit() -> UsbipHeader {
        UsbipHeader {
            base: UsbipHeaderBasic {
                command: constants::CMD_SUBMIT,
                seqnum: 2,
                devid: 1,
                direction: constants::USBIP_DIR_IN,
                ep: 0x81,
            },
            u: UsbipHeaderUnion {
                cmd_submit: UsbipHeaderCmdSubmit {
                    transfer_flags: constants::USBIP_URB_DIR_IN,
                    transfer_buffer_length: 8,
                    start_frame: 0,
                    number_of_packets: 0,
                    interval: 10,
                    setup: [0u8; 8],
                },
            },
        }
    }

    fn make_bulk_cmd_submit() -> UsbipHeader {
        UsbipHeader {
            base: UsbipHeaderBasic {
                command: constants::CMD_SUBMIT,
                seqnum: 3,
                devid: 2,
                direction: constants::USBIP_DIR_OUT,
                ep: 0x02,
            },
            u: UsbipHeaderUnion {
                cmd_submit: UsbipHeaderCmdSubmit {
                    transfer_flags: 0,
                    transfer_buffer_length: 512,
                    start_frame: 0,
                    number_of_packets: 0,
                    interval: 0,
                    setup: [0u8; 8],
                },
            },
        }
    }

    fn make_ret_submit() -> UsbipHeader {
        UsbipHeader {
            base: UsbipHeaderBasic {
                command: constants::RET_SUBMIT,
                seqnum: 3,
                devid: 2,
                direction: constants::USBIP_DIR_IN,
                ep: 0x02,
            },
            u: UsbipHeaderUnion {
                ret_submit: UsbipHeaderRetSubmit {
                    status: 0,
                    actual_length: 512,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    setup: [0u8; 8],
                },
            },
        }
    }

    #[test]
    fn roundtrip_control_transfer() {
        let header = make_control_cmd_submit();
        let payload = b"\x12\x01\x00\x02\x00\x00\x00\x40";
        let encoded = encode_header(&header, Some(payload)).expect("encode should succeed");
        assert_eq!(encoded.len(), PAYLOAD_OFFSET + 8);

        let (decoded, decoded_payload) = decode_header(&encoded).expect("decode should succeed");
        let cmd_specific = unsafe { decoded.u.cmd_submit };
        assert_eq!(decoded.base.command, constants::CMD_SUBMIT);
        assert_eq!(decoded.base.seqnum, 1);
        assert_eq!(cmd_specific.transfer_buffer_length, 8);
        assert_eq!(cmd_specific.setup, [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00]);
        assert_eq!(decoded_payload.as_deref(), Some(payload.as_slice()));
    }

    #[test]
    fn roundtrip_interrupt_transfer() {
        let header = make_intr_cmd_submit();
        let encoded = encode_header(&header, None).expect("encode should succeed");

        let (decoded, decoded_payload) = decode_header(&encoded).expect("decode should succeed");
        let cmd_specific = unsafe { decoded.u.cmd_submit };
        assert_eq!(decoded.base.command, constants::CMD_SUBMIT);
        assert_eq!(decoded.base.direction, constants::USBIP_DIR_IN);
        assert_eq!(cmd_specific.transfer_flags, constants::USBIP_URB_DIR_IN);
        assert_eq!(cmd_specific.interval, 10);
        assert_eq!(cmd_specific.transfer_buffer_length, 8);
        assert!(decoded_payload.is_none(), "DIR_IN CMD_SUBMIT should have no payload");
    }

    #[test]
    fn roundtrip_bulk_transfer() {
        let header = make_bulk_cmd_submit();
        let payload = vec![0xAAu8; 512];
        let encoded = encode_header(&header, Some(&payload)).expect("encode should succeed");
        assert_eq!(encoded.len(), PAYLOAD_OFFSET + 512);

        let (decoded, decoded_payload) = decode_header(&encoded).expect("decode should succeed");
        let cmd_specific = unsafe { decoded.u.cmd_submit };
        assert_eq!(decoded.base.command, constants::CMD_SUBMIT);
        assert_eq!(decoded.base.ep, 2);
        assert_eq!(cmd_specific.transfer_buffer_length, 512);
        assert_eq!(decoded_payload.as_deref(), Some(payload.as_slice()));
    }

    #[test]
    fn roundtrip_ret_submit() {
        let header = make_ret_submit();
        let payload = vec![0xBBu8; 512];
        let encoded = encode_header(&header, Some(&payload)).expect("encode should succeed");

        let (decoded, decoded_payload) = decode_header(&encoded).expect("decode should succeed");
        let ret_specific = unsafe { decoded.u.ret_submit };
        assert_eq!(ret_specific.status, 0);
        assert_eq!(ret_specific.actual_length, 512);
        assert_eq!(decoded_payload.as_deref(), Some(payload.as_slice()));
    }

    #[test]
    fn truncated_buffer_error() {
        let truncated = vec![0u8; 10];
        let result = decode_header(&truncated);
        assert!(matches!(result, Err(ProtocolError::IncompleteBuffer { .. })));
    }

    #[test]
    fn invalid_command_error() {
        let mut buf = BytesMut::new();
        buf.put_u32(0xDEADu32);
        buf.put_u32(0u32);
        buf.put_u32(0u32);
        buf.put_u32(0u32);
        buf.put_u32(0u32);
        buf.resize(PAYLOAD_OFFSET, 0);
        let result = decode_header(&buf);
        assert!(matches!(result, Err(ProtocolError::InvalidCommand { command: 0xDEAD })));
    }

    #[test]
    fn roundtrip_op_rep_import() {
        let rep = wire::OpRepImport {
            status: 0,
            path: {
                let mut p = [0u8; 256];
                p[..23].copy_from_slice(b"/sys/devices/pci0000:00");
                p
            },
            busid: {
                let mut b = [0u8; 32];
                b[..7].copy_from_slice(b"1-1.2.3");
                b
            },
            busnum: 1,
            devnum: 3,
            speed: 3,
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

        let encoded = encode_op_rep_import(&rep).expect("encode should succeed");
        let decoded = decode_op_rep_import(&encoded).expect("decode should succeed");

        assert_eq!(decoded.status, rep.status);
        assert_eq!(&decoded.path[..23], b"/sys/devices/pci0000:00");
        assert_eq!(&decoded.busid[..7], b"1-1.2.3");
        assert_eq!(decoded.busnum, rep.busnum);
        assert_eq!(decoded.devnum, rep.devnum);
        assert_eq!(decoded.speed, rep.speed);
        assert_eq!(decoded.id_vendor, rep.id_vendor);
        assert_eq!(decoded.id_product, rep.id_product);
        assert_eq!(decoded.bcd_device, rep.bcd_device);
        assert_eq!(decoded.b_device_class, rep.b_device_class);
        assert_eq!(decoded.b_device_sub_class, rep.b_device_sub_class);
        assert_eq!(decoded.b_device_protocol, rep.b_device_protocol);
        assert_eq!(decoded.b_configuration_value, rep.b_configuration_value);
        assert_eq!(decoded.b_num_configurations, rep.b_num_configurations);
        assert_eq!(decoded.b_num_interfaces, rep.b_num_interfaces);
    }

    #[test]
    fn roundtrip_op_req_import() {
        let req = wire::OpReqImport {
            status: 0,
            path: {
                let mut p = [0u8; 256];
                p[..23].copy_from_slice(b"/sys/devices/pci0000:00");
                p
            },
            busid: {
                let mut b = [0u8; 32];
                b[..7].copy_from_slice(b"1-1.2.3");
                b
            },
        };

        let encoded = encode_op_req_import(&req).expect("encode should succeed");
        let decoded = decode_op_req_import(&encoded).expect("decode should succeed");

        assert_eq!(decoded.status, req.status);
        assert_eq!(&decoded.path[..23], b"/sys/devices/pci0000:00");
        assert_eq!(&decoded.busid[..7], b"1-1.2.3");
    }

    #[test]
    fn zero_transfer_buffer_length_no_payload() {
        let header = UsbipHeader {
            base: UsbipHeaderBasic {
                command: constants::CMD_SUBMIT,
                seqnum: 4,
                devid: 0,
                direction: constants::USBIP_DIR_OUT,
                ep: 0,
            },
            u: UsbipHeaderUnion {
                cmd_submit: UsbipHeaderCmdSubmit {
                    transfer_flags: 0,
                    transfer_buffer_length: 0,
                    start_frame: 0,
                    number_of_packets: 0,
                    interval: 0,
                    setup: [0u8; 8],
                },
            },
        };

        let encoded = encode_header(&header, None).expect("encode should succeed");
        assert_eq!(encoded.len(), PAYLOAD_OFFSET);

        let (_decoded, payload) = decode_header(&encoded).expect("decode should succeed");
        assert!(payload.is_none());
    }

    #[test]
    fn buffer_too_large_error() {
        let header = make_bulk_cmd_submit();
        let payload = vec![0u8; MAX_PAYLOAD_SIZE + 1];
        let result = encode_header(&header, Some(&payload));
        assert!(matches!(result, Err(ProtocolError::BufferTooLarge { .. })));
    }

    #[test]
    fn invalid_direction_error() {
        let mut buf = BytesMut::new();
        buf.put_u32(constants::CMD_SUBMIT);
        buf.put_u32(0);
        buf.put_u32(0);
        buf.put_u32(0xDEADu32); // invalid direction
        buf.put_u32(0);
        buf.resize(PAYLOAD_OFFSET, 0);
        let result = decode_header(&buf);
        assert!(matches!(result, Err(ProtocolError::EncodingError(_))));
    }

    #[test]
    fn isochronous_rejection_error() {
        let header = UsbipHeader {
            base: UsbipHeaderBasic {
                command: constants::CMD_SUBMIT,
                seqnum: 5,
                devid: 0,
                direction: constants::USBIP_DIR_OUT,
                ep: 0,
            },
            u: UsbipHeaderUnion {
                cmd_submit: UsbipHeaderCmdSubmit {
                    transfer_flags: 0,
                    transfer_buffer_length: 0,
                    start_frame: 0,
                    number_of_packets: 1, // isochronous — should be rejected
                    interval: 0,
                    setup: [0u8; 8],
                },
            },
        };

        let result = encode_header(&header, None);
        assert!(matches!(result, Err(ProtocolError::IsochronousNotSupported)));
    }

    #[test]
    fn io_error_partial_eq_and_display() {
        let err = ProtocolError::IoError("connection reset".into());
        assert_eq!(err.to_string(), "I/O error: connection reset");
        assert_eq!(err, ProtocolError::IoError("connection reset".into()));
        assert_ne!(err, ProtocolError::EncodingError("different".into()));
        assert_ne!(err, ProtocolError::IsochronousNotSupported);
    }
}
