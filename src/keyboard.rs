//! PS/2 keyboard driver. Each key press/release makes the keyboard controller raise
//! IRQ 1 and put a "scancode" (a number for the physical key, not a letter) on port 0x60.
//! We translate scancode set 1 into characters for a US keyboard layout.

use crate::port::inb;
use spin::Mutex;

const DATA_PORT: u16 = 0x60;

/// Scancodes with this bit set mean "key released" (the same key's code + 0x80).
const RELEASED: u8 = 0x80;
/// Prefix byte for "extended" keys (arrows, right Ctrl, ...), which we ignore.
const EXTENDED_PREFIX: u8 = 0xe0;

const LEFT_SHIFT: u8 = 0x2a;
const RIGHT_SHIFT: u8 = 0x36;
const CAPS_LOCK: u8 = 0x3a;
const BACKSPACE: u8 = 0x0e;

/// Set-1 scancode → character, without and with Shift. `0` = no character.
/// Index = scancode. Rows follow the physical keyboard rows.
const NORMAL: [u8; 58] = *b"\0\x1b1234567890-=\x08\tqwertyuiop[]\n\0asdfghjkl;'`\0\\zxcvbnm,./\0*\0 ";
const SHIFTED: [u8; 58] = *b"\0\x1b!@#$%^&*()_+\x08\tQWERTYUIOP{}\n\0ASDFGHJKL:\"~\0|ZXCVBNM<>?\0*\0 ";

pub enum Key {
    Char(char),
    Backspace,
}

struct KeyboardState {
    shift_held: bool,
    caps_lock: bool,
    after_extended_prefix: bool,
}

static STATE: Mutex<KeyboardState> = Mutex::new(KeyboardState {
    shift_held: false,
    caps_lock: false,
    after_extended_prefix: false,
});

/// Reads the waiting scancode (this also tells the controller we've taken it) and
/// turns it into a key, if it's a press of a key we know.
pub fn read_key() -> Option<Key> {
    // SAFETY: port 0x60 is the PS/2 controller's data port; reading it takes the byte.
    let scancode = unsafe { inb(DATA_PORT) };
    let mut state = STATE.lock();

    if scancode == EXTENDED_PREFIX {
        state.after_extended_prefix = true;
        return None;
    }
    if state.after_extended_prefix {
        state.after_extended_prefix = false;
        return None; // an arrow key etc. — not supported yet
    }

    let released = scancode & RELEASED != 0;
    let code = scancode & !RELEASED;

    match code {
        LEFT_SHIFT | RIGHT_SHIFT => {
            state.shift_held = !released;
            None
        }
        CAPS_LOCK if !released => {
            state.caps_lock = !state.caps_lock;
            None
        }
        _ if released => None,
        BACKSPACE => Some(Key::Backspace),
        _ => {
            let normal = *NORMAL.get(code as usize)?;
            if normal == 0 || normal == 0x1b {
                return None; // modifier or Escape: nothing to print
            }
            // Caps Lock only affects letters; Shift affects everything.
            let use_shifted = if normal.is_ascii_alphabetic() {
                state.shift_held != state.caps_lock
            } else {
                state.shift_held
            };
            let byte = if use_shifted { SHIFTED[code as usize] } else { normal };
            Some(Key::Char(byte as char))
        }
    }
}
