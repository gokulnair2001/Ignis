//! Boot-time demonstrations for Milestones 10 and 11.

use crate::scheduler::{self, spawn, yield_now};
use crate::timer::{self, TICKS_PER_SECOND};
use crate::vga_buffer::{Color, WRITER};
use crate::{print, println, serial_println};
use core::sync::atomic::{AtomicU64, Ordering};

/// M10: three tasks take turns by yielding. Each prints its name and round number,
/// so the output interleaves: A1 B1 C1 A2 B2 C2 A3 B3 C3.
pub fn cooperative() {
    scheduler::set_preemption(false);
    print!("[M10] cooperative, tasks yield in turn: ");
    for name in ["A", "B", "C"] {
        spawn(name, move || {
            for round in 1..=3 {
                print!("{}{} ", name, round);
                serial_println!("[M10] task {} round {} (running as {})", name, round, scheduler::current_task_name());
                yield_now();
            }
        });
    }
    // The main task yields too, until the others have finished.
    while scheduler::task_count() > 1 {
        yield_now();
    }
    println!("-> done");
}

/// When each racer finished, in timer ticks (0 = still running).
static FINISHED_AT: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

/// M11: three tasks that never yield. Without preemption, the first would hog the CPU
/// until it finished. With the timer switching tasks 100 times a second, all three
/// progress bars move together. B has twice A's work, C three times.
pub fn preemptive() {
    const BASE_WORK: u64 = 6_000_000;
    println!("[M11] preemptive: 3 busy tasks, timer switches them (see row 2)");
    for (slot, name) in ["A", "B", "C"].into_iter().enumerate() {
        spawn(name, move || {
            let work = BASE_WORK * (slot as u64 + 1);
            let step = work / 100;
            for i in 0..=work {
                if i % step == 0 {
                    draw_progress(slot, name, i * 100 / work);
                }
                core::hint::black_box(i); // stops the compiler deleting the "useless" loop
            }
            FINISHED_AT[slot].store(timer::ticks(), Ordering::Relaxed);
        });
    }

    let start = timer::ticks();
    scheduler::set_preemption(true);
    // The main task just sleeps; timer ticks switch between it and the racers.
    while scheduler::task_count() > 1 {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }

    let seconds = |slot: usize| {
        let ticks = FINISHED_AT[slot].load(Ordering::Relaxed) - start;
        (ticks / TICKS_PER_SECOND as u64, ticks % TICKS_PER_SECOND as u64)
    };
    let (a, b, c) = (seconds(0), seconds(1), seconds(2));
    println!(
        "[M11] finished: A {}.{:02}s, B {}.{:02}s, C {}.{:02}s (fair share)",
        a.0, a.1, b.0, b.1, c.0, c.1
    );
}

/// Draws `A [########........] 50%` in the task row, one slot per task.
fn draw_progress(slot: usize, name: &str, percent: u64) {
    const BAR_WIDTH: usize = 16;
    let filled = (percent as usize * BAR_WIDTH) / 100;
    let mut text = [b' '; 25];
    text[0] = name.as_bytes()[0];
    text[2] = b'[';
    for i in 0..BAR_WIDTH {
        text[3 + i] = if i < filled { b'#' } else { b'.' };
    }
    text[3 + BAR_WIDTH] = b']';
    let digits = [(percent / 100) as u8, (percent / 10 % 10) as u8, (percent % 10) as u8];
    let start = 21;
    for (i, digit) in digits.iter().enumerate() {
        text[start + i] = b'0' + digit;
    }
    if percent < 100 {
        text[start] = b' ';
        if percent < 10 {
            text[start + 1] = b' ';
        }
    }
    text[24] = b'%';
    let text = core::str::from_utf8(&text).unwrap();
    let color = if percent == 100 { Color::LightGreen } else { Color::Yellow };
    crate::interrupts::without_interrupts(|| {
        WRITER.lock().write_task_row(slot * 27, text, color);
    });
}
