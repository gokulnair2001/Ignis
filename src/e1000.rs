//! Driver for the Intel 82540EM Gigabit Ethernet card ("e1000"), emulated by QEMU.
//!
//! The card is controlled through memory-mapped registers (found via PCI BAR0), and it
//! moves packets with DMA: we give it rings of descriptors pointing at buffers in RAM,
//! and it reads outgoing packets from / writes incoming packets into those buffers
//! by itself. Register names follow Intel's "PCI/PCI-X Family of Gigabit Ethernet
//! Controllers Software Developer's Manual".

use crate::frame_allocator::{self, FRAME_SIZE};
use crate::paging::{self, NO_CACHE, WRITABLE, WRITE_THROUGH};
use crate::pci::{self, PciAddress};
use crate::{interrupts, serial_println};
use core::ptr::{read_volatile, write_volatile};
use spin::Mutex;

pub const VENDOR_INTEL: u16 = 0x8086;
pub const DEVICE_82540EM: u16 = 0x100e;

// Register offsets (bytes from the start of the register area).
const CTRL: usize = 0x0000; // device control
const STATUS: usize = 0x0008; // device status
const ICR: usize = 0x00c0; // interrupt cause (reading it clears it)
const IMS: usize = 0x00d0; // interrupt mask set: which causes raise an interrupt
const IMC: usize = 0x00d8; // interrupt mask clear
const RCTL: usize = 0x0100; // receive control
const TCTL: usize = 0x0400; // transmit control
const TIPG: usize = 0x0410; // transmit inter-packet gap
const RDBAL: usize = 0x2800; // receive descriptor ring: base address (low 32 bits)
const RDBAH: usize = 0x2804; //   ...(high 32 bits)
const RDLEN: usize = 0x2808; //   ring size in bytes
const RDH: usize = 0x2810; //   head: next descriptor the card will fill
const RDT: usize = 0x2818; //   tail: last descriptor we've handed to the card
const TDBAL: usize = 0x3800; // transmit descriptor ring, same idea
const TDBAH: usize = 0x3804;
const TDLEN: usize = 0x3808;
const TDH: usize = 0x3810;
const TDT: usize = 0x3818;
const MTA: usize = 0x5200; // multicast table (128 × u32)
const RAL0: usize = 0x5400; // receive address (our MAC), low 4 bytes
const RAH0: usize = 0x5404; //   ...high 2 bytes

// CTRL bits.
const CTRL_AUTO_SPEED: u32 = 1 << 5; // ASDE: detect link speed automatically
const CTRL_SET_LINK_UP: u32 = 1 << 6; // SLU
const CTRL_RESET: u32 = 1 << 26; // RST: reset the card (self-clearing)

const STATUS_LINK_UP: u32 = 1 << 1;

// RCTL bits.
const RCTL_ENABLE: u32 = 1 << 1;
const RCTL_BROADCAST_ACCEPT: u32 = 1 << 15; // BAM: needed for ARP requests
const RCTL_STRIP_CRC: u32 = 1 << 26; // SECRC: don't give us the 4-byte Ethernet checksum
// Buffer size bits 16–17 left at 0 = 2048-byte receive buffers.

// TCTL bits.
const TCTL_ENABLE: u32 = 1 << 1;
const TCTL_PAD_SHORT: u32 = 1 << 3; // PSP: pad frames shorter than 64 bytes
const TCTL_COLLISION_THRESHOLD: u32 = 0x10 << 4; // CT: recommended value
const TCTL_COLLISION_DISTANCE: u32 = 0x40 << 12; // COLD: recommended for full duplex

// Interrupt causes.
const INT_LINK_CHANGE: u32 = 1 << 2; // LSC
const INT_RX_LOW: u32 = 1 << 4; // RXDMT0: running low on free receive descriptors
const INT_RX_OVERRUN: u32 = 1 << 6; // RXO
const INT_RX_DONE: u32 = 1 << 7; // RXT0: a packet arrived

// Descriptor status / command bits.
const DESC_DONE: u8 = 1 << 0; // DD: the card has finished with this descriptor
const RX_END_OF_PACKET: u8 = 1 << 1; // EOP
const TX_CMD_END_OF_PACKET: u8 = 1 << 0; // EOP
const TX_CMD_INSERT_CRC: u8 = 1 << 1; // IFCS: card appends the Ethernet checksum
const TX_CMD_REPORT_STATUS: u8 = 1 << 3; // RS: set DESC_DONE when sent

const RING_SIZE: usize = 32; // descriptors per ring (32 × 16 bytes fits in one frame)
const BUFFER_SIZE: usize = 2048;
const BUFFERS_PER_FRAME: usize = FRAME_SIZE as usize / BUFFER_SIZE;

/// Virtual address where we map the card's 128 KiB of registers.
const MMIO_VIRTUAL: u64 = 0x6666_0000_0000;
const MMIO_SIZE: u64 = 128 * 1024;

/// Receive descriptor, laid out exactly as the card reads/writes it (16 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct RxDescriptor {
    buffer_address: u64, // physical address of a 2 KiB buffer
    length: u16,         // bytes the card wrote
    checksum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

/// Transmit descriptor (16 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct TxDescriptor {
    buffer_address: u64,
    length: u16,
    checksum_offset: u8,
    command: u8,
    status: u8,
    checksum_start: u8,
    special: u16,
}

const _: () = assert!(core::mem::size_of::<RxDescriptor>() == 16);
const _: () = assert!(core::mem::size_of::<TxDescriptor>() == 16);

pub struct E1000 {
    registers: *mut u8,
    rx_ring: *mut RxDescriptor,
    tx_ring: *mut TxDescriptor,
    /// Virtual addresses of each descriptor's buffer.
    rx_buffers: [*mut u8; RING_SIZE],
    tx_buffers: [*mut u8; RING_SIZE],
    rx_next: usize,
    pub mac: [u8; 6],
    pub irq: u8,
}

// SAFETY: the raw pointers point at device memory and DMA frames owned by this driver;
// all access goes through the `NIC` mutex.
unsafe impl Send for E1000 {}

pub static NIC: Mutex<Option<E1000>> = Mutex::new(None);

#[derive(Debug)]
#[allow(dead_code)] // the details are only ever shown via `{:?}`
pub enum InitError {
    NotFound,
    OutOfMemory,
    Mapping(paging::MapError),
}

impl E1000 {
    fn read(&self, register: usize) -> u32 {
        unsafe { read_volatile(self.registers.add(register) as *const u32) }
    }

    fn write(&self, register: usize, value: u32) {
        unsafe { write_volatile(self.registers.add(register) as *mut u32, value) }
    }

    pub fn link_up(&self) -> bool {
        self.read(STATUS) & STATUS_LINK_UP != 0
    }

    /// Copies the next received packet (if any) into `out` and returns its length,
    /// then hands the descriptor back to the card.
    pub fn receive(&mut self, out: &mut [u8]) -> Option<usize> {
        let index = self.rx_next;
        let descriptor = unsafe { read_volatile(self.rx_ring.add(index)) };
        if descriptor.status & DESC_DONE == 0 {
            return None; // the card hasn't filled this one yet
        }

        let mut length = 0;
        // We only accept packets that fit in one buffer (no jumbo frames).
        if descriptor.status & RX_END_OF_PACKET != 0 && descriptor.errors == 0 {
            length = (descriptor.length as usize).min(out.len());
            unsafe { core::ptr::copy_nonoverlapping(self.rx_buffers[index], out.as_mut_ptr(), length) };
        }

        // Clear the status and give the descriptor back by moving the tail onto it.
        unsafe { write_volatile(&mut (*self.rx_ring.add(index)).status, 0) };
        self.write(RDT, index as u32);
        self.rx_next = (index + 1) % RING_SIZE;
        Some(length) // 0 = a bad packet was dropped; keep calling
    }

    /// Queues one Ethernet frame for sending. Returns false if the ring is full.
    pub fn transmit(&mut self, frame: &[u8]) -> bool {
        let index = self.read(TDT) as usize;
        let descriptor = unsafe { &mut *self.tx_ring.add(index) };
        if unsafe { read_volatile(&descriptor.status) } & DESC_DONE == 0 {
            return false; // the card is still sending what we put here last time
        }
        let length = frame.len().min(BUFFER_SIZE);
        unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), self.tx_buffers[index], length) };
        unsafe {
            write_volatile(&mut descriptor.length, length as u16);
            write_volatile(&mut descriptor.command, TX_CMD_END_OF_PACKET | TX_CMD_INSERT_CRC | TX_CMD_REPORT_STATUS);
            write_volatile(&mut descriptor.status, 0);
        }
        // Moving the tail past this descriptor tells the card "send it".
        self.write(TDT, ((index + 1) % RING_SIZE) as u32);
        true
    }
}

/// Allocates one zeroed physical frame for DMA; returns (physical, virtual) addresses.
fn dma_frame() -> Result<(u64, *mut u8), InitError> {
    let physical = frame_allocator::allocate_frame().ok_or(InitError::OutOfMemory)?;
    let virtual_address = paging::phys_to_virt(physical);
    unsafe { core::ptr::write_bytes(virtual_address, 0, FRAME_SIZE as usize) };
    Ok((physical, virtual_address))
}

/// Allocates buffers for a ring: fills in each descriptor's physical buffer address
/// via `set_address`, and returns the buffers' virtual addresses.
fn allocate_buffers(mut set_address: impl FnMut(usize, u64)) -> Result<[*mut u8; RING_SIZE], InitError> {
    let mut buffers = [core::ptr::null_mut(); RING_SIZE];
    for first in (0..RING_SIZE).step_by(BUFFERS_PER_FRAME) {
        let (physical, virtual_address) = dma_frame()?;
        for part in 0..BUFFERS_PER_FRAME {
            let offset = part * BUFFER_SIZE;
            set_address(first + part, physical + offset as u64);
            buffers[first + part] = unsafe { virtual_address.add(offset) };
        }
    }
    Ok(buffers)
}

/// Finds the card, maps its registers, sets up both rings, and enables its interrupt.
pub fn init(on_interrupt: fn()) -> Result<(), InitError> {
    let address: PciAddress = pci::find(VENDOR_INTEL, DEVICE_82540EM).ok_or(InitError::NotFound)?;
    address.enable_memory_and_bus_mastering();

    // Map the register area, uncached: every access must reach the device.
    let physical = address.bar0_memory_address();
    let mut offset = 0;
    while offset < MMIO_SIZE {
        paging::map_page(MMIO_VIRTUAL + offset, physical + offset, WRITABLE | NO_CACHE | WRITE_THROUGH)
            .map_err(InitError::Mapping)?;
        offset += FRAME_SIZE;
    }
    serial_println!("[e1000] PCI {:02x}:{:02x}.{}, registers at phys {:#x}, IRQ {}",
        address.bus, address.device, address.function, physical, address.interrupt_line());

    let (rx_ring_physical, rx_ring) = dma_frame()?;
    let (tx_ring_physical, tx_ring) = dma_frame()?;
    let rx_ring = rx_ring as *mut RxDescriptor;
    let tx_ring = tx_ring as *mut TxDescriptor;

    let rx_buffers = allocate_buffers(|i, buffer| unsafe { (*rx_ring.add(i)).buffer_address = buffer })?;
    let tx_buffers = allocate_buffers(|i, buffer| unsafe {
        (*tx_ring.add(i)).buffer_address = buffer;
        (*tx_ring.add(i)).status = DESC_DONE; // "free": nothing pending
    })?;

    let mut nic = E1000 {
        registers: MMIO_VIRTUAL as *mut u8,
        rx_ring,
        tx_ring,
        rx_buffers,
        tx_buffers,
        rx_next: 0,
        mac: [0; 6],
        irq: address.interrupt_line(),
    };

    // 1. Reset the card, and wait for the reset bit to clear itself.
    nic.write(IMC, u32::MAX); // no interrupts during setup
    nic.write(CTRL, nic.read(CTRL) | CTRL_RESET);
    while nic.read(CTRL) & CTRL_RESET != 0 {
        core::hint::spin_loop();
    }
    nic.write(IMC, u32::MAX);
    nic.read(ICR); // clear anything pending

    // 2. Bring the link up.
    nic.write(CTRL, nic.read(CTRL) | CTRL_SET_LINK_UP | CTRL_AUTO_SPEED);

    // 3. Our MAC address: loaded into RAL0/RAH0 from the card's EEPROM at reset.
    let low = nic.read(RAL0);
    let high = nic.read(RAH0);
    nic.mac = [low as u8, (low >> 8) as u8, (low >> 16) as u8, (low >> 24) as u8, high as u8, (high >> 8) as u8];

    // 4. Don't join any multicast groups.
    for i in 0..128 {
        nic.write(MTA + i * 4, 0);
    }

    // 5. Receive ring: base, size, head = 0, tail = last. All descriptors belong to the card.
    let ring_bytes = (RING_SIZE * 16) as u32;
    nic.write(RDBAL, rx_ring_physical as u32);
    nic.write(RDBAH, (rx_ring_physical >> 32) as u32);
    nic.write(RDLEN, ring_bytes);
    nic.write(RDH, 0);
    nic.write(RDT, (RING_SIZE - 1) as u32);
    nic.write(RCTL, RCTL_ENABLE | RCTL_BROADCAST_ACCEPT | RCTL_STRIP_CRC);

    // 6. Transmit ring: head = tail = 0 means "nothing to send".
    nic.write(TDBAL, tx_ring_physical as u32);
    nic.write(TDBAH, (tx_ring_physical >> 32) as u32);
    nic.write(TDLEN, ring_bytes);
    nic.write(TDH, 0);
    nic.write(TDT, 0);
    nic.write(TCTL, TCTL_ENABLE | TCTL_PAD_SHORT | TCTL_COLLISION_THRESHOLD | TCTL_COLLISION_DISTANCE);
    nic.write(TIPG, 10 | (8 << 10) | (6 << 20)); // recommended gap timings

    // 7. Interrupts: packet received, receive ring low/overrun, link change.
    let irq = nic.irq;
    interrupts::without_interrupts(|| *NIC.lock() = Some(nic));
    interrupts::register_irq_handler(irq, on_interrupt);
    interrupts::without_interrupts(|| {
        if let Some(nic) = NIC.lock().as_ref() {
            nic.write(IMS, INT_RX_DONE | INT_RX_LOW | INT_RX_OVERRUN | INT_LINK_CHANGE);
            nic.read(ICR);
        }
    });
    Ok(())
}

/// Called from the IRQ handler: reads (and so clears) the interrupt causes.
pub fn acknowledge_interrupt() -> u32 {
    interrupts::without_interrupts(|| NIC.lock().as_ref().map_or(0, |nic| nic.read(ICR)))
}
