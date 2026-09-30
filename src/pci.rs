//! PCI (Peripheral Component Interconnect) bus: finding devices and reading their
//! configuration space.
//!
//! Every PCI device has 256 bytes of configuration space describing it: vendor and
//! device IDs, class, where its registers live (BARs), and its interrupt line. We reach
//! it through two I/O ports: write *which* device/register to 0xCF8, then read or write
//! the value at 0xCFC ("configuration mechanism #1").

use crate::port::{inl, outl};

const CONFIG_ADDRESS: u16 = 0xcf8;
const CONFIG_DATA: u16 = 0xcfc;

// Configuration space offsets we use.
const VENDOR_ID: u8 = 0x00; // u16; 0xFFFF = no device here
const DEVICE_ID: u8 = 0x02; // u16
const COMMAND: u8 = 0x04; // u16
const CLASS_INFO: u8 = 0x08; // u32: class, subclass, prog-if, revision
const HEADER_TYPE: u8 = 0x0e; // u8; bit 7 = device has several functions
const BAR0: u8 = 0x10; // u32
const INTERRUPT_LINE: u8 = 0x3c; // u8: which PIC IRQ the firmware wired the device to

// COMMAND register bits.
const COMMAND_MEMORY_SPACE: u16 = 1 << 1; // respond to memory-mapped register access
const COMMAND_BUS_MASTER: u16 = 1 << 2; // allowed to do DMA (read/write RAM by itself)

/// Where a device sits: bus (0–255), device (0–31), function (0–7).
#[derive(Clone, Copy, Debug)]
pub struct PciAddress {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciAddress {
    /// The value written to CONFIG_ADDRESS: enable bit, bus, device, function, and the
    /// register offset rounded down to 4 bytes.
    fn config_address(&self, offset: u8) -> u32 {
        (1 << 31)
            | (self.bus as u32) << 16
            | (self.device as u32) << 11
            | (self.function as u32) << 8
            | (offset as u32 & 0xfc)
    }

    pub fn read_u32(&self, offset: u8) -> u32 {
        // SAFETY: the PCI configuration ports only select/read configuration registers.
        unsafe {
            outl(CONFIG_ADDRESS, self.config_address(offset));
            inl(CONFIG_DATA)
        }
    }

    fn write_u32(&self, offset: u8, value: u32) {
        unsafe {
            outl(CONFIG_ADDRESS, self.config_address(offset));
            outl(CONFIG_DATA, value);
        }
    }

    /// Reads 16 bits by reading the surrounding 32 bits and picking the right half.
    pub fn read_u16(&self, offset: u8) -> u16 {
        (self.read_u32(offset) >> ((offset & 2) * 8)) as u16
    }

    pub fn read_u8(&self, offset: u8) -> u8 {
        (self.read_u32(offset) >> ((offset & 3) * 8)) as u8
    }

    pub fn vendor_id(&self) -> u16 {
        self.read_u16(VENDOR_ID)
    }

    pub fn device_id(&self) -> u16 {
        self.read_u16(DEVICE_ID)
    }

    /// (class, subclass), e.g. (0x02, 0x00) = network / Ethernet controller.
    pub fn class(&self) -> (u8, u8) {
        let info = self.read_u32(CLASS_INFO);
        ((info >> 24) as u8, (info >> 16) as u8)
    }

    /// Physical address of a memory-mapped BAR (low 4 bits are type flags, not address).
    pub fn bar0_memory_address(&self) -> u64 {
        (self.read_u32(BAR0) & !0xf) as u64
    }

    pub fn interrupt_line(&self) -> u8 {
        self.read_u8(INTERRUPT_LINE)
    }

    /// Lets the device answer memory accesses to its registers and do DMA.
    pub fn enable_memory_and_bus_mastering(&self) {
        // COMMAND shares its 32-bit slot with STATUS (upper half); writing 0 to STATUS
        // bits is harmless (they're cleared by writing 1).
        let command = self.read_u16(COMMAND) | COMMAND_MEMORY_SPACE | COMMAND_BUS_MASTER;
        self.write_u32(COMMAND, command as u32);
    }
}

/// Calls `visit` for every device on every bus.
pub fn for_each_device(mut visit: impl FnMut(PciAddress)) {
    for bus in 0..=255u8 {
        for device in 0..32u8 {
            let first = PciAddress { bus, device, function: 0 };
            if first.vendor_id() == 0xffff {
                continue; // nothing in this slot
            }
            let functions = if first.read_u8(HEADER_TYPE) & 0x80 != 0 { 8 } else { 1 };
            for function in 0..functions {
                let address = PciAddress { bus, device, function };
                if address.vendor_id() != 0xffff {
                    visit(address);
                }
            }
        }
    }
}

/// Finds the first device with the given vendor and device IDs.
pub fn find(vendor: u16, device: u16) -> Option<PciAddress> {
    let mut found = None;
    for_each_device(|address| {
        if found.is_none() && address.vendor_id() == vendor && address.device_id() == device {
            found = Some(address);
        }
    });
    found
}

/// A human-readable name for the classes QEMU's machine has.
pub fn class_name(class: (u8, u8)) -> &'static str {
    match class {
        (0x01, 0x01) => "IDE storage controller",
        (0x02, 0x00) => "Ethernet controller",
        (0x03, 0x00) => "VGA display controller",
        (0x06, 0x00) => "host bridge",
        (0x06, 0x01) => "ISA bridge",
        (0x06, 0x80) => "other bridge",
        _ => "unknown",
    }
}

/// Logs every device over serial.
pub fn log_devices() {
    crate::serial_println!("[pci] devices:");
    for_each_device(|address| {
        let class = address.class();
        crate::serial_println!(
            "  {:02x}:{:02x}.{}  vendor {:04x} device {:04x}  {}",
            address.bus,
            address.device,
            address.function,
            address.vendor_id(),
            address.device_id(),
            class_name(class)
        );
    });
}
