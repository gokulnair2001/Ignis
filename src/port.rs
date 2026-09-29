//! Port-mapped I/O: wrappers around the x86 `in` and `out` instructions.
//!
//! I/O ports are a separate 16-bit address space (0x0000–0xFFFF), unrelated to memory.

use core::arch::asm;

/// Sends one byte to an I/O port.
///
/// # Safety
/// Writing to a port can reconfigure hardware; the caller must know what lives there.
pub unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

/// Reads one byte from an I/O port.
///
/// # Safety
/// Reading some ports has side effects (e.g. taking a byte out of a receive queue).
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}
