# USB/IP Interop Verification Checklist (Manual)

**Story:** 1.4 — USB/IP Interop Harness vs Reference Linux usbipd — Phase A GATE
**Date:** __________________
**Maintainer:** __________________
**Status:** ☐ Not Started  ☐ In Progress  ☐ Complete

> This is a **manual sign-off gate**. CI-green does not cover this gate
> because USB hardware passthrough is not available in CI runners.
> A maintainer must sign off before any platform-adapter story
> (1.7, 1.8, 1.16) may proceed.

---

## Prerequisites

- [ ] Linux machine with kernel 4.19+ and `usbip` tools installed
- [ ] `usbip-host` and `vhci-hcd` kernel modules loaded
- [ ] One physical USB test device that appears in `lsusb` (e.g. a USB HID or mass-storage device)
- [ ] Rust toolchain installed, `easy-usb-core` checked out and compiled (`cargo build -p easy-usb-core`)

---

## Section A: Client Interop (easy-usb as client against reference usbipd)

### A.1 Start reference usbipd server

- [ ] Run `sudo usbipd -D`
- [ ] Bind the test device: `sudo usbip bind -b <busid>` (find busid via `usbip list -l`)
- [ ] Verify the device is exported: `usbip list -r 127.0.0.1`

### A.2 Run easy-usb client interop test

- [ ] Run `USBIP_DOCKER_INTEROP=1 cargo test -p easy-usb-core -- --ignored interop_client`
- [ ] Verify output shows: `test interop_client_against_reference_usbipd ... ok`
- [ ] Verify the test completes within the 30-second timeout
- [ ] Verify OpRepImport fields are valid: `status == 0`, `busnum > 0`, `devnum > 0`

### A.3 Wire format validation

- [ ] Confirm no custom USB/IP headers are sent: use `tcpdump` or Wireshark to
      capture loopback traffic on port 3240 and verify only standard USB/IP
      opcodes appear (OP_REQ_IMPORT=0x8003, OP_REP_IMPORT=0x0003,
      CMD_SUBMIT=0x0001, RET_SUBMIT=0x0003)
- [ ] Verify struct sizes match reference: OpRepImport = 316 bytes

---

## Section B: Server Interop (easy-usb as server against reference usbip client)

### B.1 Start easy-usb server

- [ ] Run `USBIP_DOCKER_INTEROP=1 cargo test -p easy-usb-core -- --ignored interop_server_handshake`
- [ ] Verify it accepts a connection from the reference usbip client
- [ ] Run `USBIP_DOCKER_INTEROP=1 cargo test -p easy-usb-core -- --ignored interop_server_urb_echo`
- [ ] Verify the URB echo test passes

### B.2 Manual verification (alternative to automated test)

- [ ] Start the easy-usb server manually: `cargo run -p easy-usb-core --example usbipd-server` (if available)
- [ ] From another terminal: `usbip attach -r 127.0.0.1 -b 1-1`
- [ ] Verify: `usbip list -r 127.0.0.1` shows the device
- [ ] Verify: `dmesg | tail` shows the device being attached
- [ ] Detach: `usbip detach -p 0`

---

## Section C: Behavioral Parameter Validation

- [ ] Handshake timeout: Verify that a connection that sends OP_REQ_IMPORT but
      does not receive OP_REP_IMPORT within 10s causes a timeout error
- [ ] TCP keepalive: Verify TCP keepalive is configured with 10s interval
      (checked via `sysctl net.ipv4.tcp_keepalive_*` on the reference side)

---

## Section D: No Protocol Invention

- [ ] Verify opcodes match USB/IP v1.1.1 exactly:
  - `OP_REQ_IMPORT` = 0x8003
  - `OP_REP_IMPORT` = 0x0003
  - `CMD_SUBMIT` = 0x0001
  - `RET_SUBMIT` = 0x0003
- [ ] Verify no custom headers or extensions are present in the protocol codec
- [ ] Verify struct layout matches `#[repr(C)]` and field ordering matches
      Linux kernel `usbip_common.h`

---

## Section E: Acceptance Criteria Sign-Off

| AC | Description | Status |
|----|-------------|--------|
| 1  | Client interop: easy-usb imports device from reference usbipd, URB round-trip succeeds | ☐ Pass ☐ Fail |
| 2  | Server interop: reference usbip client imports device from easy-usb server | ☐ Pass ☐ Fail |
| 3  | No custom USB/IP headers or extensions present in wire traffic | ☐ Pass ☐ Fail |
| 4  | Interop test automated via Docker or manual checklist with maintainer sign-off | ☐ Pass ☐ Fail |
| 5  | This gate is done before any platform-adapter story starts | ☐ Confirmed |
| 6  | Manual sign-off gate: maintainer has signed off this checklist | ☐ Signed Off |

---

## Sign-Off

I, the undersigned maintainer, have verified the above checklist items and
confirm that the easy-usb-core protocol engine interoperates correctly with
the reference Linux usbipd server and client per USB/IP v1.1.1. No custom
headers or protocol extensions are present.

**Maintainer Name:** __________________________________

**Date:** __________________________________

**Signature:** __________________________________

**Notes / Issues Found:**
___________________________________________________________________________
___________________________________________________________________________
___________________________________________________________________________
