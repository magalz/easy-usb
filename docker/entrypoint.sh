#!/bin/bash
set -e
modprobe usbip_host || true
modprobe vhci_hcd || true
# usbip_test module — may not exist on all kernels; fall back to dummy
modprobe usbip_test 2>/dev/null || echo "usbip_test not available, using dummy device"

# Create a virtual USB device via dummy_hcd if usbip_test isn't available
if ! ls /sys/bus/platform/drivers/vhci_hcd/attach 2>/dev/null; then
    modprobe dummy_hcd 2>/dev/null || true
    echo "1-1" > /sys/bus/platform/drivers/dummy_hcd/attach 2>/dev/null || true
else
    echo "1-1" > /sys/bus/platform/drivers/vhci_hcd/attach 2>/dev/null || true
fi

usbipd -D
# Keep container alive
tail -f /dev/null
