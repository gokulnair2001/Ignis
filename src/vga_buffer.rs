//! VGA text-mode driver: draws characters by writing into the memory-mapped
//! text buffer at 0xB8000.

use crate::port::outb;
use core::{fmt, ptr};
use spin::{LazyLock, Mutex};

/// VGA CRT (Cathode Ray Tube) controller ports, used to move the hardware cursor.
const VGA_INDEX_PORT: u16 = 0x3d4;
const VGA_DATA_PORT: u16 = 0x3d5;

/// The one global screen writer. `LazyLock` builds it on first use; `Mutex` makes
/// sure only one piece of code writes to the screen at a time.
pub static WRITER: LazyLock<Mutex<Writer>> = LazyLock::new(|| {
    Mutex::new(Writer {
        column_position: 0,
        color_code: ColorCode::new(Color::LightCyan, Color::Black),
        // SAFETY: 0xB8000 is the VGA text buffer, and this is the only reference to it.
        buffer: unsafe { &mut *(0xb8000 as *mut Buffer) },
    })
});

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;

/// The 16 colours VGA text mode supports.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

/// A foreground + background pair packed into one byte: high 4 bits = background, low 4 = foreground.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColorCode(u8);

impl ColorCode {
    const fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

/// One cell on screen: exactly the 2 bytes the hardware expects, in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColorCode,
}

/// The whole 80×25 screen, laid out exactly like the memory at 0xB8000.
#[repr(transparent)]
struct Buffer {
    chars: [[ScreenChar; BUFFER_WIDTH]; BUFFER_HEIGHT],
}

/// Writes text to the bottom row of the screen, scrolling up when a line fills.
pub struct Writer {
    column_position: usize,
    color_code: ColorCode,
    buffer: &'static mut Buffer,
}

impl Writer {
    pub fn set_color(&mut self, foreground: Color, background: Color) {
        self.color_code = ColorCode::new(foreground, background);
    }

    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            byte => {
                if self.column_position >= BUFFER_WIDTH {
                    self.new_line();
                }

                let row = BUFFER_HEIGHT - 1;
                let col = self.column_position;
                self.write_cell(row, col, ScreenChar {
                    ascii_character: byte,
                    color_code: self.color_code,
                });
                self.column_position += 1;
            }
        }
    }

    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                // Printable ASCII or newline.
                0x20..=0x7e | b'\n' => self.write_byte(byte),
                // Anything else (e.g. multi-byte UTF-8) isn't in the VGA font: show ■.
                _ => self.write_byte(0xfe),
            }
        }
        self.update_cursor();
    }

    pub fn clear_screen(&mut self) {
        for row in 0..BUFFER_HEIGHT {
            self.clear_row(row);
        }
        self.column_position = 0;
        self.update_cursor();
    }

    /// Moves the blinking hardware cursor to where the next character will go.
    fn update_cursor(&self) {
        // The cursor position is a cell index (row * 80 + col). A full row would put it
        // past the edge, so keep it on the last column until the next character wraps.
        let col = self.column_position.min(BUFFER_WIDTH - 1);
        let position = ((BUFFER_HEIGHT - 1) * BUFFER_WIDTH + col) as u16;

        // The VGA card has dozens of internal registers but only two ports for them:
        // write a register number to the index port, then its value to the data port.
        // Registers 0x0F/0x0E hold the low/high byte of the cursor position.
        // SAFETY: these are the standard VGA CRT controller ports.
        unsafe {
            outb(VGA_INDEX_PORT, 0x0f);
            outb(VGA_DATA_PORT, (position & 0xff) as u8);
            outb(VGA_INDEX_PORT, 0x0e);
            outb(VGA_DATA_PORT, (position >> 8) as u8);
        }
    }

    /// Moves every row up by one (the top row is lost) and starts a fresh bottom row.
    fn new_line(&mut self) {
        for row in 1..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                let character = self.read_cell(row, col);
                self.write_cell(row - 1, col, character);
            }
        }
        self.clear_row(BUFFER_HEIGHT - 1);
        self.column_position = 0;
    }

    fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        for col in 0..BUFFER_WIDTH {
            self.write_cell(row, col, blank);
        }
    }

    // All screen access goes through these two, so every read/write is volatile:
    // the compiler must not drop or reorder them, since the hardware is watching.
    fn write_cell(&mut self, row: usize, col: usize, character: ScreenChar) {
        unsafe { ptr::write_volatile(&mut self.buffer.chars[row][col], character) };
    }

    fn read_cell(&self, row: usize, col: usize) -> ScreenChar {
        unsafe { ptr::read_volatile(&self.buffer.chars[row][col]) }
    }
}

/// Lets Rust's formatting machinery (`write!`, `{}` placeholders) output to the screen.
impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

/// Like the standard `print!`, but draws on the VGA screen.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::vga_buffer::_print(format_args!($($arg)*)));
}

/// Like the standard `println!`, but draws on the VGA screen.
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// Used by the macros above; not meant to be called directly.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    WRITER.lock().write_fmt(args).unwrap();
}
