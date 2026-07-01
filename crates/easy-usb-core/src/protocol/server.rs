use core::mem::size_of;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::protocol::codec::{
    MAX_PAYLOAD_SIZE, ProtocolError, decode_header, decode_op_req_import, encode_header, encode_op_rep_import,
};
use crate::protocol::constants;
use crate::protocol::wire::{
    OpRepImport, OpReqImport, UsbipHeader, UsbipHeaderBasic, UsbipHeaderCmdSubmit, UsbipHeaderRetSubmit,
    UsbipHeaderRetUnlink, UsbipHeaderUnion,
};

const DIRECTION_OFFSET: usize = 12;
const TFL_OFFSET: usize = 20 + 4;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AcceptError {
    #[error("accept failed: {0}")]
    AcceptFailed(String),
    #[error("invalid import request: {0}")]
    InvalidRequest(String),
    #[error("invalid reply: {0}")]
    InvalidReply(String),
}

pub async fn accept_device(listener: &TcpListener) -> Result<(TcpStream, OpReqImport), AcceptError> {
    let (mut stream, _addr) = listener
        .accept()
        .await
        .map_err(|e| AcceptError::AcceptFailed(e.to_string()))?;

    let mut buf = vec![0u8; size_of::<OpReqImport>()];
    stream
        .read_exact(&mut buf)
        .await
        .map_err(|e| AcceptError::AcceptFailed(format!("read OP_REQ_IMPORT: {e}")))?;

    let req = decode_op_req_import(&buf).map_err(|e| AcceptError::InvalidRequest(e.to_string()))?;

    if req.busid.iter().all(|&b| b == 0) {
        return Err(AcceptError::InvalidRequest("empty busid".into()));
    }

    let busid_str = core::str::from_utf8(&req.busid)
        .map(|s| s.trim_end_matches('\0'))
        .map_err(|_| AcceptError::InvalidRequest("non-UTF-8 busid".into()))?;
    tracing::info!("accepted device import request for busid={busid_str}");

    Ok((stream, req))
}

pub async fn send_op_rep_import(stream: &mut TcpStream, rep: &OpRepImport) -> Result<(), ProtocolError> {
    let buf = encode_op_rep_import(rep)?;
    stream
        .write_all(&buf)
        .await
        .map_err(|e| ProtocolError::IoError(e.to_string()))
}

pub async fn serve_urb_echo(stream: &mut TcpStream) -> Result<UsbipHeader, ProtocolError> {
    let basic_size = size_of::<UsbipHeaderBasic>();
    let cmd_size = size_of::<UsbipHeaderCmdSubmit>();
    let header_size = basic_size + cmd_size;

    let mut buf = vec![0u8; header_size];
    stream
        .read_exact(&mut buf)
        .await
        .map_err(|e| ProtocolError::IoError(e.to_string()))?;

    let command = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let direction = u32::from_be_bytes([
        buf[DIRECTION_OFFSET],
        buf[DIRECTION_OFFSET + 1],
        buf[DIRECTION_OFFSET + 2],
        buf[DIRECTION_OFFSET + 3],
    ]);

    let payload_len = match command {
        constants::CMD_SUBMIT => {
            let tfl = i32::from_be_bytes([
                buf[TFL_OFFSET],
                buf[TFL_OFFSET + 1],
                buf[TFL_OFFSET + 2],
                buf[TFL_OFFSET + 3],
            ]);
            if tfl > 0 && direction == constants::USBIP_DIR_OUT {
                tfl as usize
            } else {
                0
            }
        }
        _ => 0,
    };

    if payload_len > MAX_PAYLOAD_SIZE {
        return Err(ProtocolError::BufferTooLarge {
            size: payload_len,
            max: MAX_PAYLOAD_SIZE,
        });
    }

    if payload_len > 0 {
        let mut payload = vec![0u8; payload_len];
        stream
            .read_exact(&mut payload)
            .await
            .map_err(|e| ProtocolError::IoError(e.to_string()))?;
        buf.extend_from_slice(&payload);
    }

    let (header, payload) = decode_header(&buf)?;

    match header.base.command {
        constants::CMD_SUBMIT => {
            let cmd = header
                .cmd_submit()
                .ok_or_else(|| ProtocolError::EncodingError("CMD_SUBMIT header missing cmd_submit data".into()))?;
            if cmd.number_of_packets != 0 {
                return Err(ProtocolError::IsochronousNotSupported);
            }
            let echo_payload = payload.as_deref();
            let echo_len = i32::try_from(echo_payload.map(|p| p.len()).unwrap_or(0)).unwrap_or(0);
            let reply = UsbipHeader {
                base: UsbipHeaderBasic {
                    command: constants::RET_SUBMIT,
                    seqnum: header.base.seqnum,
                    devid: header.base.devid,
                    direction: constants::USBIP_DIR_IN,
                    ep: header.base.ep,
                },
                u: UsbipHeaderUnion {
                    ret_submit: UsbipHeaderRetSubmit {
                        status: 0,
                        actual_length: echo_len,
                        start_frame: 0,
                        number_of_packets: 0,
                        error_count: 0,
                        setup: cmd.setup,
                    },
                },
            };
            let encoded = encode_header(&reply, echo_payload)?;
            stream
                .write_all(&encoded)
                .await
                .map_err(|e| ProtocolError::IoError(e.to_string()))?;
        }
        constants::CMD_UNLINK => {
            let reply = UsbipHeader {
                base: UsbipHeaderBasic {
                    command: constants::RET_UNLINK,
                    seqnum: header.base.seqnum,
                    devid: header.base.devid,
                    direction: header.base.direction,
                    ep: header.base.ep,
                },
                u: UsbipHeaderUnion {
                    ret_unlink: UsbipHeaderRetUnlink {
                        status: 0,
                        __padding: [0u32; 6],
                    },
                },
            };
            let encoded = encode_header(&reply, None)?;
            stream
                .write_all(&encoded)
                .await
                .map_err(|e| ProtocolError::IoError(e.to_string()))?;
        }
        _ => {
            return Err(ProtocolError::InvalidCommand {
                command: header.base.command,
            });
        }
    }

    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::codec::encode_op_req_import;
    use crate::protocol::wire::OpReqImport;
    use tokio::net::TcpListener;

    fn make_op_req_import(busid: &str) -> OpReqImport {
        let mut busid_arr = [0u8; 32];
        let bytes = busid.as_bytes();
        let len = bytes.len().min(32);
        busid_arr[..len].copy_from_slice(&bytes[..len]);
        OpReqImport {
            status: 0x0111,
            path: {
                let mut p = [0u8; 256];
                p[..23].copy_from_slice(b"/sys/devices/pci0000:00");
                p
            },
            busid: busid_arr,
        }
    }

    fn make_op_rep_import_ok() -> OpRepImport {
        OpRepImport {
            status: 0,
            path: [0u8; 256],
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
        }
    }

    #[tokio::test]
    async fn accept_device_receives_op_req_import() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let req = make_op_req_import("1-1.2.3");

        let server = tokio::spawn(async move { accept_device(&listener).await });

        let client = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let encoded = encode_op_req_import(&req).unwrap();
            stream.write_all(&encoded).await.unwrap();
            stream
        });

        let (client_stream, received_req) = server.await.unwrap().unwrap();
        let _client_stream = client.await.unwrap();

        assert_eq!(&received_req.busid[..7], b"1-1.2.3");
        drop(client_stream);
    }

    #[tokio::test]
    async fn accept_device_rejects_empty_busid() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let req = make_op_req_import("");

        let server = tokio::spawn(async move { accept_device(&listener).await });

        tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let encoded = encode_op_req_import(&req).unwrap();
            stream.write_all(&encoded).await.unwrap();
        });

        let result = server.await.unwrap();
        assert!(matches!(result, Err(AcceptError::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn accept_device_rejects_non_utf8_busid() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut req = make_op_req_import("1-1");
        req.busid[0] = 0xFF;

        let server = tokio::spawn(async move { accept_device(&listener).await });

        tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let encoded = encode_op_req_import(&req).unwrap();
            stream.write_all(&encoded).await.unwrap();
        });

        let result = server.await.unwrap();
        assert!(matches!(result, Err(AcceptError::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn accept_device_rejects_truncated_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move { accept_device(&listener).await });

        tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            stream.write_all(b"too_short").await.unwrap();
        });

        let result = server.await.unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn send_op_rep_import_writes_reply() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let rep = make_op_rep_import_ok();

        let server = tokio::spawn(async move {
            let (mut stream, _req) = accept_device(&listener).await.unwrap();
            send_op_rep_import(&mut stream, &rep).await.unwrap();
            stream
        });

        let client = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req = make_op_req_import("1-1.2.3");
            let encoded_req = encode_op_req_import(&req).unwrap();
            stream.write_all(&encoded_req).await.unwrap();
            let mut rep_buf = vec![0u8; size_of::<OpRepImport>()];
            stream.read_exact(&mut rep_buf).await.unwrap();
            let decoded = crate::protocol::codec::decode_op_rep_import(&rep_buf).unwrap();
            assert_eq!(decoded.status, 0);
            assert_eq!(decoded.busnum, 1);
            assert_eq!(decoded.devnum, 3);
            stream
        });

        let _stream = server.await.unwrap();
        let _client_stream = client.await.unwrap();
    }

    #[tokio::test]
    async fn serve_urb_echo_echoes_cmd_submit() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let payload = b"\x01\x02\x03\x04";

        let server = tokio::spawn(async move {
            let (mut stream, _req) = accept_device(&listener).await.unwrap();
            let rep = make_op_rep_import_ok();
            send_op_rep_import(&mut stream, &rep).await.unwrap();
            serve_urb_echo(&mut stream).await
        });

        let client = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req = make_op_req_import("1-1.2.3");
            let encoded_req = encode_op_req_import(&req).unwrap();
            stream.write_all(&encoded_req).await.unwrap();

            let mut rep_buf = vec![0u8; size_of::<OpRepImport>()];
            stream.read_exact(&mut rep_buf).await.unwrap();

            let cmd = UsbipHeader {
                base: UsbipHeaderBasic {
                    command: constants::CMD_SUBMIT,
                    seqnum: 1,
                    devid: 3,
                    direction: constants::USBIP_DIR_OUT,
                    ep: 0x02,
                },
                u: UsbipHeaderUnion {
                    cmd_submit: UsbipHeaderCmdSubmit {
                        transfer_flags: 0,
                        transfer_buffer_length: payload.len() as i32,
                        start_frame: 0,
                        number_of_packets: 0,
                        interval: 0,
                        setup: [0u8; 8],
                    },
                },
            };
            let encoded_cmd = encode_header(&cmd, Some(payload)).unwrap();
            stream.write_all(&encoded_cmd).await.unwrap();

            let basic_size = size_of::<UsbipHeaderBasic>();
            let cmd_size = size_of::<UsbipHeaderCmdSubmit>();
            let mut ret_buf = vec![0u8; basic_size + cmd_size + payload.len()];
            stream.read_exact(&mut ret_buf).await.unwrap();

            let (ret_header, ret_payload) = decode_header(&ret_buf).unwrap();
            assert_eq!(ret_header.base.command, constants::RET_SUBMIT);
            assert_eq!(ret_payload.as_deref(), Some(payload.as_slice()));
        });

        let result = server.await.unwrap();
        assert!(result.is_ok());
        client.await.unwrap();
    }

    #[tokio::test]
    async fn serve_urb_echo_handles_cmd_unlink() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _req) = accept_device(&listener).await.unwrap();
            let rep = make_op_rep_import_ok();
            send_op_rep_import(&mut stream, &rep).await.unwrap();
            serve_urb_echo(&mut stream).await
        });

        let client = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req = make_op_req_import("1-1.2.3");
            let encoded_req = encode_op_req_import(&req).unwrap();
            stream.write_all(&encoded_req).await.unwrap();

            let mut rep_buf = vec![0u8; size_of::<OpRepImport>()];
            stream.read_exact(&mut rep_buf).await.unwrap();

            let cmd = UsbipHeader {
                base: UsbipHeaderBasic {
                    command: constants::CMD_UNLINK,
                    seqnum: 5,
                    devid: 3,
                    direction: constants::USBIP_DIR_OUT,
                    ep: 0x02,
                },
                u: UsbipHeaderUnion {
                    cmd_unlink: crate::protocol::wire::UsbipHeaderCmdUnlink {
                        seqnum: 1,
                        __padding: [0u32; 6],
                    },
                },
            };
            let encoded_cmd = encode_header(&cmd, None).unwrap();
            stream.write_all(&encoded_cmd).await.unwrap();

            let basic_size = size_of::<UsbipHeaderBasic>();
            let cmd_size = size_of::<UsbipHeaderCmdSubmit>();
            let mut ret_buf = vec![0u8; basic_size + cmd_size];
            stream.read_exact(&mut ret_buf).await.unwrap();

            let (ret_header, _ret_payload) = decode_header(&ret_buf).unwrap();
            assert_eq!(ret_header.base.command, constants::RET_UNLINK);
            assert_eq!(ret_header.base.seqnum, 5);
        });

        let result = server.await.unwrap();
        assert!(result.is_ok());
        client.await.unwrap();
    }

    #[tokio::test]
    async fn serve_urb_echo_echoes_intr_in_cmd_submit() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _req) = accept_device(&listener).await.unwrap();
            let rep = make_op_rep_import_ok();
            send_op_rep_import(&mut stream, &rep).await.unwrap();
            serve_urb_echo(&mut stream).await
        });

        let client = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req = make_op_req_import("1-1.2.3");
            let encoded_req = encode_op_req_import(&req).unwrap();
            stream.write_all(&encoded_req).await.unwrap();

            let mut rep_buf = vec![0u8; size_of::<OpRepImport>()];
            stream.read_exact(&mut rep_buf).await.unwrap();

            let cmd = UsbipHeader {
                base: UsbipHeaderBasic {
                    command: constants::CMD_SUBMIT,
                    seqnum: 2,
                    devid: 3,
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
            };
            let encoded_cmd = encode_header(&cmd, None).unwrap();
            stream.write_all(&encoded_cmd).await.unwrap();

            let basic_size = size_of::<UsbipHeaderBasic>();
            let cmd_size = size_of::<UsbipHeaderCmdSubmit>();
            let mut ret_buf = vec![0u8; basic_size + cmd_size];
            stream.read_exact(&mut ret_buf).await.unwrap();

            let (ret_header, ret_payload) = decode_header(&ret_buf).unwrap();
            assert_eq!(ret_header.base.command, constants::RET_SUBMIT);
            assert_eq!(ret_header.base.seqnum, 2);
            assert!(ret_payload.is_none());
        });

        let result = server.await.unwrap();
        assert!(result.is_ok(), "serve_urb_echo failed: {:?}", result.as_ref().err());
        client.await.unwrap();
    }

    #[tokio::test]
    async fn serve_urb_echo_rejects_unknown_command() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _req) = accept_device(&listener).await.unwrap();
            let rep = make_op_rep_import_ok();
            send_op_rep_import(&mut stream, &rep).await.unwrap();
            serve_urb_echo(&mut stream).await
        });

        let client = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req = make_op_req_import("1-1.2.3");
            let encoded_req = encode_op_req_import(&req).unwrap();
            stream.write_all(&encoded_req).await.unwrap();

            let mut rep_buf = vec![0u8; size_of::<OpRepImport>()];
            stream.read_exact(&mut rep_buf).await.unwrap();

            use bytes::BufMut;
            let mut raw = bytes::BytesMut::new();
            raw.put_u32(0xDEADu32); // command
            raw.put_u32(0u32); // seqnum
            raw.put_u32(0u32); // devid
            raw.put_u32(constants::USBIP_DIR_IN); // direction
            raw.put_u32(0u32); // ep
            raw.resize(48, 0u8); // pad to full header size
            stream.write_all(&raw).await.unwrap();
        });

        let result = server.await.unwrap();
        assert!(matches!(result, Err(ProtocolError::InvalidCommand { command: 0xDEAD })));
        client.await.unwrap();
    }

    #[test]
    fn accept_error_display() {
        let af = AcceptError::AcceptFailed("connection reset".into());
        assert!(af.to_string().contains("connection reset"));

        let ir = AcceptError::InvalidRequest("bad busid".into());
        assert!(ir.to_string().contains("bad busid"));

        let iv = AcceptError::InvalidReply("wrong size".into());
        assert!(iv.to_string().contains("wrong size"));
    }

    #[test]
    fn accept_error_partial_eq() {
        let a = AcceptError::AcceptFailed("x".into());
        let b = AcceptError::AcceptFailed("x".into());
        assert_eq!(a, b);
        assert_ne!(a, AcceptError::AcceptFailed("y".into()));

        let c = AcceptError::InvalidRequest("z".into());
        let d = AcceptError::InvalidRequest("z".into());
        assert_eq!(c, d);
        assert_ne!(c, AcceptError::InvalidReply("z".into()));
    }
}
