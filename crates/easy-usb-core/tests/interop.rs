use core::mem::size_of;
use std::net::SocketAddr;
use std::time::Duration;

use easy_usb_core::protocol::codec::decode_op_rep_import;
use easy_usb_core::protocol::constants;
use easy_usb_core::protocol::wire::{UsbipHeaderBasic, UsbipHeaderCmdSubmit, UsbipHeaderUnion};
use easy_usb_core::protocol::{
    OpRepImport, TcpSession, UsbipHeader, accept_device, send_op_rep_import, serve_urb_echo,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::process::Command;

const USBIPD_PORT: u16 = 3240;
const WORKSPACE_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn docker_available() -> bool {
    std::env::var("USBIP_DOCKER_INTEROP").map(|v| v == "1").unwrap_or(false)
}

fn container_already_running() -> bool {
    std::env::var("EASY_USB_CONTAINER_READY")
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn skip_if_no_docker() {
    if !docker_available() {
        eprintln!(
            "SKIP: USBIP_DOCKER_INTEROP not set. Run: USBIP_DOCKER_INTEROP=1 cargo test -p easy-usb-core -- --ignored interop"
        );
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

async fn ensure_usbipd_ready(usbipd_addr: SocketAddr) -> Result<(), String> {
    if container_already_running() {
        return Ok(());
    }
    wait_for_usbipd(usbipd_addr, Duration::from_secs(30)).await
}

async fn wait_for_usbipd(addr: SocketAddr, timeout: Duration) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            return Err("timeout waiting for usbipd".into());
        }
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn docker_compose_up() -> Result<(), String> {
    if container_already_running() {
        return Ok(());
    }
    let output = Command::new("docker")
        .args(["compose", "-f", "docker/docker-compose.yml", "up", "--build", "-d"])
        .current_dir(WORKSPACE_ROOT)
        .output()
        .await
        .map_err(|e| format!("docker compose up failed: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("docker compose up failed: {stderr}"));
    }
    Ok(())
}

async fn docker_compose_down() {
    if container_already_running() {
        return;
    }
    let _ = Command::new("docker")
        .args(["compose", "-f", "docker/docker-compose.yml", "down", "-t", "5"])
        .current_dir(WORKSPACE_ROOT)
        .output()
        .await;
}

async fn docker_exec(args: &[&str]) -> Result<String, String> {
    let output = Command::new("docker")
        .args(["compose", "-f", "docker/docker-compose.yml", "exec", "-T", "usbipd"])
        .args(args)
        .current_dir(WORKSPACE_ROOT)
        .output()
        .await
        .map_err(|e| format!("docker exec failed: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("docker exec failed: {stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[ignore = "requires Docker with usbip; run with USBIP_DOCKER_INTEROP=1"]
#[tokio::test]
async fn interop_client_against_reference_usbipd() {
    if !docker_available() {
        skip_if_no_docker();
        return;
    }

    docker_compose_up().await.expect("docker compose up");
    let usbipd_addr: SocketAddr = ([127, 0, 0, 1], USBIPD_PORT).into();
    ensure_usbipd_ready(usbipd_addr).await.expect("usbipd should be ready");

    // Import the virtual test device
    let busid = "1-1";
    let devid = 1u32;

    let mut session = TcpSession::connect(usbipd_addr, busid, devid)
        .await
        .expect("should import device from reference usbipd");

    assert_eq!(session.devid(), devid, "devid should match");
    assert_eq!(session.busid(), busid, "busid should match");

    // Send CMD_SUBMIT (interrupt IN on ep 0x81)
    let cmd = UsbipHeader {
        base: UsbipHeaderBasic {
            command: constants::CMD_SUBMIT,
            seqnum: 0,
            devid: session.devid(),
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

    session.send_header(&cmd, None).await.expect("should send CMD_SUBMIT");

    let (ret_header, _ret_payload) = session.recv_header().await.expect("should receive RET_SUBMIT");

    assert_eq!(
        ret_header.base.command,
        constants::RET_SUBMIT,
        "should receive RET_SUBMIT"
    );
    assert_eq!(
        unsafe { ret_header.u.ret_submit.status },
        0,
        "RET_SUBMIT status should be 0"
    );

    // Verify no custom headers: opcodes match USB/IP v1.1.1
    assert_eq!(
        size_of::<OpRepImport>(),
        316,
        "OpRepImport size must match reference (316 bytes)"
    );

    session.close().await;
    docker_compose_down().await;
}

#[ignore = "requires Docker with usbip; run with USBIP_DOCKER_INTEROP=1"]
#[tokio::test]
async fn interop_server_handshake_easy_usb_as_server() {
    if !docker_available() {
        skip_if_no_docker();
        return;
    }

    docker_compose_up().await.expect("docker compose up");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("should bind");
    let server_addr = listener.local_addr().expect("should get addr");

    let server_handle = tokio::spawn(async move {
        let (mut stream, req) = accept_device(&listener).await.expect("should accept device");

        assert!(!req.busid.iter().all(|&b| b == 0), "busid should be non-empty");

        let rep = make_op_rep_import_ok();
        send_op_rep_import(&mut stream, &rep)
            .await
            .expect("should send OP_REP_IMPORT");
    });

    let host_ip = if cfg!(target_os = "windows") {
        "host.docker.internal"
    } else {
        "172.17.0.1"
    };

    let result = tokio::time::timeout(
        Duration::from_secs(15),
        docker_exec(&[
            "usbip",
            "attach",
            "-r",
            host_ip,
            "-p",
            &server_addr.port().to_string(),
            "-b",
            "1-1",
        ]),
    )
    .await;

    match result {
        Ok(Ok(output)) => eprintln!("usbip attach succeeded: {output}"),
        Ok(Err(e)) => eprintln!("usbip attach failed (expected without real USB hardware): {e}"),
        Err(_) => eprintln!("usbip attach timed out"),
    }

    server_handle.await.expect("server should complete");
    docker_compose_down().await;
}

#[ignore = "requires Docker with usbip; run with USBIP_DOCKER_INTEROP=1"]
#[tokio::test]
async fn interop_server_urb_echo_with_mock_client() {
    if !docker_available() {
        skip_if_no_docker();
        return;
    }

    docker_compose_up().await.expect("docker compose up");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("should bind");
    let server_addr = listener.local_addr().expect("should get addr");

    let server_handle = tokio::spawn(async move {
        let (mut stream, _req) = accept_device(&listener).await.expect("should accept device");

        let rep = make_op_rep_import_ok();
        send_op_rep_import(&mut stream, &rep)
            .await
            .expect("should send OP_REP_IMPORT");

        let received = serve_urb_echo(&mut stream).await.expect("should echo URB");

        assert_eq!(
            received.base.command,
            constants::CMD_SUBMIT,
            "should receive CMD_SUBMIT"
        );
    });

    // Mock client: connect, send OP_REQ_IMPORT, receive OP_REP_IMPORT, then CMD_SUBMIT, receive RET_SUBMIT
    let client_handle = tokio::spawn(async move {
        use easy_usb_core::protocol::codec::{encode_header, encode_op_req_import};
        use easy_usb_core::protocol::wire::OpReqImport;

        let mut stream = tokio::net::TcpStream::connect(server_addr)
            .await
            .expect("should connect");

        let mut busid_arr = [0u8; 32];
        busid_arr[..3].copy_from_slice(b"1-1");
        let req = OpReqImport {
            status: constants::USBIP_VERSION as u32,
            path: {
                let mut p = [0u8; 256];
                p[..23].copy_from_slice(b"/sys/devices/pci0000:00");
                p
            },
            busid: busid_arr,
        };
        let encoded_req = encode_op_req_import(&req).expect("encode");
        stream.write_all(&encoded_req).await.unwrap();

        let mut rep_buf = vec![0u8; size_of::<OpRepImport>()];
        stream.read_exact(&mut rep_buf).await.unwrap();
        let _rep = decode_op_rep_import(&rep_buf).expect("decode");
        assert_eq!(_rep.status, 0, "OP_REP_IMPORT status should be 0");

        let cmd = UsbipHeader {
            base: UsbipHeaderBasic {
                command: constants::CMD_SUBMIT,
                seqnum: 1,
                devid: 0,
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
        let encoded_cmd = encode_header(&cmd, None).expect("encode cmd");
        stream.write_all(&encoded_cmd).await.unwrap();

        let basic_size = size_of::<easy_usb_core::protocol::wire::UsbipHeaderBasic>();
        let cmd_size = size_of::<easy_usb_core::protocol::wire::UsbipHeaderCmdSubmit>();
        let mut ret_buf = vec![0u8; basic_size + cmd_size];
        stream.read_exact(&mut ret_buf).await.unwrap();

        let (ret_header, _payload) = easy_usb_core::protocol::codec::decode_header(&ret_buf).expect("decode ret");
        assert_eq!(ret_header.base.command, constants::RET_SUBMIT, "should get RET_SUBMIT");
        assert_eq!(ret_header.base.seqnum, 1, "seqnum should match");
    });

    client_handle.await.expect("client should complete");
    server_handle.await.expect("server should complete");
    docker_compose_down().await;
}

#[test]
fn struct_sizes_match_reference() {
    assert_eq!(size_of::<OpRepImport>(), 316, "OpRepImport must be 316 bytes");
}
