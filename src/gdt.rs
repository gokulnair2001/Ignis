//! Global Descriptor Table (GDT) and Task State Segment (TSS).
//!
//! In 64-bit mode segmentation is mostly disabled (flat memory), but the CPU still needs:
//! - a code segment that says "64-bit mode, ring 0", and
//! - a TSS, whose Interrupt Stack Table (IST) holds known-good stacks for emergencies
//!   like a double fault caused by a stack overflow.

use core::arch::asm;
use core::mem::size_of;
use spin::LazyLock;

// ---------------------------------------------------------------------------
// Segment descriptor bits (one GDT entry = one 64-bit number)
// ---------------------------------------------------------------------------

const LIMIT_0_15: u64 = 0xffff; // limit bits 0–15 (ignored in 64-bit mode, set for tidiness)
const ACCESSED: u64 = 1 << 40; // pre-set so the CPU never needs to write it
const WRITABLE: u64 = 1 << 41; // data: writable / code: readable
const EXECUTABLE: u64 = 1 << 43; // code segment (vs data segment)
const USER_SEGMENT: u64 = 1 << 44; // "S" bit: code/data segment (vs system segment like TSS)
const PRESENT: u64 = 1 << 47; // entry is valid
const LIMIT_16_19: u64 = 0xf << 48;
const LONG_MODE: u64 = 1 << 53; // "L" bit: this code segment runs 64-bit code
const DEFAULT_SIZE: u64 = 1 << 54; // "D/B" bit: 32-bit operands (must be 0 for 64-bit code)
const GRANULARITY: u64 = 1 << 55; // limit counts 4 KiB pages instead of bytes
// Privilege (DPL) lives in bits 45–46; we leave it 0, which means ring 0.

const COMMON: u64 =
    USER_SEGMENT | PRESENT | WRITABLE | ACCESSED | LIMIT_0_15 | LIMIT_16_19 | GRANULARITY;
const KERNEL_CODE: u64 = COMMON | EXECUTABLE | LONG_MODE;
const KERNEL_DATA: u64 = COMMON | DEFAULT_SIZE;

// A selector = (entry index << 3) | requested privilege level. Index 0 must be the null entry.
pub const KERNEL_CODE_SELECTOR: u16 = 1 << 3; // 0x08
pub const KERNEL_DATA_SELECTOR: u16 = 2 << 3; // 0x10
pub const TSS_SELECTOR: u16 = 3 << 3; // 0x18 (the TSS takes two slots: 3 and 4)

// ---------------------------------------------------------------------------
// Task State Segment
// ---------------------------------------------------------------------------

/// Which IST slot the double-fault handler (Milestone 5) will switch to.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// The 64-bit TSS layout, exactly as the CPU expects it (104 bytes).
#[repr(C, packed(4))]
pub struct TaskStateSegment {
    reserved_1: u32,
    /// Stacks used when switching *into* ring 0 from a less privileged ring (unused for now).
    privilege_stack_table: [u64; 3],
    reserved_2: u64,
    /// Up to 7 emergency stacks an interrupt handler can ask to switch to.
    interrupt_stack_table: [u64; 7],
    reserved_3: u64,
    reserved_4: u16,
    /// Offset of the I/O permission bitmap; pointing past the end means "no bitmap".
    iomap_base: u16,
}

const _: () = assert!(size_of::<TaskStateSegment>() == 104);

const DOUBLE_FAULT_STACK_SIZE: usize = 4096 * 5; // 20 KiB

/// Stack memory must be 16-byte aligned for the x86_64 calling convention.
#[repr(align(16))]
#[allow(dead_code)] // the bytes are only ever used by the CPU, never read by our code
struct Stack([u8; DOUBLE_FAULT_STACK_SIZE]);

/// The emergency stack itself. Only ever touched by the CPU, via its address.
static mut DOUBLE_FAULT_STACK: Stack = Stack([0; DOUBLE_FAULT_STACK_SIZE]);

static TSS: LazyLock<TaskStateSegment> = LazyLock::new(|| {
    // Stacks grow *downwards*, so the CPU needs the address just past the end.
    let stack_start = (&raw const DOUBLE_FAULT_STACK) as u64;
    let stack_end = stack_start + DOUBLE_FAULT_STACK_SIZE as u64;

    let mut interrupt_stack_table = [0; 7];
    interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = stack_end;

    TaskStateSegment {
        reserved_1: 0,
        privilege_stack_table: [0; 3],
        reserved_2: 0,
        interrupt_stack_table,
        reserved_3: 0,
        reserved_4: 0,
        iomap_base: size_of::<TaskStateSegment>() as u16,
    }
});

/// Builds the 16-byte (two-slot) GDT entry that points at a TSS.
fn tss_descriptor(tss: &'static TaskStateSegment) -> [u64; 2] {
    let base = tss as *const _ as u64;
    let limit = (size_of::<TaskStateSegment>() - 1) as u64;
    const TYPE_AVAILABLE_TSS: u64 = 0b1001 << 40;

    let low = (limit & 0xffff)
        | ((base & 0xff_ffff) << 16) // base bits 0–23
        | TYPE_AVAILABLE_TSS
        | PRESENT
        | ((base >> 24) & 0xff) << 56; // base bits 24–31
    let high = base >> 32; // base bits 32–63
    [low, high]
}

// ---------------------------------------------------------------------------
// The GDT itself
// ---------------------------------------------------------------------------

#[repr(C, align(8))]
struct Gdt([u64; 5]);

static GDT: LazyLock<Gdt> = LazyLock::new(|| {
    let [tss_low, tss_high] = tss_descriptor(&TSS);
    Gdt([0, KERNEL_CODE, KERNEL_DATA, tss_low, tss_high])
});

/// What `lgdt` reads: the table's size (minus one) and its address.
#[repr(C, packed)]
struct GdtPointer {
    limit: u16,
    base: u64,
}

/// Replaces the bootloader's GDT with ours and loads the TSS. Call exactly once, at boot.
pub fn init() {
    let pointer = GdtPointer {
        limit: (size_of::<Gdt>() - 1) as u16,
        base: &*GDT as *const Gdt as u64,
    };

    // SAFETY: the GDT and TSS are statics that live forever, and the selectors match
    // their entries. Called once, so the TSS isn't already marked busy.
    unsafe {
        asm!("lgdt [{}]", in(reg) &pointer, options(readonly, nostack, preserves_flags));

        // CS can't be set with `mov`. Instead, fake a "far return": push the new code
        // selector and the address of the next instruction, then `retfq` pops both.
        asm!(
            "push {sel}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            sel = in(reg) KERNEL_CODE_SELECTOR as u64,
            tmp = lateout(reg) _,
            options(preserves_flags),
        );

        // The data segment registers can be set with plain `mov`.
        asm!(
            "mov ss, {0:x}",
            "mov ds, {0:x}",
            "mov es, {0:x}",
            in(reg) KERNEL_DATA_SELECTOR,
            options(nostack, preserves_flags),
        );

        // Load the Task Register, so the CPU knows where our TSS is.
        asm!("ltr {0:x}", in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
    }
}

/// Where the GDT and emergency stack live (for boot logging).
pub fn debug_addresses() -> (u64, u64) {
    let gdt = &*GDT as *const Gdt as u64;
    let stack_end = (&raw const DOUBLE_FAULT_STACK) as u64 + DOUBLE_FAULT_STACK_SIZE as u64;
    (gdt, stack_end)
}
