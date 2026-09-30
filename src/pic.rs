//! Driver for the two 8259 PICs (Programmable Interrupt Controllers).
//!
//! Hardware devices don't talk to the CPU directly; they raise an IRQ (Interrupt
//! ReQuest) line on a PIC, which forwards it to the CPU as an interrupt number.
//! Two PICs are chained: the "primary" handles IRQ 0–7, and the "secondary"
//! handles IRQ 8–15, reporting through the primary's IRQ 2.

use crate::port::{inb, outb};

const PRIMARY_COMMAND: u16 = 0x20;
const PRIMARY_DATA: u16 = 0x21;
const SECONDARY_COMMAND: u16 = 0xa0;
const SECONDARY_DATA: u16 = 0xa1;

/// By default the PICs use interrupt numbers 0–15, which clash with CPU
/// exceptions (e.g. IRQ 0 would look like a divide error). Move them to 32–47.
pub const PRIMARY_OFFSET: u8 = 32;
pub const SECONDARY_OFFSET: u8 = PRIMARY_OFFSET + 8;

/// Interrupt numbers of the IRQs we use.
pub const TIMER_VECTOR: u8 = PRIMARY_OFFSET; // IRQ 0: PIT timer
pub const KEYBOARD_VECTOR: u8 = PRIMARY_OFFSET + 1; // IRQ 1: PS/2 keyboard
/// IRQ 7 / 15 can fire "spuriously" (electrical noise, a cancelled request)
/// even while masked, so we must have handlers for them.
pub const PRIMARY_SPURIOUS_VECTOR: u8 = PRIMARY_OFFSET + 7;
pub const SECONDARY_SPURIOUS_VECTOR: u8 = SECONDARY_OFFSET + 7;

const END_OF_INTERRUPT: u8 = 0x20;
const READ_IN_SERVICE: u8 = 0x0b;

/// Old hardware needs a moment between PIC commands; writing to unused port 0x80
/// takes about a microsecond, which is a handy delay.
unsafe fn io_wait() {
    unsafe { outb(0x80, 0) };
}

/// Remaps both PICs to 32–47 and enables only the timer and keyboard IRQs.
pub fn init() {
    // SAFETY: these are the standard PIC ports, and interrupts are still disabled.
    unsafe {
        // ICW = Initialization Command Word. The PIC expects 4 of them, in order.
        // ICW1: "start initialisation; an ICW4 will follow."
        outb(PRIMARY_COMMAND, 0x11);
        io_wait();
        outb(SECONDARY_COMMAND, 0x11);
        io_wait();
        // ICW2: the first interrupt number each PIC should use.
        outb(PRIMARY_DATA, PRIMARY_OFFSET);
        io_wait();
        outb(SECONDARY_DATA, SECONDARY_OFFSET);
        io_wait();
        // ICW3: how they're wired: secondary is on primary's IRQ 2 (bit mask 0b100),
        // and the secondary's own identity is 2.
        outb(PRIMARY_DATA, 0b0000_0100);
        io_wait();
        outb(SECONDARY_DATA, 2);
        io_wait();
        // ICW4: "8086 mode" (the mode for x86 PCs).
        outb(PRIMARY_DATA, 0x01);
        io_wait();
        outb(SECONDARY_DATA, 0x01);
        io_wait();

        // Masks: a 1 bit blocks that IRQ. Allow only IRQ 0 (timer) and 1 (keyboard).
        outb(PRIMARY_DATA, 0b1111_1100);
        outb(SECONDARY_DATA, 0b1111_1111);
    }
}

/// Tells the PIC(s) we've finished handling an IRQ, so they'll send the next one.
/// Without this, the PIC waits forever and that IRQ (and lower-priority ones) stop.
pub fn end_of_interrupt(vector: u8) {
    // SAFETY: standard PIC ports; EOI only affects the PIC's in-service state.
    unsafe {
        if vector >= SECONDARY_OFFSET {
            outb(SECONDARY_COMMAND, END_OF_INTERRUPT);
        }
        outb(PRIMARY_COMMAND, END_OF_INTERRUPT);
    }
}

/// Whether an IRQ 7/15 is real: the PIC's In-Service Register (ISR) has the bit
/// set for an IRQ it actually delivered.
pub fn is_spurious(vector: u8) -> bool {
    // SAFETY: reading the ISR has no side effects beyond selecting what port reads return.
    unsafe {
        if vector == SECONDARY_SPURIOUS_VECTOR {
            outb(SECONDARY_COMMAND, READ_IN_SERVICE);
            inb(SECONDARY_COMMAND) & 0x80 == 0
        } else {
            outb(PRIMARY_COMMAND, READ_IN_SERVICE);
            inb(PRIMARY_COMMAND) & 0x80 == 0
        }
    }
}

/// A spurious IRQ 15 still counts as "delivered" by the primary PIC (through its
/// IRQ 2 cascade line), so the primary alone needs an end-of-interrupt.
pub fn end_of_spurious_secondary() {
    // SAFETY: standard PIC port.
    unsafe { outb(PRIMARY_COMMAND, END_OF_INTERRUPT) };
}
