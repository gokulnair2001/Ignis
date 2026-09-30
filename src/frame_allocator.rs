//! Physical memory manager (Milestone 7).
//!
//! Physical RAM is handed out in 4 KiB "frames". The bootloader tells us which parts
//! of RAM are free (its memory map); we track every frame with one bit in a bitmap.

use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use spin::Mutex;

pub const FRAME_SIZE: u64 = 4096;

/// We track up to 4 GiB of RAM (QEMU gives us 128 MiB by default).
const MAX_FRAMES: usize = (4 << 30) / FRAME_SIZE as usize; // 1,048,576 frames
const BITMAP_WORDS: usize = MAX_FRAMES / 64; // 64 frames per u64 → 128 KiB of bitmap

/// One bit per frame: 1 = free, 0 = used (or not RAM at all). Using 1 for "free"
/// means the all-zero starting state is "everything used", which is the safe default,
/// and an all-zero static costs no space in the kernel file.
pub struct FrameAllocator {
    bitmap: [u64; BITMAP_WORDS],
    free_frames: usize,
    usable_frames: usize,
    /// Where to start searching next time, so we don't rescan the full start each call.
    next_word: usize,
}

static FRAME_ALLOCATOR: Mutex<FrameAllocator> = Mutex::new(FrameAllocator {
    bitmap: [0; BITMAP_WORDS],
    free_frames: 0,
    usable_frames: 0,
    next_word: 0,
});

impl FrameAllocator {
    fn mark_free(&mut self, frame: usize) {
        self.bitmap[frame / 64] |= 1 << (frame % 64);
    }

    fn allocate(&mut self) -> Option<u64> {
        for offset in 0..BITMAP_WORDS {
            let word_index = (self.next_word + offset) % BITMAP_WORDS;
            let word = self.bitmap[word_index];
            if word != 0 {
                // Lowest set bit = first free frame in this group of 64.
                let bit = word.trailing_zeros() as usize;
                self.bitmap[word_index] &= !(1 << bit);
                self.free_frames -= 1;
                self.next_word = word_index;
                return Some((word_index * 64 + bit) as u64 * FRAME_SIZE);
            }
        }
        None // out of memory
    }

    fn deallocate(&mut self, address: u64) {
        let frame = (address / FRAME_SIZE) as usize;
        let mask = 1 << (frame % 64);
        assert!(self.bitmap[frame / 64] & mask == 0, "double free of frame {:#x}", address);
        self.bitmap[frame / 64] |= mask;
        self.free_frames += 1;
    }
}

/// Reads the bootloader's memory map and marks every `Usable` frame as free.
pub fn init(memory_map: &MemoryMap) {
    let mut allocator = FRAME_ALLOCATOR.lock();
    for region in memory_map.iter() {
        if region.region_type != MemoryRegionType::Usable {
            continue;
        }
        let start = region.range.start_frame_number as usize;
        let end = (region.range.end_frame_number as usize).min(MAX_FRAMES);
        for frame in start..end {
            allocator.mark_free(frame);
        }
        let count = end.saturating_sub(start);
        allocator.free_frames += count;
        allocator.usable_frames += count;
    }
}

/// Hands out one free 4 KiB physical frame, returning its physical address.
pub fn allocate_frame() -> Option<u64> {
    FRAME_ALLOCATOR.lock().allocate()
}

/// Returns a frame previously given out by `allocate_frame`.
pub fn deallocate_frame(address: u64) {
    FRAME_ALLOCATOR.lock().deallocate(address);
}

/// (free frames, usable frames)
pub fn stats() -> (usize, usize) {
    let allocator = FRAME_ALLOCATOR.lock();
    (allocator.free_frames, allocator.usable_frames)
}

/// Logs the bootloader's memory map over serial, one line per region.
pub fn log_memory_map(memory_map: &MemoryMap) {
    crate::serial_println!("[ignis] physical memory map from the bootloader:");
    for region in memory_map.iter() {
        let start = region.range.start_addr();
        let end = region.range.end_addr();
        crate::serial_println!(
            "  {:#012x} - {:#012x}  {:>8} KiB  {:?}",
            start,
            end,
            (end - start) / 1024,
            region.region_type
        );
    }
}
