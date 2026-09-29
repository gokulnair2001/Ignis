//! Helpers that only make sense when running inside QEMU.

use crate::port::outb;

/// Port of QEMU's `isa-debug-exit` device (configured in Cargo.toml's bootimage run-args).
const ISA_DEBUG_EXIT_PORT: u16 = 0xf4;

/// QEMU exits with status `(code << 1) | 1`, so Success → 33 and Failed → 35.
/// (0 can't be produced, which is why we don't use it for success.)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

/// Powers off QEMU immediately, reporting `code` as QEMU's exit status.
#[allow(dead_code)]
pub fn exit_qemu(code: QemuExitCode) -> ! {
    // SAFETY: the isa-debug-exit device lives at this port; writing to it only exits QEMU.
    unsafe { outb(ISA_DEBUG_EXIT_PORT, code as u8) };
    // Only reached if the device is missing (e.g. QEMU started without it).
    crate::hlt_loop();
}
