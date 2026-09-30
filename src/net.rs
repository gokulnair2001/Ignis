//! A tiny network stack: Ethernet, ARP, IPv4 and ICMP echo (ping).
//!
//! Each layer wraps the next, like envelopes inside envelopes:
//!
//!   Ethernet [dst MAC | src MAC | type] → ARP  (who has this IP?)
//!                                       → IPv4 [src IP | dst IP | protocol | checksum]
//!                                              → ICMP [type | code | checksum | id | seq | data]
//!
//! All multi-byte header fields are big-endian ("network byte order").

use crate::e1000::{self, NIC};
use crate::interrupts::without_interrupts;
use crate::{println, serial_println, timer};
use core::fmt;
use core::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use spin::Mutex;

pub type Mac = [u8; 6];
pub type Ip = [u8; 4];

const BROADCAST_MAC: Mac = [0xff; 6];
const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;
const ARP_REQUEST: u16 = 1;
const ARP_REPLY: u16 = 2;
const IP_PROTOCOL_ICMP: u8 = 1;
const ICMP_ECHO_REPLY: u8 = 0;
const ICMP_ECHO_REQUEST: u8 = 8;

const ETHERNET_HEADER: usize = 14;
const ARP_PACKET: usize = 28;
const IPV4_HEADER: usize = 20;
const ICMP_HEADER: usize = 8;
const MAX_FRAME: usize = 1514;

/// Identifies *our* pings, so we recognise their replies.
const PING_IDENTIFIER: u16 = 0x1905;
const PING_PAYLOAD: &[u8] = b"Ignis kernel says hello over ICMP";

#[derive(Clone, Copy)]
struct Config {
    mac: Mac,
    ip: Ip,
}

static CONFIG: Mutex<Option<Config>> = Mutex::new(None);

/// A small ARP cache: IP → MAC for machines we've heard from.
static ARP_CACHE: Mutex<[Option<(Ip, Mac)>; 8]> = Mutex::new([None; 8]);

static IP_PACKET_ID: AtomicU16 = AtomicU16::new(1);
static LAST_PING_REPLY: AtomicU16 = AtomicU16::new(0);
pub static FRAMES_RECEIVED: AtomicU64 = AtomicU64::new(0);
pub static FRAMES_SENT: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

/// Our IP address, chosen at build time (`IGNIS_IP=...`); QEMU's user network uses 10.0.2.15.
pub fn configured_ip() -> Ip {
    parse_ip(option_env!("IGNIS_IP").unwrap_or("10.0.2.15")).expect("IGNIS_IP is not a valid IPv4 address")
}

/// The machine to ping at boot (`IGNIS_GATEWAY=...`); 10.0.2.2 is QEMU's virtual router.
pub fn configured_gateway() -> Ip {
    parse_ip(option_env!("IGNIS_GATEWAY").unwrap_or("10.0.2.2")).expect("IGNIS_GATEWAY is not a valid IPv4 address")
}

pub fn init() -> Result<(Mac, Ip), e1000::InitError> {
    e1000::init(on_nic_interrupt)?;
    let mac = without_interrupts(|| NIC.lock().as_ref().map(|nic| nic.mac)).unwrap();
    let ip = configured_ip();
    without_interrupts(|| *CONFIG.lock() = Some(Config { mac, ip }));
    Ok((mac, ip))
}

fn config() -> Option<Config> {
    without_interrupts(|| *CONFIG.lock())
}

pub fn link_up() -> bool {
    without_interrupts(|| NIC.lock().as_ref().is_some_and(|nic| nic.link_up()))
}

// ---------------------------------------------------------------------------
// Receiving
// ---------------------------------------------------------------------------

/// Runs (inside the IRQ handler) whenever the card raises its interrupt.
fn on_nic_interrupt() {
    e1000::acknowledge_interrupt();
    let mut buffer = [0u8; 2048];
    // Take packets out one at a time. The NIC lock is released between packets,
    // because handling one may need to send a reply (which takes the lock again).
    while let Some(length) = without_interrupts(|| NIC.lock().as_mut()?.receive(&mut buffer)) {
        if length > 0 {
            FRAMES_RECEIVED.fetch_add(1, Ordering::Relaxed);
            handle_frame(&buffer[..length]);
        }
    }
}

fn handle_frame(frame: &[u8]) {
    if frame.len() < ETHERNET_HEADER {
        return;
    }
    match read_u16(frame, 12) {
        ETHERTYPE_ARP => handle_arp(&frame[ETHERNET_HEADER..]),
        ETHERTYPE_IPV4 => handle_ipv4(frame),
        _ => {} // IPv6 and friends: not supported
    }
}

fn handle_arp(packet: &[u8]) {
    let Some(config) = config() else { return };
    // Only Ethernet (1) + IPv4 (0x0800) ARP.
    if packet.len() < ARP_PACKET || read_u16(packet, 0) != 1 || read_u16(packet, 2) != ETHERTYPE_IPV4 {
        return;
    }
    let operation = read_u16(packet, 6);
    let sender_mac: Mac = packet[8..14].try_into().unwrap();
    let sender_ip: Ip = packet[14..18].try_into().unwrap();
    let target_ip: Ip = packet[24..28].try_into().unwrap();

    // Whoever sent this is on our network: remember their MAC.
    arp_remember(sender_ip, sender_mac);

    if operation == ARP_REQUEST && target_ip == config.ip {
        serial_println!("[net] ARP: {} asked who has {}; replying with our MAC", IpAddr(sender_ip), IpAddr(config.ip));
        send_arp(ARP_REPLY, sender_mac, sender_ip, sender_mac);
    }
}

fn handle_ipv4(frame: &[u8]) {
    let Some(config) = config() else { return };
    let ip = &frame[ETHERNET_HEADER..];
    if ip.len() < IPV4_HEADER || ip[0] >> 4 != 4 {
        return; // not IPv4
    }
    let header_length = (ip[0] & 0xf) as usize * 4;
    let total_length = read_u16(ip, 2) as usize;
    if header_length < IPV4_HEADER || total_length > ip.len() || total_length < header_length {
        return; // malformed
    }
    if checksum(&ip[..header_length]) != 0 {
        return; // corrupted header
    }
    let source: Ip = ip[12..16].try_into().unwrap();
    let destination: Ip = ip[16..20].try_into().unwrap();
    if destination != config.ip || ip[9] != IP_PROTOCOL_ICMP {
        return;
    }

    let icmp = &ip[header_length..total_length];
    if icmp.len() < ICMP_HEADER || checksum(icmp) != 0 {
        return;
    }
    let identifier = read_u16(icmp, 4);
    let sequence = read_u16(icmp, 6);
    let sender_mac: Mac = frame[6..12].try_into().unwrap();

    match icmp[0] {
        ICMP_ECHO_REQUEST => {
            arp_remember(source, sender_mac);
            // Echo it back: same identifier, sequence and data, type = reply.
            send_icmp(sender_mac, source, ICMP_ECHO_REPLY, identifier, sequence, &icmp[ICMP_HEADER..]);
            println!("[net] ping from {} seq={} -> replied", IpAddr(source), sequence);
            serial_println!("[net] echo request from {} seq={} ({} data bytes), replied", IpAddr(source), sequence, icmp.len() - ICMP_HEADER);
        }
        ICMP_ECHO_REPLY if identifier == PING_IDENTIFIER => {
            LAST_PING_REPLY.store(sequence, Ordering::Release);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------

fn transmit(frame: &[u8]) -> bool {
    let sent = without_interrupts(|| NIC.lock().as_mut().is_some_and(|nic| nic.transmit(frame)));
    if sent {
        FRAMES_SENT.fetch_add(1, Ordering::Relaxed);
    }
    sent
}

/// Writes an Ethernet header into `frame` and returns the offset where the payload starts.
fn write_ethernet_header(frame: &mut [u8], destination: Mac, source: Mac, ethertype: u16) -> usize {
    frame[0..6].copy_from_slice(&destination);
    frame[6..12].copy_from_slice(&source);
    write_u16(frame, 12, ethertype);
    ETHERNET_HEADER
}

fn send_arp(operation: u16, destination_mac: Mac, target_ip: Ip, target_mac: Mac) {
    let Some(config) = config() else { return };
    let mut frame = [0u8; ETHERNET_HEADER + ARP_PACKET];
    let arp_start = write_ethernet_header(&mut frame, destination_mac, config.mac, ETHERTYPE_ARP);
    let arp = &mut frame[arp_start..];
    write_u16(arp, 0, 1); // hardware type: Ethernet
    write_u16(arp, 2, ETHERTYPE_IPV4); // protocol type: IPv4
    arp[4] = 6; // MAC length
    arp[5] = 4; // IP length
    write_u16(arp, 6, operation);
    arp[8..14].copy_from_slice(&config.mac);
    arp[14..18].copy_from_slice(&config.ip);
    arp[18..24].copy_from_slice(&target_mac);
    arp[24..28].copy_from_slice(&target_ip);
    transmit(&frame);
}

fn send_icmp(destination_mac: Mac, destination_ip: Ip, icmp_type: u8, identifier: u16, sequence: u16, data: &[u8]) {
    let Some(config) = config() else { return };
    let data = &data[..data.len().min(MAX_FRAME - ETHERNET_HEADER - IPV4_HEADER - ICMP_HEADER)];
    let icmp_length = ICMP_HEADER + data.len();
    let ip_length = IPV4_HEADER + icmp_length;
    let mut frame = [0u8; MAX_FRAME];
    let ip_start = write_ethernet_header(&mut frame, destination_mac, config.mac, ETHERTYPE_IPV4);

    // IPv4 header.
    let ip = &mut frame[ip_start..ip_start + ip_length];
    ip[0] = 0x45; // version 4, header length 5 × 4 = 20 bytes
    write_u16(ip, 2, ip_length as u16);
    write_u16(ip, 4, IP_PACKET_ID.fetch_add(1, Ordering::Relaxed));
    ip[8] = 64; // TTL (time to live): max router hops
    ip[9] = IP_PROTOCOL_ICMP;
    ip[12..16].copy_from_slice(&config.ip);
    ip[16..20].copy_from_slice(&destination_ip);
    let header_checksum = checksum(&ip[..IPV4_HEADER]);
    write_u16(ip, 10, header_checksum);

    // ICMP message.
    let icmp = &mut ip[IPV4_HEADER..];
    icmp[0] = icmp_type;
    write_u16(icmp, 4, identifier);
    write_u16(icmp, 6, sequence);
    icmp[ICMP_HEADER..].copy_from_slice(data);
    let icmp_checksum = checksum(icmp);
    write_u16(icmp, 2, icmp_checksum);

    transmit(&frame[..ip_start + ip_length]);
}

// ---------------------------------------------------------------------------
// ARP cache and pinging
// ---------------------------------------------------------------------------

fn arp_remember(ip: Ip, mac: Mac) {
    without_interrupts(|| {
        let mut cache = ARP_CACHE.lock();
        if let Some(entry) = cache.iter_mut().flatten().find(|(known, _)| *known == ip) {
            entry.1 = mac;
        } else if let Some(slot) = cache.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some((ip, mac));
        } else {
            cache[0] = Some((ip, mac)); // full: overwrite the oldest-ish entry
        }
    });
}

fn arp_lookup(ip: Ip) -> Option<Mac> {
    without_interrupts(|| ARP_CACHE.lock().iter().flatten().find(|(known, _)| *known == ip).map(|(_, mac)| *mac))
}

/// Sleeps until `done()` is true or `timeout_ticks` pass. Returns whether it became true.
fn wait_until(timeout_ticks: u64, done: impl Fn() -> bool) -> bool {
    let deadline = timer::ticks() + timeout_ticks;
    while !done() {
        if timer::ticks() >= deadline {
            return false;
        }
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
    true
}

/// Finds the MAC for `ip`, asking the network with a broadcast ARP request if needed.
/// ARP packets can get lost, so we ask up to 3 times, waiting a second each time.
/// (QEMU's e1000 also holds received packets for ~1 s after the receiver is enabled.)
pub fn resolve(ip: Ip) -> Option<Mac> {
    for _attempt in 0..3 {
        if let Some(mac) = arp_lookup(ip) {
            return Some(mac);
        }
        send_arp(ARP_REQUEST, BROADCAST_MAC, ip, [0; 6]);
        wait_until(timer::TICKS_PER_SECOND as u64, || arp_lookup(ip).is_some());
    }
    arp_lookup(ip)
}

/// Sends one echo request and waits up to a second for the reply.
/// Returns the round-trip time in timer ticks (10 ms each), or None on timeout.
pub fn ping(ip: Ip, sequence: u16) -> Option<u64> {
    let mac = resolve(ip)?;
    let sent_at = timer::ticks();
    send_icmp(mac, ip, ICMP_ECHO_REQUEST, PING_IDENTIFIER, sequence, PING_PAYLOAD);
    let replied = wait_until(timer::TICKS_PER_SECOND as u64, || LAST_PING_REPLY.load(Ordering::Acquire) == sequence);
    replied.then(|| timer::ticks() - sent_at)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

/// The Internet checksum: add up the data as 16-bit numbers, fold any overflow back
/// in, and flip all the bits. Checking a packet that includes its own checksum gives 0.
fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for chunk in data.chunks(2) {
        let word = if chunk.len() == 2 { u16::from_be_bytes([chunk[0], chunk[1]]) } else { (chunk[0] as u16) << 8 };
        sum += word as u32;
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn parse_ip(text: &str) -> Option<Ip> {
    let mut ip = [0u8; 4];
    let mut parts = text.split('.');
    for byte in &mut ip {
        *byte = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(ip)
}

/// Displays an IP as `10.0.2.15`.
pub struct IpAddr(pub Ip);

impl fmt::Display for IpAddr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

/// Displays a MAC as `52:54:00:12:34:56`.
pub struct MacAddr(pub Mac);

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let m = self.0;
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
    }
}
