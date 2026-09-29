//! Driver for the 16550 UART serial port. QEMU forwards COM1 to the host terminal
//! (`-serial stdio`), which gives us scrollable, copyable log output.

use crate::port::{inb, outb};
use core::fmt;
use spin::{LazyLock, Mutex};

/// COM1's registers start at this I/O port.
const COM1: u16 = 0x3f8;

// Register offsets from the base port.
const DATA: u16 = 0;
const INTERRUPT_ENABLE: u16 = 1;
const FIFO_CONTROL: u16 = 2;
const LINE_CONTROL: u16 = 3;
const MODEM_CONTROL: u16 = 4;
const LINE_STATUS: u16 = 5;

/// Line Status bit 5: the transmit holding register is empty, ready for another byte.
const TRANSMIT_EMPTY: u8 = 1 << 5;

pub static SERIAL1: LazyLock<Mutex<SerialPort>> = LazyLock::new(|| {
    // SAFETY: COM1 is the standard PC serial port, and this is its only driver.
    Mutex::new(unsafe { SerialPort::init(COM1) })
});

pub struct SerialPort {
    base: u16,
}

impl SerialPort {
    /// Configures the UART for 38400 baud, 8N1, with FIFOs enabled and interrupts off.
    ///
    /// # Safety
    /// `base` must be the base port of a 16550-compatible UART.
    unsafe fn init(base: u16) -> SerialPort {
        unsafe {
            outb(base + INTERRUPT_ENABLE, 0x00); // no interrupts: we'll poll instead

            // With DLAB (Divisor Latch Access Bit) set, ports +0/+1 temporarily become
            // the baud-rate divisor. Speed = 115200 / divisor, so 3 → 38400 baud.
            outb(base + LINE_CONTROL, 0x80); // DLAB on
            outb(base + DATA, 0x03); // divisor, low byte
            outb(base + INTERRUPT_ENABLE, 0x00); // divisor, high byte

            outb(base + LINE_CONTROL, 0x03); // DLAB off; 8 data bits, no parity, 1 stop bit
            outb(base + FIFO_CONTROL, 0xc7); // enable + clear FIFOs, 14-byte threshold
            outb(base + MODEM_CONTROL, 0x0b); // "data terminal ready", "request to send", OUT2
        }
        SerialPort { base }
    }

    pub fn send(&mut self, byte: u8) {
        // SAFETY: `base` points at a UART we initialised.
        unsafe {
            while inb(self.base + LINE_STATUS) & TRANSMIT_EMPTY == 0 {
                core::hint::spin_loop();
            }
            outb(self.base + DATA, byte);
        }
    }
}

impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            // Terminals expect "\r\n": carriage return (back to column 0) + line feed (down one row).
            if byte == b'\n' {
                self.send(b'\r');
            }
            self.send(byte);
        }
        Ok(())
    }
}

/// Like `print!`, but sends the text over the serial port to the host terminal.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::serial::_print(format_args!($($arg)*)));
}

/// Like `println!`, but sends the text over the serial port to the host terminal.
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}

/// Used by the macros above; not meant to be called directly.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    SERIAL1.lock().write_fmt(args).unwrap();
}
