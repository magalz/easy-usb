use std::net::SocketAddr;
use std::time::Duration;

use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::protocol::codec::{MAX_PAYLOAD_SIZE, ProtocolError, decode_header, encode_header};
use crate::protocol::constants;
use crate::protocol::handshake::{HandshakeError, import_device};
use crate::protocol::wire::{OpRepImport, UsbipHeader, UsbipHeaderBasic, UsbipHeaderCmdSubmit};

const BASIC_HEADER_SIZE: usize = core::mem::size_of::<UsbipHeaderBasic>();
const CMD_HEADER_SIZE: usize = core::mem::size_of::<UsbipHeaderCmdSubmit>();
const DIRECTION_OFFSET: usize = 12;
const TFL_OFFSET: usize = BASIC_HEADER_SIZE + 4;
const NUM_PACKETS_OFFSET: usize = BASIC_HEADER_SIZE + 12;

pub struct TcpSession {
    stream: TcpStream,
    next_seqnum: u32,
    devid: u32,
    busid: String,
    _reply: OpRepImport,
    closed: bool,
}

impl TcpSession {
    pub async fn connect(addr: SocketAddr, busid: &str, devid: u32) -> Result<Self, HandshakeError> {
        let (stream, reply) = import_device(addr, busid, devid).await?;

        let sock = SockRef::from(&stream);
        sock.set_tcp_keepalive(
            &TcpKeepalive::new()
                .with_time(Duration::from_secs(1))
                .with_interval(Duration::from_secs(10)),
        )
        .map_err(|e| HandshakeError::ConnectionFailed(format!("keepalive setup: {e}")))?;

        tracing::info!("connected to {} for device {}", addr, busid);

        Ok(Self {
            stream,
            next_seqnum: 0,
            devid,
            busid: busid.to_string(),
            _reply: reply,
            closed: false,
        })
    }

    pub fn devid(&self) -> u32 {
        self.devid
    }

    pub fn busid(&self) -> &str {
        &self.busid
    }

    pub fn reply(&self) -> &OpRepImport {
        &self._reply
    }

    pub async fn send_header(&mut self, header: &UsbipHeader, payload: Option<&[u8]>) -> Result<(), ProtocolError> {
        if self.closed {
            return Err(ProtocolError::IoError("session closed".into()));
        }
        let mut header = *header;
        header.base.seqnum = self.next_seqnum;
        self.next_seqnum = self.next_seqnum.wrapping_add(1);

        let buf = encode_header(&header, payload)?;
        self.stream
            .write_all(&buf)
            .await
            .map_err(|e| ProtocolError::IoError(e.to_string()))?;
        Ok(())
    }

    pub async fn recv_header(&mut self) -> Result<(UsbipHeader, Option<Vec<u8>>), ProtocolError> {
        if self.closed {
            return Err(ProtocolError::IoError("session closed".into()));
        }
        self.recv_header_inner().await
    }

    pub async fn recv_header_or_close(&mut self) -> Result<(UsbipHeader, Option<Vec<u8>>), ProtocolError> {
        match self.recv_header_inner().await {
            Ok(result) => Ok(result),
            Err(e) => {
                self.closed = true;
                let _ = self.stream.shutdown().await;
                Err(e)
            }
        }
    }

    pub fn into_stream(self) -> TcpStream {
        self.stream
    }

    pub async fn close(mut self) {
        tracing::info!("disconnecting from device {}", self.busid);
        let _ = self.stream.shutdown().await;
    }

    async fn recv_header_inner(&mut self) -> Result<(UsbipHeader, Option<Vec<u8>>), ProtocolError> {
        let mut buf = vec![0u8; BASIC_HEADER_SIZE + CMD_HEADER_SIZE];
        self.stream
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
        let num_packets = i32::from_be_bytes([
            buf[NUM_PACKETS_OFFSET],
            buf[NUM_PACKETS_OFFSET + 1],
            buf[NUM_PACKETS_OFFSET + 2],
            buf[NUM_PACKETS_OFFSET + 3],
        ]);
        if num_packets != 0 {
            return Err(ProtocolError::IsochronousNotSupported);
        }

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
            constants::RET_SUBMIT => {
                let al = i32::from_be_bytes([
                    buf[TFL_OFFSET],
                    buf[TFL_OFFSET + 1],
                    buf[TFL_OFFSET + 2],
                    buf[TFL_OFFSET + 3],
                ]);
                if al > 0 { al as usize } else { 0 }
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
            self.stream
                .read_exact(&mut payload)
                .await
                .map_err(|e| ProtocolError::IoError(e.to_string()))?;
            buf.extend_from_slice(&payload);
        }

        decode_header(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::codec::encode_op_rep_import;
    use crate::protocol::constants;
    use crate::protocol::wire::{
        OpRepImport, UsbipHeader, UsbipHeaderBasic, UsbipHeaderCmdSubmit, UsbipHeaderRetSubmit, UsbipHeaderUnion,
    };
    use core::mem::size_of;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const OP_REQ_SIZE: usize = size_of::<crate::protocol::wire::OpReqImport>();

    fn make_rep_import_ok() -> OpRepImport {
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

    fn make_cmd_submit(seqnum: u32, tfl: i32, _payload_len: i32) -> UsbipHeader {
        UsbipHeader {
            base: UsbipHeaderBasic {
                command: constants::CMD_SUBMIT,
                seqnum,
                devid: 3,
                direction: constants::USBIP_DIR_OUT,
                ep: 0x02,
            },
            u: UsbipHeaderUnion {
                cmd_submit: UsbipHeaderCmdSubmit {
                    transfer_flags: 0,
                    transfer_buffer_length: tfl,
                    start_frame: 0,
                    number_of_packets: 0,
                    interval: 0,
                    setup: [0u8; 8],
                },
            },
        }
    }

    fn make_ret_submit(seqnum: u32, actual_length: i32) -> UsbipHeader {
        UsbipHeader {
            base: UsbipHeaderBasic {
                command: constants::RET_SUBMIT,
                seqnum,
                devid: 3,
                direction: constants::USBIP_DIR_IN,
                ep: 0x02,
            },
            u: UsbipHeaderUnion {
                ret_submit: UsbipHeaderRetSubmit {
                    status: 0,
                    actual_length,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    setup: [0u8; 8],
                },
            },
        }
    }

    async fn full_handshake_server(listener: TcpListener) -> TcpStream {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; OP_REQ_SIZE];
        stream.read_exact(&mut buf).await.unwrap();
        let reply = make_rep_import_ok();
        let reply_bytes = encode_op_rep_import(&reply).unwrap();
        stream.write_all(&reply_bytes).await.unwrap();
        stream
    }

    #[tokio::test]
    async fn connect_and_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(full_handshake_server(listener));

        let session = TcpSession::connect(addr, "1-1.2.3", 3).await.unwrap();

        assert_eq!(session.devid(), 3);
        assert_eq!(session.busid(), "1-1.2.3");

        server.await.unwrap();
        session.close().await;
    }

    #[tokio::test]
    async fn send_header() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let mut stream = full_handshake_server(listener).await;

            let mut header_buf = vec![0u8; BASIC_HEADER_SIZE + CMD_HEADER_SIZE];
            stream.read_exact(&mut header_buf).await.unwrap();

            let command = u32::from_be_bytes([header_buf[0], header_buf[1], header_buf[2], header_buf[3]]);
            let seqnum = u32::from_be_bytes([header_buf[4], header_buf[5], header_buf[6], header_buf[7]]);

            assert_eq!(command, constants::CMD_SUBMIT);
            assert_eq!(seqnum, 0);
        });

        let mut session = TcpSession::connect(addr, "1-1.2.3", 3).await.unwrap();
        let header = make_cmd_submit(0, 0, 0);

        session.send_header(&header, None).await.unwrap();

        server_handle.await.unwrap();
        session.close().await;
    }

    #[tokio::test]
    async fn recv_header() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let mut stream = full_handshake_server(listener).await;

            let ret_header = make_ret_submit(0, 0);
            let encoded = encode_header(&ret_header, None).unwrap();
            stream.write_all(&encoded).await.unwrap();
        });

        let mut session = TcpSession::connect(addr, "1-1.2.3", 3).await.unwrap();

        let (header, payload) = session.recv_header().await.unwrap();

        assert_eq!(header.base.command, constants::RET_SUBMIT);
        assert_eq!(header.base.seqnum, 0);
        assert!(payload.is_none());

        server_handle.await.unwrap();
        session.close().await;
    }

    #[tokio::test]
    async fn send_recv_roundtrip() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let test_payload = b"HELLO USBIP DATA";

        let server_handle = tokio::spawn(async move {
            let mut stream = full_handshake_server(listener).await;

            let mut header_buf = vec![0u8; BASIC_HEADER_SIZE + CMD_HEADER_SIZE + test_payload.len()];
            stream.read_exact(&mut header_buf).await.unwrap();

            let (cmd_header, cmd_payload) = decode_header(&header_buf).unwrap();
            let cmd = unsafe { cmd_header.u.cmd_submit };
            assert_eq!(cmd.transfer_buffer_length, test_payload.len() as i32);
            assert_eq!(cmd_payload.as_deref(), Some(test_payload.as_slice()));

            let echo = make_ret_submit(0, test_payload.len() as i32);
            let encoded = encode_header(&echo, Some(test_payload)).unwrap();
            stream.write_all(&encoded).await.unwrap();
        });

        let mut session = TcpSession::connect(addr, "1-1.2.3", 3).await.unwrap();

        let cmd = make_cmd_submit(0, test_payload.len() as i32, test_payload.len() as i32);
        session.send_header(&cmd, Some(test_payload)).await.unwrap();

        let (ret_header, ret_payload) = session.recv_header().await.unwrap();
        assert_eq!(ret_header.base.command, constants::RET_SUBMIT);
        assert_eq!(ret_payload.as_deref(), Some(test_payload.as_slice()));

        server_handle.await.unwrap();
        session.close().await;
    }

    #[tokio::test]
    async fn malformed_urb_closes_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let mut stream = full_handshake_server(listener).await;
            stream.write_all(b"garbage_bytes_not_a_header").await.unwrap();
            stream.flush().await.unwrap();
        });

        let mut session = TcpSession::connect(addr, "1-1.2.3", 3).await.unwrap();

        let result = session.recv_header_or_close().await;
        assert!(result.is_err());

        let mut stream = session.into_stream();
        let mut buf = [0u8; 1];
        let read_result = stream.read(&mut buf).await;
        assert!(read_result.is_err() || read_result.unwrap_or(0) == 0);

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn recv_header_malformed_data() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let mut stream = full_handshake_server(listener).await;
            stream.write_all(b"garbage_bytes_not_a_header").await.unwrap();
            stream.flush().await.unwrap();
        });

        let mut session = TcpSession::connect(addr, "1-1.2.3", 3).await.unwrap();

        let result = session.recv_header().await;
        assert!(result.is_err());

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn seqnum_wraps_at_u32_max() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let mut stream = full_handshake_server(listener).await;
            let header_size = BASIC_HEADER_SIZE + CMD_HEADER_SIZE;
            let mut expected_seqnum: u32 = u32::MAX - 1;

            for _ in 0..3 {
                let mut buf = vec![0u8; header_size];
                stream.read_exact(&mut buf).await.unwrap();
                let seqnum = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
                assert_eq!(seqnum, expected_seqnum, "seqnum mismatch at wrap");
                expected_seqnum = expected_seqnum.wrapping_add(1);
            }
        });

        let mut session = TcpSession::connect(addr, "1-1.2.3", 3).await.unwrap();
        session.next_seqnum = u32::MAX - 1;

        for _ in 0..3 {
            let header = make_cmd_submit(99, 0, 0);
            session.send_header(&header, None).await.unwrap();
        }

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn sequence_number_increments() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let mut stream = full_handshake_server(listener).await;
            let header_size = BASIC_HEADER_SIZE + CMD_HEADER_SIZE;
            let mut expected_seqnum = 0u32;

            for _ in 0..3 {
                let mut buf = vec![0u8; header_size];
                stream.read_exact(&mut buf).await.unwrap();

                let seqnum = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
                assert_eq!(seqnum, expected_seqnum, "seqnum mismatch");
                expected_seqnum += 1;
            }
        });

        let mut session = TcpSession::connect(addr, "1-1.2.3", 3).await.unwrap();

        for _ in 0..3 {
            let header = make_cmd_submit(99, 0, 0);
            session.send_header(&header, None).await.unwrap();
        }

        server_handle.await.unwrap();
        session.close().await;
    }

    #[tokio::test]
    async fn graceful_close() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let mut stream = full_handshake_server(listener).await;

            let mut buf = [0u8; 1];
            let read_result = stream.read(&mut buf).await;
            let is_closed = matches!(&read_result, Ok(0)) || read_result.is_err();
            assert!(is_closed, "stream should be closed or error after client close");
        });

        let session = TcpSession::connect(addr, "1-1.2.3", 3).await.unwrap();
        session.close().await;

        server_handle.await.unwrap();
    }
}
