use core::mem::size_of;
use std::net::SocketAddr;
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::protocol::codec::{decode_op_rep_import, encode_op_req_import};
use crate::protocol::constants::USBIP_VERSION;
use crate::protocol::wire::{OpRepImport, OpReqImport};
use tracing::warn;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RETRIES: u32 = 3;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HandshakeError {
    #[error("operation timed out after {0:?}")]
    Timeout(Duration),

    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    #[error("server rejected with status {status}")]
    ServerRejected { status: u32 },

    #[error("max retries exceeded ({0})")]
    MaxRetriesExceeded(u32),

    #[error("invalid reply: {0}")]
    InvalidReply(String),
}

pub async fn import_device(
    addr: SocketAddr,
    busid: &str,
    devid: u32,
) -> Result<(TcpStream, OpRepImport), HandshakeError> {
    if busid.is_empty() {
        return Err(HandshakeError::InvalidReply("empty busid".into()));
    }
    let req = make_import_request(busid, devid);

    for attempt in 0..MAX_RETRIES {
        let result = tokio::time::timeout(HANDSHAKE_TIMEOUT, do_handshake(addr, &req)).await;

        match result {
            Err(_elapsed) => {
                warn!(
                    "handshake attempt {}/{} timed out for {}",
                    attempt + 1,
                    MAX_RETRIES,
                    busid
                );
            }
            Ok(Ok(success)) => return Ok(success),
            Ok(Err(e @ (HandshakeError::ServerRejected { .. } | HandshakeError::InvalidReply(_)))) => {
                warn!("handshake failed for {}: {}", busid, e);
                return Err(e);
            }
            Ok(Err(e)) => {
                warn!(
                    "handshake attempt {}/{} failed for {}: {}",
                    attempt + 1,
                    MAX_RETRIES,
                    busid,
                    e
                );
            }
        }
    }

    Err(HandshakeError::MaxRetriesExceeded(MAX_RETRIES))
}

async fn do_handshake(addr: SocketAddr, req: &OpReqImport) -> Result<(TcpStream, OpRepImport), HandshakeError> {
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|e| HandshakeError::ConnectionFailed(e.to_string()))?;

    let req_bytes = encode_op_req_import(req).map_err(|e| HandshakeError::InvalidReply(e.to_string()))?;
    stream
        .write_all(&req_bytes)
        .await
        .map_err(|e| HandshakeError::ConnectionFailed(e.to_string()))?;

    let mut rep_buf = vec![0u8; size_of::<OpRepImport>()];
    stream
        .read_exact(&mut rep_buf)
        .await
        .map_err(|e| HandshakeError::ConnectionFailed(e.to_string()))?;

    let reply = decode_op_rep_import(&rep_buf).map_err(|e| HandshakeError::InvalidReply(e.to_string()))?;

    if reply.status != 0 {
        return Err(HandshakeError::ServerRejected { status: reply.status });
    }
    if reply.busnum == 0 || reply.devnum == 0 {
        return Err(HandshakeError::InvalidReply(format!(
            "busnum={} devnum={}",
            reply.busnum, reply.devnum
        )));
    }

    Ok((stream, reply))
}

fn make_import_request(busid: &str, _devid: u32) -> OpReqImport {
    let mut busid_arr = [0u8; 32];
    let bytes = busid.as_bytes();
    if bytes.len() > 32 {
        warn!("busid '{}' truncated to 32 bytes", busid);
    }
    let len = bytes.len().min(32);
    busid_arr[..len].copy_from_slice(&bytes[..len]);

    OpReqImport {
        status: USBIP_VERSION as u32,
        path: {
            let mut p = [0u8; 256];
            p[..23].copy_from_slice(b"/sys/devices/pci0000:00");
            p
        },
        busid: busid_arr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::codec::encode_op_rep_import;
    use crate::protocol::wire::OpRepImport;
    use core::mem::size_of;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::time::Duration;

    const OP_REQ_SIZE: usize = size_of::<OpReqImport>();

    fn make_rep_import(status: u32, busnum: u32, devnum: u32, speed: u32) -> OpRepImport {
        OpRepImport {
            status,
            path: [0u8; 256],
            busid: {
                let mut b = [0u8; 32];
                b[..7].copy_from_slice(b"1-1.2.3");
                b
            },
            busnum,
            devnum,
            speed,
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

    async fn handshake_server_ok(listener: TcpListener) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; OP_REQ_SIZE];
        stream.read_exact(&mut buf).await.unwrap();
        let reply = make_rep_import(0, 1, 3, 3);
        let reply_bytes = encode_op_rep_import(&reply).unwrap();
        stream.write_all(&reply_bytes).await.unwrap();
    }

    async fn handshake_server_reject(listener: TcpListener) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; OP_REQ_SIZE];
        stream.read_exact(&mut buf).await.unwrap();
        let reply = make_rep_import(1, 1, 3, 3);
        let reply_bytes = encode_op_rep_import(&reply).unwrap();
        stream.write_all(&reply_bytes).await.unwrap();
    }

    async fn handshake_server_no_reply(listener: TcpListener) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; OP_REQ_SIZE];
        stream.read_exact(&mut buf).await.unwrap();
        let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
        let _ = rx.await;
        drop(stream);
    }

    async fn handshake_server_invalid(listener: TcpListener) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; OP_REQ_SIZE];
        stream.read_exact(&mut buf).await.unwrap();
        let reply = make_rep_import(0, 0, 0, 3);
        let reply_bytes = encode_op_rep_import(&reply).unwrap();
        stream.write_all(&reply_bytes).await.unwrap();
    }

    async fn handshake_server_repeat_no_reply(listener: TcpListener, count: usize) {
        for _ in 0..count {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; OP_REQ_SIZE];
            stream.read_exact(&mut buf).await.unwrap();
            drop(stream);
        }
    }

    #[tokio::test]
    async fn successful_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(handshake_server_ok(listener));

        let (_stream, result) = import_device(addr, "1-1.2.3", 3).await.unwrap();

        assert_eq!(result.status, 0);
        assert_eq!(result.busnum, 1);
        assert_eq!(result.devnum, 3);
        assert_eq!(result.speed, 3);
        assert_eq!(result.id_vendor, 0x1234);
        assert_eq!(result.id_product, 0x5678);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn server_rejection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(handshake_server_reject(listener));

        let result = import_device(addr, "1-1.2.3", 3).await;

        assert!(matches!(result, Err(HandshakeError::ServerRejected { status: 1 })));

        server.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn connect_timeout() {
        let _listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = _listener.local_addr().unwrap();

        let handle = tokio::spawn(async move { import_device(addr, "1-1", 1).await });

        tokio::time::advance(Duration::from_secs(31)).await;

        let result = handle.await.unwrap();
        assert!(matches!(result, Err(HandshakeError::MaxRetriesExceeded(3))));
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(handshake_server_no_reply(listener));

        let handle = tokio::spawn(async move { import_device(addr, "1-1.2.3", 3).await });

        tokio::time::advance(Duration::from_secs(31)).await;

        let result = handle.await.unwrap();
        assert!(matches!(result, Err(HandshakeError::MaxRetriesExceeded(3))));

        server.abort();
    }

    #[tokio::test]
    async fn max_retries_exceeded() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(handshake_server_repeat_no_reply(listener, 3));

        let result = import_device(addr, "1-1", 1).await;

        assert!(matches!(result, Err(HandshakeError::MaxRetriesExceeded(3))));

        server.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_reply_zero_ids() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(handshake_server_invalid(listener));

        let result = import_device(addr, "1-1.2.3", 3).await;

        assert!(matches!(result, Err(HandshakeError::InvalidReply(_))));

        server.await.unwrap();
    }

    #[test]
    fn handshake_error_display() {
        let timeout = HandshakeError::Timeout(Duration::from_secs(10));
        assert!(timeout.to_string().contains("10s"));

        let rejected = HandshakeError::ServerRejected { status: 5 };
        assert!(rejected.to_string().contains("5"));

        let max_retries = HandshakeError::MaxRetriesExceeded(3);
        assert!(max_retries.to_string().contains("3"));

        let invalid = HandshakeError::InvalidReply("bad data".into());
        assert!(invalid.to_string().contains("bad data"));

        let conn_failed = HandshakeError::ConnectionFailed("refused".into());
        assert!(conn_failed.to_string().contains("refused"));
    }

    #[test]
    fn handshake_error_partial_eq() {
        let a = HandshakeError::Timeout(Duration::from_secs(10));
        let b = HandshakeError::Timeout(Duration::from_secs(10));
        assert_eq!(a, b);

        let c = HandshakeError::ServerRejected { status: 1 };
        let d = HandshakeError::ServerRejected { status: 1 };
        assert_eq!(c, d);
        assert_ne!(c, HandshakeError::ServerRejected { status: 2 });

        let e = HandshakeError::MaxRetriesExceeded(3);
        let f = HandshakeError::MaxRetriesExceeded(3);
        assert_eq!(e, f);
        assert_ne!(e, HandshakeError::MaxRetriesExceeded(2));

        let g = HandshakeError::InvalidReply("x".into());
        let h = HandshakeError::InvalidReply("x".into());
        assert_eq!(g, h);

        let i = HandshakeError::ConnectionFailed("a".into());
        let j = HandshakeError::ConnectionFailed("a".into());
        assert_eq!(i, j);
        assert_ne!(i, HandshakeError::ConnectionFailed("b".into()));
    }

    #[tokio::test]
    async fn connection_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let result = import_device(addr, "1-1", 1).await;
        assert!(
            matches!(&result, Err(HandshakeError::MaxRetriesExceeded(3))),
            "expected MaxRetriesExceeded(3) after 3 connection failures, got {result:?}"
        );
    }
}
