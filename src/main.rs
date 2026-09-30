#![no_std] // no Rust standard library — there is no OS underneath us
#![no_main] // no normal `main`; the bootloader jumps straight to `_start`
#![feature(abi_x86_interrupt)] // nightly: lets Rust generate CPU exception handlers

extern crate alloc; // Box, Vec, String, ... backed by our heap (Milestone 9)

mod allocator;
mod crash_demo;
mod e1000;
mod frame_allocator;
mod gdt;
mod interrupts;
mod keyboard;
mod memory_demo;
mod multitasking_demo;
mod net;
mod paging;
mod pci;
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

    start_networking();

    crash_demo::run_from_env();

    println!("Type on your keyboard:");
    hlt_loop();
}

/// Phase 3: find the network card, bring it up, and ping the gateway.
fn start_networking() {
    pci::log_devices();
    let (mac, ip) = match net::init() {
        Ok(addresses) => addresses,
        Err(error) => {
            println!("[net] no network: {:?}", error);
            return;
        }
    };
    let link = if net::link_up() { "up" } else { "down" };
    serial_println!("[net] e1000 ready: MAC {}, IP {}, link {}", net::MacAddr(mac), net::IpAddr(ip), link);
    println!("[net] e1000 MAC {}, IP {}, link {}", net::MacAddr(mac), net::IpAddr(ip), link);

    let gateway = net::configured_gateway();
    let mut results = alloc::string::String::new();
    let mut replies = 0;
    for sequence in 1..=3 {
        let result = match net::ping(gateway, sequence) {
            Some(0) => { replies += 1; alloc::string::String::from("<10ms") }
            Some(ticks) => { replies += 1; alloc::format!("{}ms", ticks * 10) }
            None => alloc::string::String::from("lost"),
        };
        serial_println!("[net] ping {} seq={}: {}", net::IpAddr(gateway), sequence, result);
        results.push_str(&result);
        results.push(' ');
    }
    println!("[net] ping {}: {}/3 replies ({})", net::IpAddr(gateway), replies, results.trim_end());
    println!("[net] listening: try `ping {}` from another machine", net::IpAddr(ip));
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
