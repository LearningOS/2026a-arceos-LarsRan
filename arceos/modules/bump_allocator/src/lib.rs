#![no_std]

use allocator::{AllocError, AllocResult, BaseAllocator, ByteAllocator, PageAllocator};

/// Early memory allocator
/// Use it before formal bytes-allocator and pages-allocator can work!
/// This is a double-end memory range:
/// - Alloc bytes forward
/// - Alloc pages backward
///
/// [ bytes-used | avail-area | pages-used ]
/// |            | -->    <-- |            |
/// start       b_pos        p_pos       end
///
/// For bytes area, 'count' records number of allocations.
/// When it goes down to ZERO, free bytes-used area.
/// For pages area, it will never be freed!
///
pub struct EarlyAllocator<const SIZE: usize> {
    /// The start of the managed memory range.
    start: usize,
    /// The end of the managed memory range.
    end: usize,
    /// Cursor of the byte area, grows upwards from `start`.
    b_pos: usize,
    /// Number of live byte allocations. The byte area is reclaimed as a whole
    /// once this drops back to zero.
    b_count: usize,
    /// Cursor of the page area, grows downwards from `end`.
    p_pos: usize,
}

impl<const SIZE: usize> EarlyAllocator<SIZE> {
    pub const fn new() -> Self {
        Self {
            start: 0,
            end: 0,
            b_pos: 0,
            b_count: 0,
            p_pos: 0,
        }
    }
}

/// Rounds `addr` up to the next multiple of `align`, which must be a power of
/// two (guaranteed by [`core::alloc::Layout`]).
const fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

impl<const SIZE: usize> BaseAllocator for EarlyAllocator<SIZE> {
    fn init(&mut self, start: usize, size: usize) {
        self.start = start;
        self.end = start + size;
        self.b_pos = start;
        self.b_count = 0;
        self.p_pos = self.end;
    }

    fn add_memory(&mut self, _start: usize, _size: usize) -> AllocResult {
        // The early allocator owns a single contiguous range and is only used
        // before the real allocators are ready, so it cannot grow.
        Err(AllocError::MemoryOverlap)
    }
}

impl<const SIZE: usize> ByteAllocator for EarlyAllocator<SIZE> {
    fn alloc(&mut self, layout: core::alloc::Layout) -> AllocResult<core::ptr::NonNull<u8>> {
        let start = align_up(self.b_pos, layout.align());
        let end = start
            .checked_add(layout.size())
            .ok_or(AllocError::NoMemory)?;
        if end > self.p_pos {
            return Err(AllocError::NoMemory); // would collide with the page area
        }
        self.b_pos = end;
        self.b_count += 1;
        core::ptr::NonNull::new(start as *mut u8).ok_or(AllocError::InvalidParam)
    }

    fn dealloc(&mut self, _pos: core::ptr::NonNull<u8>, _layout: core::alloc::Layout) {
        self.b_count = self.b_count.saturating_sub(1);
        if self.b_count == 0 {
            // No live byte allocation, give the whole byte area back at once.
            self.b_pos = self.start;
        }
    }

    fn total_bytes(&self) -> usize {
        self.end - self.start
    }

    fn used_bytes(&self) -> usize {
        self.b_pos - self.start
    }

    fn available_bytes(&self) -> usize {
        self.p_pos - self.b_pos
    }
}

impl<const SIZE: usize> PageAllocator for EarlyAllocator<SIZE> {
    const PAGE_SIZE: usize = SIZE;

    fn alloc_pages(&mut self, num_pages: usize, align_pow2: usize) -> AllocResult<usize> {
        if align_pow2 < SIZE || !align_pow2.is_power_of_two() {
            return Err(AllocError::InvalidParam);
        }
        let size = num_pages
            .checked_mul(SIZE)
            .ok_or(AllocError::InvalidParam)?;
        let start = self.p_pos.checked_sub(size).ok_or(AllocError::NoMemory)?;
        let start = start & !(align_pow2 - 1);
        if start < self.b_pos {
            return Err(AllocError::NoMemory); // would collide with the byte area
        }
        self.p_pos = start;
        Ok(start)
    }

    fn dealloc_pages(&mut self, _pos: usize, _num_pages: usize) {
        // Pages are never reclaimed: `b_pos` may have already moved past them,
        // so the released range cannot be reused safely.
    }

    fn total_pages(&self) -> usize {
        (self.end - self.start) / SIZE
    }

    fn used_pages(&self) -> usize {
        (self.end - self.p_pos) / SIZE
    }

    fn available_pages(&self) -> usize {
        (self.p_pos - self.b_pos) / SIZE
    }
}
