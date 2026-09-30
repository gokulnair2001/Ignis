//! Deliberate crashes, to see each exception handler in action.
//!
//! Pick one at build time with the `IGNIS_CRASH` environment variable, e.g.
//! `IGNIS_CRASH=page_fault cargo run`. Without it, nothing crashes.

use core::arch::asm;

pub fn run_from_env() {
    let Some(which) = option_env!("IGNIS_CRASH") else {
        return;
    };
    crate::serial_println!("[ignis] crash demo requested: {}", which);
    crate::println!("Crash demo: {}", which);

    match which {
        "divide" => divide_by_zero(),
        "opcode" => invalid_opcode(),
        "gpf" => bad_segment(),
        "page_fault" => page_fault(),
        "stack_overflow" => stack_overflow(),
        "oom" => out_of_memory(),
        other => crate::println!(
            "Unknown IGNIS_CRASH={}; try divide, opcode, gpf, page_fault, stack_overflow, oom",
            other
        ),
    }
}

/// `div` with a zero divisor. (Rust's own `/` checks for zero and panics first,
/// so we use the raw instruction to reach the CPU exception.)
fn divide_by_zero() {
    unsafe {
        asm!("div ecx", in("ecx") 0u32, inout("eax") 1u32 => _, inout("edx") 0u32 => _);
    }
}

/// `ud2` is an instruction that is guaranteed to be invalid.
fn invalid_opcode() {
    unsafe { asm!("ud2") };
}

/// Load a selector for GDT entry 6, which doesn't exist (our GDT has 5 entries).
fn bad_segment() {
    unsafe { asm!("mov ds, {0:x}", in(reg) 6u16 << 3) };
}

/// Write to an address nothing is mapped at.
fn page_fault() {
    unsafe { core::ptr::write_volatile(0xdead_beef_000 as *mut u8, 42) };
}

/// Recurse forever. Each call uses stack space until it runs into the unmapped
/// guard page below the stack → page fault → the CPU can't push the page fault's
/// frame onto the full stack → double fault, handled on the IST emergency stack.
#[allow(unconditional_recursion)]
fn stack_overflow() {
    stack_overflow();
    // Using the stack after the call stops the compiler turning the recursion into a loop.
    core::hint::black_box(());
}

/// Ask the 1 MiB heap for 2 MiB. The allocator returns null, and Rust's default
/// "allocation failed" handler turns that into a panic.
fn out_of_memory() {
    let too_big: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(2 * 1024 * 1024);
    core::hint::black_box(too_big);
}
