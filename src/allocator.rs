//! Kernel heap (Milestone 9).
//!
//! We map a fixed range of virtual pages to fresh physical frames, then manage that
//! range with a linked-list allocator: every free chunk stores its own size and a
//! pointer to the next free chunk, inside the free memory itself.

use crate::frame_allocator::{self, FRAME_SIZE};
use crate::paging::{self, MapError, WRITABLE};
use core::alloc::{GlobalAlloc, Layout};
use core::mem::size_of;
use core::ptr;
use spin::Mutex;

/// An easy-to-spot virtual address, far away from everything else.
pub const HEAP_START: u64 = 0x4444_4444_0000;
pub const HEAP_SIZE: usize = 1024 * 1024; // 1 MiB

/// Maps the heap's pages to physical frames and hands the range to the allocator.
pub fn init_heap() -> Result<(), MapError> {
    let mut page = HEAP_START;
    while page < HEAP_START + HEAP_SIZE as u64 {
        let frame = frame_allocator::allocate_frame().ok_or(MapError::OutOfFrames)?;
        paging::map_page(page, frame, WRITABLE)?;
        page += FRAME_SIZE;
    }
    // SAFETY: the range was just mapped, is writable, and nothing else uses it.
    unsafe { ALLOCATOR.0.lock().add_free_region(HEAP_START as usize, HEAP_SIZE) };
    Ok(())
}

/// Bytes currently free in the heap.
pub fn free_bytes() -> usize {
    crate::interrupts::without_interrupts(|| ALLOCATOR.0.lock().free_bytes)
}

// ---------------------------------------------------------------------------
// Linked-list allocator
// ---------------------------------------------------------------------------

/// Header written at the start of every free chunk.
#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

/// Every chunk (free or used) is a multiple of 16 bytes and starts 16-byte aligned.
/// 16 is also the size of a `FreeBlock` header, so any leftover piece is either
/// empty or big enough to become a free chunk itself — nothing is ever lost.
const GRANULE: usize = 16;
const _: () = assert!(size_of::<FreeBlock>() == GRANULE);

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1) // `align` is always a power of two
}

pub struct LinkedListAllocator {
    /// First free chunk; the list is kept sorted by address so neighbours can merge.
    head: *mut FreeBlock,
    free_bytes: usize,
}

// SAFETY: the raw pointers only point into the heap, and access is guarded by a Mutex.
unsafe impl Send for LinkedListAllocator {}

impl LinkedListAllocator {
    const fn new() -> Self {
        LinkedListAllocator { head: ptr::null_mut(), free_bytes: 0 }
    }

    /// Adds `[address, address + size)` to the free list, merging it with the free
    /// chunks directly before and after it, so freed memory doesn't stay in crumbs.
    ///
    /// # Safety
    /// The range must be unused, writable heap memory, 16-byte aligned and sized.
    unsafe fn add_free_region(&mut self, address: usize, size: usize) {
        // Find the free chunks just before (`previous`) and after (`current`) the new one.
        let mut previous: *mut FreeBlock = ptr::null_mut();
        let mut current = self.head;
        unsafe {
            while !current.is_null() && (current as usize) < address {
                previous = current;
                current = (*current).next;
            }

            let block = address as *mut FreeBlock;
            block.write(FreeBlock { size, next: current });
            if previous.is_null() {
                self.head = block;
            } else {
                (*previous).next = block;
            }

            // Merge with the following chunk if they touch.
            if !current.is_null() && address + size == current as usize {
                (*block).size += (*current).size;
                (*block).next = (*current).next;
            }
            // Merge with the preceding chunk if they touch.
            if !previous.is_null() && previous as usize + (*previous).size == address {
                (*previous).size += (*block).size;
                (*previous).next = (*block).next;
            }
        }
        self.free_bytes += size;
    }

    /// First fit: use the first free chunk big enough for the request.
    unsafe fn allocate(&mut self, layout: Layout) -> *mut u8 {
        let size = align_up(layout.size().max(1), GRANULE);
        let align = layout.align().max(GRANULE);

        let mut previous: *mut FreeBlock = ptr::null_mut();
        let mut current = self.head;
        unsafe {
            while !current.is_null() {
                let chunk_start = current as usize;
                let chunk_end = chunk_start + (*current).size;
                let alloc_start = align_up(chunk_start, align);
                let alloc_end = alloc_start + size;

                if alloc_end <= chunk_end {
                    // Take the whole chunk out of the list...
                    let next = (*current).next;
                    if previous.is_null() {
                        self.head = next;
                    } else {
                        (*previous).next = next;
                    }
                    self.free_bytes -= chunk_end - chunk_start;
                    // ...and give back whatever we don't need, before and after.
                    if alloc_start > chunk_start {
                        self.add_free_region(chunk_start, alloc_start - chunk_start);
                    }
                    if chunk_end > alloc_end {
                        self.add_free_region(alloc_end, chunk_end - alloc_end);
                    }
                    return alloc_start as *mut u8;
                }
                previous = current;
                current = (*current).next;
            }
        }
        ptr::null_mut() // no chunk is big enough: out of heap memory
    }

    unsafe fn deallocate(&mut self, pointer: *mut u8, layout: Layout) {
        let size = align_up(layout.size().max(1), GRANULE);
        unsafe { self.add_free_region(pointer as usize, size) };
    }
}

// ---------------------------------------------------------------------------
// Hooking it into Rust's `alloc` crate
// ---------------------------------------------------------------------------

/// `GlobalAlloc` needs `&self`, so the allocator lives behind a lock.
pub struct KernelHeap(Mutex<LinkedListAllocator>);

// SAFETY: `allocate` returns suitably sized and aligned blocks of unused memory,
// and `deallocate` only accepts blocks it handed out (Rust guarantees the matching layout).
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Interrupts off while locked, so a handler that allocates can't deadlock us.
        crate::interrupts::without_interrupts(|| unsafe { self.0.lock().allocate(layout) })
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        crate::interrupts::without_interrupts(|| unsafe { self.0.lock().deallocate(pointer, layout) })
    }
}

/// Tells Rust: `Box`, `Vec`, `String`, ... get their memory from here.
#[global_allocator]
static ALLOCATOR: KernelHeap = KernelHeap(Mutex::new(LinkedListAllocator::new()));
