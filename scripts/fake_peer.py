#!/usr/bin/env python3
"""Pretend to be a second computer on Ignis's network, and ping the kernel.

QEMU's `dgram` network backend sends every Ethernet frame the kernel transmits to
this script as a UDP datagram, and delivers frames we send back to the kernel's
network card. So this script can play "another machine" without root access:

  - it answers ARP requests and pings for its own address (10.0.2.2), so the
    kernel's boot-time ping of the gateway succeeds, and
  - it asks "who has 10.0.2.15?" (ARP), then sends echo requests (ping) and checks
    that the kernel's echo replies are correct.

Start this first, then QEMU with:
  -netdev dgram,id=net0,local.type=inet,local.host=127.0.0.1,local.port=5555,
          remote.type=inet,remote.host=127.0.0.1,remote.port=5556
  -device e1000,netdev=net0
"""

import socket
import struct
import sys
import time

PEER_MAC = bytes.fromhex("525400aabbcc")
PEER_IP = bytes([10, 0, 2, 2])
KERNEL_IP = bytes([10, 0, 2, 15])
BROADCAST = b"\xff" * 6
PINGS = 4
ICMP_ID = 0xBEEF


def checksum(data: bytes) -> int:
    if len(data) % 2:
        data += b"\0"
    total = sum(struct.unpack(f"!{len(data) // 2}H", data))
    while total > 0xFFFF:
        total = (total & 0xFFFF) + (total >> 16)
    return ~total & 0xFFFF


def ethernet(dst: bytes, ethertype: int, payload: bytes) -> bytes:
    return dst + PEER_MAC + struct.pack("!H", ethertype) + payload


def arp(op: int, target_mac: bytes, target_ip: bytes) -> bytes:
    return struct.pack("!HHBBH", 1, 0x0800, 6, 4, op) + PEER_MAC + PEER_IP + target_mac + target_ip


def ipv4_icmp(dst_ip: bytes, icmp: bytes) -> bytes:
    header = struct.pack("!BBHHHBBH4s4s", 0x45, 0, 20 + len(icmp), 1, 0, 64, 1, 0, PEER_IP, dst_ip)
    header = header[:10] + struct.pack("!H", checksum(header)) + header[12:]
    return header + icmp


def icmp_echo(kind: int, ident: int, seq: int, data: bytes) -> bytes:
    body = struct.pack("!BBHHH", kind, 0, 0, ident, seq) + data
    return body[:2] + struct.pack("!H", checksum(body)) + body[4:]


def main() -> int:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 5556))
    qemu = ("127.0.0.1", 5555)
    sock.settimeout(0.2)

    kernel_mac = None
    replies = {}
    sent_at = {}
    next_action = time.time() + 1.0
    deadline = time.time() + 60
    seq = 0
    print("fake peer: waiting for the kernel...", flush=True)

    while time.time() < deadline:
        try:
            frame, _ = sock.recvfrom(2048)
        except socket.timeout:
            frame = None

        if frame and len(frame) >= 14:
            ethertype = struct.unpack("!H", frame[12:14])[0]
            payload = frame[14:]
            if ethertype == 0x0806 and len(payload) >= 28:
                op = struct.unpack("!H", payload[6:8])[0]
                sender_mac, sender_ip, target_ip = payload[8:14], payload[14:18], payload[24:28]
                if op == 1 and target_ip == PEER_IP:  # kernel asks who we are
                    sock.sendto(ethernet(sender_mac, 0x0806, arp(2, sender_mac, sender_ip)), qemu)
                elif op == 2 and sender_ip == KERNEL_IP and kernel_mac is None:
                    kernel_mac = sender_mac
                    print(f"fake peer: ARP reply: 10.0.2.15 is-at {kernel_mac.hex(':')}", flush=True)
            elif ethertype == 0x0800 and len(payload) >= 28 and payload[9] == 1:
                ihl = (payload[0] & 0xF) * 4
                total = struct.unpack("!H", payload[2:4])[0]
                ip_ok = checksum(payload[:ihl]) == 0
                icmp = payload[ihl:total]
                icmp_ok = checksum(icmp) == 0
                kind, _, _, ident, s = struct.unpack("!BBHHH", icmp[:8])
                if kind == 8 and payload[16:20] == PEER_IP:  # kernel pings us
                    reply = icmp_echo(0, ident, s, icmp[8:])
                    sock.sendto(ethernet(frame[6:12], 0x0800, ipv4_icmp(payload[12:16], reply)), qemu)
                elif kind == 0 and ident == ICMP_ID:
                    rtt = (time.time() - sent_at.get(s, time.time())) * 1000
                    good = ip_ok and icmp_ok and icmp[8:] == f"hello Ignis #{s}".encode()
                    replies[s] = good
                    print(f"fake peer: echo reply seq={s} rtt={rtt:.1f}ms checksums+data {'OK' if good else 'BAD'}", flush=True)

        if time.time() >= next_action:
            next_action = time.time() + 1.0
            if kernel_mac is None:
                sock.sendto(ethernet(BROADCAST, 0x0806, arp(1, b"\0" * 6, KERNEL_IP)), qemu)
            elif seq < PINGS:
                seq += 1
                data = f"hello Ignis #{seq}".encode()
                sent_at[seq] = time.time()
                packet = ipv4_icmp(KERNEL_IP, icmp_echo(8, ICMP_ID, seq, data))
                sock.sendto(ethernet(kernel_mac, 0x0800, packet), qemu)
            elif len(replies) == PINGS or time.time() - sent_at[PINGS] > 2:
                break

    good = sum(replies.values())
    print(f"fake peer: {good}/{PINGS} valid echo replies", flush=True)
    return 0 if good == PINGS else 1


if __name__ == "__main__":
    sys.exit(main())
