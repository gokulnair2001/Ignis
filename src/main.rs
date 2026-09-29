#![no_std] // no Rust standard library — there is no OS underneath us
#![no_main] // no normal `main`; the bootloader jumps straight to `_start`
#![feature(abi_x86_interrupt)] // nightly: lets Rust generate CPU exception handlers

mod crash_demo;
mod gdt;
mod interrupts;
mod port;
mod qemu;
mod serial;
mod vga_buffer;

use core::panic::PanicInfo;
use vga_buffer::{Color, WRITER};

/// Kernel entry point. The bootloader calls this after switching to 64-bit long mode.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    WRITER.lock().clear_screen();

    println!("Hello, Ignis!");
    println!("The answer is {}, and pi is roughly {}/{}.", 42, 22, 7);
    println!("The screen lives at {:#x}.", 0xb8000);

    serial_println!("[ignis] booted; VGA text output ready");
    serial_println!("[ignis] hello from COM1 at {:#x}", 0x3f8);

    gdt::init();
    let (gdt_addr, df_stack) = gdt::debug_addresses();
    serial_println!("[ignis] GDT loaded at {:#x}; double-fault stack top {:#x}", gdt_addr, df_stack);
    println!("GDT + TSS loaded. Still alive: no triple fault!");

    interrupts::init();
    serial_println!("[ignis] IDT loaded");

    // Trigger a harmless breakpoint exception: the handler reports it and returns.
    unsafe { core::arch::asm!("int3") };
    println!("Back from the breakpoint handler: exceptions work!");

    crash_demo::run_from_env();

    hlt_loop();
}

/// Called on panic: report over serial (most robust) and on screen in red, then halt.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("[ignis] KERNEL PANIC: {}", info);
    WRITER.lock().set_color(Color::LightRed, Color::Black);
    println!("KERNEL PANIC: {}", info);
    hlt_loop();
}

/// Halt the CPU until the next interrupt, forever. Cheaper than a busy `loop {}`.
pub fn hlt_loop() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}
