//! Paging / virtual memory (Milestone 8).
//!
//! Every address the kernel uses is *virtual*. The CPU's MMU (Memory Management Unit)
//! translates it to a *physical* RAM address by walking a 4-level tree of page tables:
//!
//!   virtual address bits:  | 47..39 | 38..30 | 29..21 | 20..12 | 11..0  |
//!                          |  L4    |  L3    |  L2    |  L1    | offset |
//!
//! Each level is a 4 KiB table of 512 entries (9 bits of index). CR3 holds the physical
//! address of the level-4 table. The bootloader built these tables; now we read and
//! extend them ourselves.

use crate::frame_allocator::{self, FRAME_SIZE};
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

// Page table entry flags (low bits of each 64-bit entry).
pub const PRESENT: u64 = 1 << 0; // entry is valid
pub const WRITABLE: u64 = 1 << 1; // writes allowed
pub const HUGE_PAGE: u64 = 1 << 7; // in L3/L2: maps a 1 GiB / 2 MiB page directly
/// Bits 12–51 of an entry hold the physical address of the next table or the frame.
const ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;

const ENTRIES_PER_TABLE: usize = 512;

#[repr(C, align(4096))]
struct PageTable {
    entries: [u64; ENTRIES_PER_TABLE],
}

/// Where the bootloader mapped all of physical memory into our virtual address space.
/// Physical address `p` can be reached at virtual address `p + PHYSICAL_MEMORY_OFFSET`.
static PHYSICAL_MEMORY_OFFSET: AtomicU64 = AtomicU64::new(0);

pub fn init(physical_memory_offset: u64) {
    PHYSICAL_MEMORY_OFFSET.store(physical_memory_offset, Ordering::Relaxed);
}

/// A virtual address at which the given physical address can be accessed.
fn phys_to_virt(physical: u64) -> *mut u8 {
    (physical + PHYSICAL_MEMORY_OFFSET.load(Ordering::Relaxed)) as *mut u8
}

/// The page table stored in the given physical frame.
///
/// # Safety
/// `physical` must be the address of a page table, and the caller must not create
/// overlapping `&mut` references to the same table.
unsafe fn table_at(physical: u64) -> &'static mut PageTable {
    unsafe { &mut *(phys_to_virt(physical) as *mut PageTable) }
}

/// Physical address of the active level-4 table, from the CR3 register.
fn level4_table_address() -> u64 {
    let cr3: u64;
    unsafe { asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)) };
    cr3 & ADDRESS_MASK
}

/// Splits a virtual address into its four table indexes: [L4, L3, L2, L1].
fn table_indexes(virtual_address: u64) -> [usize; 4] {
    [39, 30, 21, 12].map(|shift| ((virtual_address >> shift) & 0x1ff) as usize)
}

/// Walks the page tables like the MMU does. Returns the physical address, or `None`
/// if the virtual address isn't mapped.
pub fn translate(virtual_address: u64) -> Option<u64> {
    let indexes = table_indexes(virtual_address);
    let mut table_address = level4_table_address();

    for (level, &index) in indexes.iter().enumerate() {
        // SAFETY: `table_address` came from CR3 or a present entry, so it's a page table.
        let entry = unsafe { table_at(table_address) }.entries[index];
        if entry & PRESENT == 0 {
            return None;
        }
        let address = entry & ADDRESS_MASK;
        match level {
            // A huge page stops the walk early; the rest of the address is the offset.
            1 if entry & HUGE_PAGE != 0 => return Some(address + (virtual_address & 0x3fff_ffff)), // 1 GiB
            2 if entry & HUGE_PAGE != 0 => return Some(address + (virtual_address & 0x1f_ffff)), // 2 MiB
            3 => return Some(address + (virtual_address & 0xfff)), // normal 4 KiB page
            _ => table_address = address,
        }
    }
    unreachable!()
}

#[derive(Debug)]
pub enum MapError {
    AlreadyMapped,
    OutOfFrames,
    HugePageInTheWay,
}

/// Maps the 4 KiB page at `virtual_address` to the frame at `physical_address`,
/// creating any missing intermediate page tables (with frames from the frame allocator).
pub fn map_page(virtual_address: u64, physical_address: u64, flags: u64) -> Result<(), MapError> {
    let indexes = table_indexes(virtual_address);
    let mut table_address = level4_table_address();

    // Levels 4, 3, 2: find (or create) the next table down.
    for &index in &indexes[..3] {
        // SAFETY: as in `translate`; interrupts don't touch page tables, and we hold
        // this reference only until we move to the next level.
        let table = unsafe { table_at(table_address) };
        let entry = &mut table.entries[index];
        if *entry & PRESENT == 0 {
            let frame = frame_allocator::allocate_frame().ok_or(MapError::OutOfFrames)?;
            // A fresh table must be all zeros, i.e. "nothing present".
            unsafe { core::ptr::write_bytes(phys_to_virt(frame), 0, FRAME_SIZE as usize) };
            *entry = frame | PRESENT | WRITABLE;
        } else if *entry & HUGE_PAGE != 0 {
            return Err(MapError::HugePageInTheWay);
        }
        table_address = *entry & ADDRESS_MASK;
    }

    // Level 1: the actual page → frame entry.
    let table = unsafe { table_at(table_address) };
    let entry = &mut table.entries[indexes[3]];
    if *entry & PRESENT != 0 {
        return Err(MapError::AlreadyMapped);
    }
    *entry = (physical_address & ADDRESS_MASK) | flags | PRESENT;

    // The CPU caches translations in the TLB (Translation Lookaside Buffer).
    // `invlpg` drops any stale cached entry for this page.
    unsafe { asm!("invlpg [{}]", in(reg) virtual_address, options(nostack, preserves_flags)) };
    Ok(())
}

/// Number of used entries in the level-4 table (each covers 512 GiB of address space).
pub fn level4_entries_used() -> usize {
    // SAFETY: CR3 points at the active level-4 table.
    let table = unsafe { table_at(level4_table_address()) };
    table.entries.iter().filter(|&&entry| entry & PRESENT != 0).count()
}
