//! The PIT (Programmable Interval Timer): a chip that raises IRQ 0 at a steady rate.
//! Each "tick" is counted here, and once a second the uptime is shown in the status bar.

use crate::port::outb;
use crate::vga_buffer::WRITER;
use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

/// The PIT's input clock: 1.193182 MHz, a frequency inherited from the original IBM PC.
const PIT_BASE_FREQUENCY: u32 = 1_193_182;
pub const TICKS_PER_SECOND: u32 = 100;

const PIT_CHANNEL0_DATA: u16 = 0x40;
const PIT_COMMAND: u16 = 0x43;

/// Ticks since boot. Atomic, so it can be updated safely from the interrupt handler.
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Programs PIT channel 0 to fire `TICKS_PER_SECOND` times a second.
pub fn init() {
    // The PIT counts down from `divisor` at 1.19 MHz and fires each time it hits 0.
    let divisor = (PIT_BASE_FREQUENCY / TICKS_PER_SECOND) as u16; // 11931 → ~100 Hz

    // SAFETY: standard PIT ports; interrupts are still disabled.
    unsafe {
        // 0x36 = channel 0, send low byte then high byte, mode 3 (square wave), binary.
        outb(PIT_COMMAND, 0x36);
        outb(PIT_CHANNEL0_DATA, (divisor & 0xff) as u8);
        outb(PIT_CHANNEL0_DATA, (divisor >> 8) as u8);
    }
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Called from the timer interrupt handler on every tick.
pub fn on_tick() {
    let ticks = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
    if ticks % TICKS_PER_SECOND as u64 == 0 {
        draw_status_bar(ticks);
    }
}

pub fn draw_status_bar(ticks: u64) {
    let seconds = ticks / TICKS_PER_SECOND as u64;
    let mut text = StackString::<80>::new();
    let _ = write!(
        text,
        " Ignis | uptime {:02}:{:02}:{:02} | {} timer ticks | type something!",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        ticks,
    );
    WRITER.lock().write_status_bar(text.as_str());
}

/// A tiny fixed-size text buffer, since we have no heap (`String`) yet.
struct StackString<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> StackString<N> {
    fn new() -> Self {
        StackString { bytes: [0; N], len: 0 }
    }

    fn as_str(&self) -> &str {
        // Only whole `&str`s are ever copied in, so the contents are valid UTF-8.
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> Write for StackString<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.len + s.len();
        if end > N {
            return Err(core::fmt::Error); // too long: stop instead of overflowing
        }
        self.bytes[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}
