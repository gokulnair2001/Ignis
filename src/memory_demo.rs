//! Boot-time demonstrations and self-checks for Milestones 7–9.
//! Each prints one summary line on screen; details go to serial.

use crate::allocator::{self, HEAP_SIZE, HEAP_START};
use crate::frame_allocator::{self, FRAME_SIZE};
use crate::paging::{self, WRITABLE};
use crate::{print, println, serial_println};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// M7: how much RAM there is, and hand-out / give-back / reuse of frames.
pub fn frames() {
    let (free, usable) = frame_allocator::stats();
    let a = frame_allocator::allocate_frame().expect("out of frames");
    let b = frame_allocator::allocate_frame().expect("out of frames");
    frame_allocator::deallocate_frame(a);
    let c = frame_allocator::allocate_frame().expect("out of frames");
    frame_allocator::deallocate_frame(b);
    frame_allocator::deallocate_frame(c);

    serial_println!("[M7] frames: allocated {:#x} and {:#x}, freed the first, next was {:#x}", a, b, c);
    assert_eq!(a, c, "a freed frame should be handed out again");
    assert_eq!(frame_allocator::stats().0, free, "frame count should be back to where it started");

    println!(
        "[M7] RAM: {} MiB usable = {} frames of 4 KiB; alloc/free/reuse ok",
        usable as u64 * FRAME_SIZE / (1024 * 1024),
        usable
    );
}

/// M8: translate a few addresses by walking the page tables, then create a new mapping.
pub fn paging(physical_memory_offset: u64, kernel_code: u64) {
    serial_println!("[M8] level-4 table entries in use: {}", paging::level4_entries_used());
    let stack_variable = 0u64;
    let examples = [
        ("VGA text buffer", 0xb8000),
        ("kernel code", kernel_code),
        ("kernel stack", &stack_variable as *const u64 as u64),
        ("VGA via physical-memory window", physical_memory_offset + 0xb8000),
        ("nothing mapped here", 0xdead_beef_000),
    ];
    for (name, virtual_address) in examples {
        match paging::translate(virtual_address) {
            Some(physical) => serial_println!("[M8]   {:>30}: virt {:#014x} -> phys {:#x}", name, virtual_address, physical),
            None => serial_println!("[M8]   {:>30}: virt {:#014x} -> not mapped", name, virtual_address),
        }
    }
    let kernel_physical = paging::translate(kernel_code).expect("kernel code must be mapped");
    assert_eq!(paging::translate(0xdead_beef_000), None);
    println!("[M8] kernel code: virtual {:#x} -> physical {:#x}", kernel_code, kernel_physical);

    // Give the screen a second virtual address: map a brand-new page to the VGA frame.
    const ALIAS: u64 = 0x5555_5555_0000;
    paging::map_page(ALIAS, 0xb8000, WRITABLE).expect("mapping the VGA alias failed");
    assert_eq!(paging::translate(ALIAS), Some(0xb8000));

    // Write "works!" onto the current line *through the new address*, not via 0xB8000.
    let prefix = "[M8] new page 0x5555_5555_0000 -> VGA frame: ";
    print!("{}", prefix);
    let alias = ALIAS as *mut u16;
    let bottom_row_start = 24 * 80;
    for (i, byte) in "works!".bytes().enumerate() {
        let cell = (0x0a << 8) | byte as u16; // light green on black
        unsafe { alias.add(bottom_row_start + prefix.len() + i).write_volatile(cell) };
    }
    println!();
}

/// M9: set up the heap and exercise it with real `alloc` types.
pub fn heap() {
    allocator::init_heap().expect("heap initialisation failed");
    let free_at_start = allocator::free_bytes();
    serial_println!("[M9] heap: {} KiB at {:#x}", HEAP_SIZE / 1024, HEAP_START);

    // Box, Vec (grows by reallocating), String.
    let answer = Box::new(41);
    let mut numbers = Vec::new();
    for n in 0..500u64 {
        numbers.push(n);
    }
    let sum: u64 = numbers.iter().sum();
    let mut greeting = String::from("Hello");
    greeting.push_str(" from the heap");
    let message = format!("{}! box={} sum={}", greeting, *answer + 1, sum);
    serial_println!("[M9] {}", message);
    assert_eq!(sum, 124_750);
    println!("[M9] 1 MiB heap: Box/Vec/String ok: \"{}\"", message);
    drop((answer, numbers, greeting, message));

    // Freed memory is reused: 10 MiB of allocations through a 1 MiB heap.
    for i in 0..10_000u32 {
        let block = Box::new([i as u8; 1024]);
        core::hint::black_box(&block);
    }

    // Freed neighbours merge: fill most of the heap with 8 KiB pieces, free them all,
    // then ask for one allocation bigger than any single piece.
    let pieces: Vec<Vec<u8>> = (0..100).map(|_| Vec::with_capacity(8 * 1024)).collect();
    drop(pieces);
    let big: Vec<u8> = Vec::with_capacity(900 * 1024);
    drop(big);

    let free_at_end = allocator::free_bytes();
    serial_println!("[M9] free bytes: {} at start, {} at end", free_at_start, free_at_end);
    assert_eq!(free_at_start, free_at_end, "heap leaked memory");
    println!("[M9] 10 MiB reused through 1 MiB; 900 KiB after merging; no leaks");
}
