//! Interrupt Descriptor Table (IDT) and CPU exception handlers.
//!
//! The IDT is the CPU's "phone book": 256 entries, one per exception/interrupt number,
//! each saying which function to jump to when that event happens.

use crate::keyboard::{self, Key};
use crate::pic;
use crate::vga_buffer::{Color, WRITER};
use crate::{gdt, print, scheduler, timer};
use core::arch::asm;
use core::fmt;
use core::mem::size_of;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::LazyLock;

// CPU exception numbers we handle (0–31 are reserved for CPU exceptions).
const DIVIDE_ERROR: usize = 0;
const BREAKPOINT: usize = 3;
const INVALID_OPCODE: usize = 6;
const DOUBLE_FAULT: usize = 8;
const GENERAL_PROTECTION_FAULT: usize = 13;
const PAGE_FAULT: usize = 14;

// ---------------------------------------------------------------------------
// IDT entries
// ---------------------------------------------------------------------------

/// One 16-byte IDT entry ("gate descriptor"). The handler's address is split
/// into three pieces for historical (16/32-bit era) reasons.
#[derive(Clone, Copy)]
#[repr(C)]
struct IdtEntry {
    offset_low: u16,     // handler address bits 0–15
    selector: u16,       // code segment to run the handler in (our GDT's 0x08)
    options: u16,        // IST index, gate type, privilege, present bit
    offset_middle: u16,  // handler address bits 16–31
    offset_high: u32,    // handler address bits 32–63
    reserved: u32,
}

const _: () = assert!(size_of::<IdtEntry>() == 16);

// Bits of the `options` field.
const PRESENT: u16 = 1 << 15;
/// Gate type 0b1110 = "64-bit interrupt gate": interrupts are switched off while
/// the handler runs, so a second interrupt can't barge in halfway through.
const INTERRUPT_GATE: u16 = 0b1110 << 8;

impl IdtEntry {
    /// An empty slot: if this exception fires, the CPU raises a double fault instead.
    const MISSING: IdtEntry = IdtEntry {
        offset_low: 0,
        selector: 0,
        options: 0,
        offset_middle: 0,
        offset_high: 0,
        reserved: 0,
    };

    /// `ist_index`: which TSS emergency stack to switch to, if any.
    fn new(handler: *const (), ist_index: Option<u16>) -> IdtEntry {
        let handler = handler as u64;
        // The IDT's IST field is 1-based: 0 means "don't switch stacks",
        // 1 means TSS slot 0, and so on.
        let ist = ist_index.map_or(0, |index| index + 1);
        IdtEntry {
            offset_low: handler as u16,
            selector: gdt::KERNEL_CODE_SELECTOR,
            options: PRESENT | INTERRUPT_GATE | ist,
            offset_middle: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, align(16))]
struct Idt([IdtEntry; 256]);

static IDT: LazyLock<Idt> = LazyLock::new(|| {
    let mut entries = [IdtEntry::MISSING; 256];
    entries[DIVIDE_ERROR] = IdtEntry::new(divide_error_handler as *const (), None);
    entries[BREAKPOINT] = IdtEntry::new(breakpoint_handler as *const (), None);
    entries[INVALID_OPCODE] = IdtEntry::new(invalid_opcode_handler as *const (), None);
    entries[GENERAL_PROTECTION_FAULT] =
        IdtEntry::new(general_protection_fault_handler as *const (), None);
    entries[PAGE_FAULT] = IdtEntry::new(page_fault_handler as *const (), None);
    // The double fault handler gets the emergency stack from the TSS (Milestone 4),
    // so it still works when the normal stack has overflowed.
    entries[DOUBLE_FAULT] =
        IdtEntry::new(double_fault_handler as *const (), Some(gdt::DOUBLE_FAULT_IST_INDEX));

    // Hardware interrupts, forwarded by the PIC at the numbers we remapped them to.
    entries[pic::TIMER_VECTOR as usize] = IdtEntry::new(timer_handler as *const (), None);
    entries[pic::KEYBOARD_VECTOR as usize] = IdtEntry::new(keyboard_handler as *const (), None);
    entries[pic::PRIMARY_SPURIOUS_VECTOR as usize] =
        IdtEntry::new(primary_spurious_handler as *const (), None);
    entries[pic::SECONDARY_SPURIOUS_VECTOR as usize] =
        IdtEntry::new(secondary_spurious_handler as *const (), None);
    // The remaining IRQ lines go to a dispatcher, so drivers found later (e.g. by the
    // PCI scan) can claim them at runtime with `register_irq_handler`.
    for (irq, handler) in DISPATCHED_IRQS.iter().zip(DISPATCH_HANDLERS) {
        entries[(pic::PRIMARY_OFFSET + irq) as usize] = IdtEntry::new(handler as *const (), None);
    }
    Idt(entries)
});

/// What `lidt` reads: the table's size (minus one) and its address.
#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base: u64,
}

/// Loads our IDT. Call once at boot, after `gdt::init()`.
pub fn init() {
    let pointer = IdtPointer {
        limit: (size_of::<Idt>() - 1) as u16,
        base: &*IDT as *const Idt as u64,
    };
    // SAFETY: the IDT is a static that lives forever and every present entry
    // points at a valid `extern "x86-interrupt"` handler.
    unsafe { asm!("lidt [{}]", in(reg) &pointer, options(readonly, nostack, preserves_flags)) };
}

// ---------------------------------------------------------------------------
// Turning interrupts on and off
// ---------------------------------------------------------------------------

/// Bit 9 of RFLAGS: the Interrupt Flag (IF). When clear, the CPU ignores hardware interrupts.
const INTERRUPT_FLAG: u64 = 1 << 9;

/// `sti` (SeT Interrupt flag): start accepting hardware interrupts.
pub fn enable() {
    // SAFETY: the IDT and PIC are set up, so every interrupt that can arrive has a handler.
    unsafe { asm!("sti", options(nomem, nostack)) };
}

/// `cli` (CLear Interrupt flag): stop accepting hardware interrupts.
pub fn disable() {
    // SAFETY: disabling interrupts can't break memory safety (at worst, input is delayed).
    unsafe { asm!("cli", options(nomem, nostack)) };
}

fn are_enabled() -> bool {
    let rflags: u64;
    // `pushfq` pushes RFLAGS onto the stack; `pop` moves it into a register we can read.
    unsafe { asm!("pushfq", "pop {}", out(reg) rflags, options(nomem, preserves_flags)) };
    rflags & INTERRUPT_FLAG != 0
}

/// Runs `f` with interrupts disabled, then restores the previous state. Used around
/// anything an interrupt handler might also lock, so the two can't deadlock.
pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    let were_enabled = are_enabled();
    if were_enabled {
        disable();
    }
    let result = f();
    if were_enabled {
        enable();
    }
    result
}

// ---------------------------------------------------------------------------
// Exception handlers
// ---------------------------------------------------------------------------

/// What the CPU pushes onto the stack before calling a handler.
#[repr(C)]
pub struct InterruptStackFrame {
    instruction_pointer: u64, // RIP: the instruction that faulted (or the next one)
    code_segment: u64,        // CS at the time
    cpu_flags: u64,           // RFLAGS
    stack_pointer: u64,       // RSP at the time
    stack_segment: u64,       // SS at the time
}

impl fmt::Debug for InterruptStackFrame {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "  RIP (instruction) = {:#x}\n  RSP (stack)       = {:#x}\n  CS = {:#x}  SS = {:#x}  RFLAGS = {:#x}",
            self.instruction_pointer,
            self.stack_pointer,
            self.code_segment,
            self.stack_segment,
            self.cpu_flags,
        )
    }
}

/// Prints to both the serial port and the screen.
macro_rules! report {
    ($($arg:tt)*) => {{
        $crate::serial_println!($($arg)*);
        $crate::println!($($arg)*);
    }};
}

/// Reports a fatal exception in red and stops. We can't safely continue after these.
fn fatal(name: &str, frame: &InterruptStackFrame, detail: fmt::Arguments) -> ! {
    WRITER.lock().set_color(Color::LightRed, Color::Black);
    report!("EXCEPTION: {}\n{}\n{:?}", name, detail, frame);
    crate::hlt_loop();
}

extern "x86-interrupt" fn breakpoint_handler(frame: InterruptStackFrame) {
    // Harmless: report it and return, and the interrupted code carries on.
    // Full details go to serial; the screen gets one line.
    crate::serial_println!("EXCEPTION: BREAKPOINT (int3) - returning and carrying on\n{:?}", frame);
    WRITER.lock().set_color(Color::Yellow, Color::Black);
    crate::println!("  EXCEPTION: BREAKPOINT at {:#x} - handled, carrying on", frame.instruction_pointer);
    WRITER.lock().set_color(Color::LightCyan, Color::Black);
}

extern "x86-interrupt" fn divide_error_handler(frame: InterruptStackFrame) {
    fatal("DIVIDE ERROR (#DE)", &frame, format_args!("  divided by zero"));
}

extern "x86-interrupt" fn invalid_opcode_handler(frame: InterruptStackFrame) {
    fatal("INVALID OPCODE (#UD)", &frame, format_args!("  the CPU doesn't know this instruction"));
}

extern "x86-interrupt" fn general_protection_fault_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) {
    // For segment-related faults, the error code is the offending selector.
    fatal(
        "GENERAL PROTECTION FAULT (#GP)",
        &frame,
        format_args!("  error code = {:#x} (selector involved, 0 if none)", error_code),
    );
}

extern "x86-interrupt" fn page_fault_handler(frame: InterruptStackFrame, error_code: u64) {
    // CR2 holds the address that was being accessed.
    let address: u64;
    unsafe { asm!("mov {}, cr2", out(reg) address, options(nomem, nostack, preserves_flags)) };

    let cause = if error_code & 1 != 0 { "protection violation" } else { "page not mapped" };
    let access = if error_code & (1 << 4) != 0 {
        "instruction fetch"
    } else if error_code & (1 << 1) != 0 {
        "write"
    } else {
        "read"
    };
    fatal(
        "PAGE FAULT (#PF)",
        &frame,
        format_args!("  address = {:#x}\n  {} during a {} (error code {:#x})", address, cause, access, error_code),
    );
}

extern "x86-interrupt" fn double_fault_handler(frame: InterruptStackFrame, _error_code: u64) -> ! {
    // The error code for a double fault is always 0.
    fatal(
        "DOUBLE FAULT (#DF)",
        &frame,
        format_args!("  a fault happened while handling another fault (running on the IST emergency stack)"),
    );
}

// ---------------------------------------------------------------------------
// Hardware interrupt (IRQ) handlers
// ---------------------------------------------------------------------------

extern "x86-interrupt" fn timer_handler(_frame: InterruptStackFrame) {
    timer::on_tick();
    // Acknowledge *before* possibly switching tasks: we may not come back to this
    // handler for a while, and the PIC sends no more ticks until it gets the EOI.
    pic::end_of_interrupt(pic::TIMER_VECTOR);
    scheduler::on_timer_tick();
}

extern "x86-interrupt" fn keyboard_handler(_frame: InterruptStackFrame) {
    match keyboard::read_key() {
        Some(Key::Char('\t')) => print!("    "),
        Some(Key::Char(c)) => print!("{}", c),
        Some(Key::Backspace) => WRITER.lock().backspace(),
        None => {}
    }
    pic::end_of_interrupt(pic::KEYBOARD_VECTOR);
}

extern "x86-interrupt" fn primary_spurious_handler(_frame: InterruptStackFrame) {
    // A spurious IRQ 7 must NOT get an end-of-interrupt: the PIC isn't expecting one.
    if !pic::is_spurious(pic::PRIMARY_SPURIOUS_VECTOR) {
        pic::end_of_interrupt(pic::PRIMARY_SPURIOUS_VECTOR);
    }
}

extern "x86-interrupt" fn secondary_spurious_handler(_frame: InterruptStackFrame) {
    if pic::is_spurious(pic::SECONDARY_SPURIOUS_VECTOR) {
        pic::end_of_spurious_secondary();
    } else {
        pic::end_of_interrupt(pic::SECONDARY_SPURIOUS_VECTOR);
    }
}

// ---------------------------------------------------------------------------
// Runtime-registered IRQ handlers (for devices discovered after boot, like PCI cards)
// ---------------------------------------------------------------------------

/// IRQ lines not already claimed (0 timer, 1 keyboard, 2 cascade, 7/15 spurious).
const DISPATCHED_IRQS: [u8; 11] = [3, 4, 5, 6, 8, 9, 10, 11, 12, 13, 14];

/// The driver function for each IRQ line, stored as a plain address (0 = none).
static IRQ_HANDLERS: [AtomicUsize; 16] = [const { AtomicUsize::new(0) }; 16];

/// Makes `handler` run whenever `irq` fires, and unmasks that line on the PIC.
pub fn register_irq_handler(irq: u8, handler: fn()) {
    assert!(DISPATCHED_IRQS.contains(&irq), "IRQ {} can't be registered", irq);
    IRQ_HANDLERS[irq as usize].store(handler as usize, Ordering::Release);
    pic::unmask(irq);
}

fn dispatch_irq(irq: u8) {
    let handler = IRQ_HANDLERS[irq as usize].load(Ordering::Acquire);
    if handler != 0 {
        // SAFETY: only ever set from a `fn()` in `register_irq_handler`.
        let handler: fn() = unsafe { core::mem::transmute(handler) };
        handler();
    }
    pic::end_of_interrupt(pic::PRIMARY_OFFSET + irq);
}

/// An `extern "x86-interrupt"` handler can't be told which vector fired, so we
/// generate one tiny handler per IRQ line that passes its own number along.
macro_rules! irq_dispatchers {
    ($($irq:literal => $name:ident),*) => {
        $(extern "x86-interrupt" fn $name(_frame: InterruptStackFrame) { dispatch_irq($irq); })*
        const DISPATCH_HANDLERS: [extern "x86-interrupt" fn(InterruptStackFrame); DISPATCHED_IRQS.len()] =
            [$($name),*];
    };
}

irq_dispatchers!(
    3 => irq3_handler, 4 => irq4_handler, 5 => irq5_handler, 6 => irq6_handler,
    8 => irq8_handler, 9 => irq9_handler, 10 => irq10_handler, 11 => irq11_handler,
    12 => irq12_handler, 13 => irq13_handler, 14 => irq14_handler
);
