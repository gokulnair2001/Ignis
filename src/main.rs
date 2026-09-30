#![no_std] // no Rust standard library — there is no OS underneath us
#![no_main] // no normal `main`; the bootloader jumps straight to `_start`
#![feature(abi_x86_interrupt)] // nightly: lets Rust generate CPU exception handlers

extern crate alloc; // Box, Vec, String, ... backed by our heap (Milestone 9)

mod allocator;
mod crash_demo;
mod frame_allocator;
mod gdt;
mod interrupts;
mod keyboard;
mod memory_demo;
mod multitasking_demo;
mod paging;
mod pic;
mod port;
mod qemu;
mod scheduler;
mod serial;
mod timer;
mod vga_buffer;

use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use vga_buffer::{Color, WRITER};

// Generates `_start` for us and checks at compile time that `kernel_main` has the
// signature the bootloader expects.
entry_point!(kernel_main);

/// Kernel entry point. The bootloader calls this after switching to 64-bit long mode,
/// passing what it learned about the machine (e.g. the memory map).
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    WRITER.lock().clear_screen();
    println!("Ignis kernel");
    println!("[ok] VGA text screen + serial port (COM1)");
    serial_println!("[ignis] booted; VGA + serial ready");

    gdt::init();
    let (gdt_addr, df_stack) = gdt::debug_addresses();
    serial_println!("[ignis] GDT at {:#x}; double-fault stack top {:#x}", gdt_addr, df_stack);
    println!("[ok] GDT + TSS (emergency stack for double faults)");

    interrupts::init();
    // Trigger a harmless breakpoint exception: the handler reports it and returns.
    unsafe { core::arch::asm!("int3") };
    println!("[ok] IDT: CPU exceptions caught");

    pic::init();
    timer::init();
    timer::draw_status_bar(0);
    interrupts::enable();
    serial_println!("[ignis] PIC remapped, PIT at {} Hz, interrupts on", timer::TICKS_PER_SECOND);
    println!("[ok] PIC + timer ({} Hz) + keyboard interrupts", timer::TICKS_PER_SECOND);

    frame_allocator::log_memory_map(&boot_info.memory_map);
    frame_allocator::init(&boot_info.memory_map);
    memory_demo::frames();

    paging::init(boot_info.physical_memory_offset);
    memory_demo::paging(boot_info.physical_memory_offset, kernel_main as *const () as u64);

    memory_demo::heap();

    scheduler::init();
    multitasking_demo::cooperative();
    multitasking_demo::preemptive();

    crash_demo::run_from_env();

    println!("Type on your keyboard:");
    hlt_loop();
}

/// Called on panic: report over serial (most robust) and on screen in red, then halt.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // No more interrupts: a timer tick must not grab the screen lock while we report.
    interrupts::disable();
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
