#!/usr/bin/env bash
# Boots Ignis on a private network shared with your Mac, so you can `ping` the kernel
# from Terminal. Uses QEMU's vmnet-host backend, which needs sudo: macOS only lets
# administrators create virtual network interfaces.
#
#   Mac (host)   192.168.100.1   <── vmnet ──>   Ignis kernel   192.168.100.2
#
# Then, in another Terminal tab:   ping 192.168.100.2
set -euo pipefail
cd "$(dirname "$0")/.."

# Build as your normal user (not root), with the kernel's address for this network.
IGNIS_IP=192.168.100.2 IGNIS_GATEWAY=192.168.100.1 cargo bootimage

echo
echo "Starting QEMU (needs your password for sudo)."
echo "When the kernel says 'listening', run in another tab:  ping 192.168.100.2"
echo

sudo qemu-system-x86_64 \
    -drive format=raw,file=target/x86_64-ignis/debug/bootimage-ignis.bin \
    -serial stdio \
    -display cocoa,zoom-to-fit=on \
    -netdev vmnet-host,id=net0,start-address=192.168.100.1,end-address=192.168.100.254,subnet-mask=255.255.255.0 \
    -device e1000,netdev=net0
