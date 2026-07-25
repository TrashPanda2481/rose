// Physical frame allocator, v0.1.
//
// Free-list over 4KiB frames. Each free frame stores the physical address
// of the next free frame in its own first 8 bytes; the allocator itself
// just holds the head pointer. No separate bookkeeping region needed.
//
// Addressing: physical memory is touched through the HHDM (Higher-Half
// Direct Map) offset Limine provides, not assumed identity-mapped. Virtual
// address for a physical address is `phys + hhdm_offset`.
//
// Only MEMMAP_USABLE regions feed the free list. Bootloader-reclaimable
// memory is left alone for now; reclaiming it safely means being done
// reading every Limine response first, which isn't tracked yet. Revisit
// once boot handoff is real.
//
// Locking: wrapped in a plain spinlock even though nothing runs concurrently
// yet (no interrupts, no scheduler). Cheap to add now, and it means this
// doesn't need revisiting once interrupts/SMP show up.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use limine::memmap;

pub const FRAME_SIZE: u64 = 4096;

/// No free frame; used as the free-list's end-of-list sentinel instead of
/// `Option` so each free frame only needs to store one u64.
const NO_NEXT: u64 = u64::MAX;

pub struct FrameAllocator {
    hhdm_offset: u64,
    free_head: u64,
    free_count: u64,
    total_count: u64,
}

impl FrameAllocator {
    const fn empty() -> Self {
        Self {
            hhdm_offset: 0,
            free_head: NO_NEXT,
            free_count: 0,
            total_count: 0,
        }
    }

    /// Walks the usable regions of the memory map and builds the free list.
    /// Safety: caller must guarantee `hhdm_offset` is the real HHDM offset
    /// from Limine's response, and that this runs exactly once, before
    /// anything else touches physical memory through this allocator.
    unsafe fn init(&mut self, hhdm_offset: u64, entries: &[&memmap::Entry]) {
        self.hhdm_offset = hhdm_offset;

        for entry in entries {
            if entry.type_ != memmap::MEMMAP_USABLE {
                continue;
            }
            let start = align_up(entry.base, FRAME_SIZE);
            let end = align_down(entry.base + entry.length, FRAME_SIZE);

            let mut frame = start;
            while frame < end {
                self.push_free(frame);
                frame += FRAME_SIZE;
            }
        }
    }

    unsafe fn push_free(&mut self, phys: u64) {
        let slot = (phys + self.hhdm_offset) as *mut u64;
        slot.write_volatile(self.free_head);
        self.free_head = phys;
        self.free_count += 1;
        self.total_count += 1;
    }

    /// Returns the physical address of a free frame, or None if out of memory.
    pub fn alloc(&mut self) -> Option<u64> {
        if self.free_head == NO_NEXT {
            return None;
        }
        let phys = self.free_head;
        let slot = (phys + self.hhdm_offset) as *const u64;
        self.free_head = unsafe { slot.read_volatile() };
        self.free_count -= 1;
        Some(phys)
    }

    /// Returns a frame to the free list. Safety: caller must guarantee
    /// `phys` was handed out by `alloc` and nothing else is still using it.
    pub unsafe fn free(&mut self, phys: u64) {
        let slot = (phys + self.hhdm_offset) as *mut u64;
        slot.write_volatile(self.free_head);
        self.free_head = phys;
        self.free_count += 1;
    }

    pub fn free_count(&self) -> u64 {
        self.free_count
    }

    pub fn total_count(&self) -> u64 {
        self.total_count
    }
}

fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

fn align_down(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}

/// Minimal spinlock. Not the standard `spin` crate on purpose, one more
/// external dependency to version-pin for something this small isn't worth
/// it; see TROUBLESHOOTING.md for what version drift already cost once.
pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

unsafe impl<T> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        SpinLockGuard { lock: self }
    }
}

pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<'a, T> core::ops::Deref for SpinLockGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.value.get() }
    }
}

impl<'a, T> core::ops::DerefMut for SpinLockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<'a, T> Drop for SpinLockGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

pub static FRAME_ALLOCATOR: SpinLock<FrameAllocator> = SpinLock::new(FrameAllocator::empty());

/// Safety: same contract as `FrameAllocator::init` — call once, with a real
/// HHDM offset, before anything else allocates.
pub unsafe fn init(hhdm_offset: u64, entries: &[&memmap::Entry]) {
    FRAME_ALLOCATOR.lock().init(hhdm_offset, entries);
}
