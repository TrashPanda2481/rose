// Kernel heap, v0.1.
//
// Backs `extern crate alloc` (Box, Vec, etc.) for kernel-internal use only;
// this has nothing to do with per-component memory later, that's Untyped
// retyping through the capability model in docs/cores/kernel/README.md.
//
// Fixed virtual region, eagerly mapped in full at init() via the kernel
// address space's map_page, one physical frame per page from the frame
// allocator. No growth-on-demand
// yet: if the heap fills up, alloc returns null like any other GlobalAlloc
// failure. Revisit once something actually needs more than HEAP_SIZE at
// once; nothing does yet.
//
// Free-list, first-fit, same technique as mem.rs's frame allocator: no
// separate bookkeeping array, metadata lives inside the memory it describes.
// Every block (free or allocated) starts with an 8-byte size header holding
// its own usable byte count. A free block additionally stores the next
// free block's header address in the first 8 bytes of its usable area,
// same free-list-node-inside-the-freed-memory trick as the frame allocator.
// Blocks are split on alloc when the leftover is big enough to be its own
// block; freed blocks are never coalesced with neighbors. No coalescing
// means alternating alloc/dealloc of different sizes will fragment over
// time; acceptable for v0.1 since nothing but the self-test uses this yet.
//
// Alignment: only guarantees 8-byte alignment (the header size), since the
// heap region and every block start on an 8-byte boundary but not
// necessarily 16. A layout asking for more than that fails alloc outright
// rather than silently violating it. Nothing needs more yet; anything that
// eventually does (DMA buffers, etc.) almost certainly wants its own
// frame-backed allocation instead of squeezing through this heap anyway.

use core::alloc::{GlobalAlloc, Layout};

use crate::mem::{FRAME_ALLOCATOR, FRAME_SIZE, SpinLock};
use crate::paging;

const HEAP_START: u64 = 0xffff_9000_0000_0000;
const HEAP_SIZE: u64 = 1024 * 1024; // 1 MiB, v0.1 fixed size
const HEADER_SIZE: u64 = 8;
const MIN_USABLE: u64 = 8; // must fit a free-list "next" pointer
const NO_NEXT: u64 = u64::MAX;
const MAX_SUPPORTED_ALIGN: u64 = 8;

struct Heap {
    free_head: u64,
    free_bytes: u64,
    allocated_bytes: u64,
}

impl Heap {
    const fn empty() -> Self {
        Self {
            free_head: NO_NEXT,
            free_bytes: 0,
            allocated_bytes: 0,
        }
    }
}

static HEAP: SpinLock<Heap> = SpinLock::new(Heap::empty());

unsafe fn read_size(header_addr: u64) -> u64 {
    unsafe { (header_addr as *const u64).read_volatile() }
}

unsafe fn write_size(header_addr: u64, size: u64) {
    unsafe { (header_addr as *mut u64).write_volatile(size) };
}

unsafe fn read_next(header_addr: u64) -> u64 {
    unsafe { ((header_addr + HEADER_SIZE) as *const u64).read_volatile() }
}

unsafe fn write_next(header_addr: u64, next: u64) {
    unsafe { ((header_addr + HEADER_SIZE) as *mut u64).write_volatile(next) };
}

fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

/// Maps the whole heap region up front and seeds the free list with one
/// block spanning it. Safety: caller must ensure the frame allocator and
/// paging are already initialized, and this runs exactly once.
pub unsafe fn init() {
    let kernel_as = paging::kernel_address_space();
    let pages = HEAP_SIZE / FRAME_SIZE;
    for i in 0..pages {
        let phys = FRAME_ALLOCATOR
            .lock()
            .alloc()
            .expect("rose: heap init: out of memory");
        unsafe {
            kernel_as.map_page(
                HEAP_START + i * FRAME_SIZE,
                phys,
                paging::PAGE_WRITABLE | paging::PAGE_NO_EXECUTE,
            );
        }
    }

    let mut heap = HEAP.lock();
    let usable = HEAP_SIZE - HEADER_SIZE;
    unsafe {
        write_size(HEAP_START, usable);
        write_next(HEAP_START, NO_NEXT);
    }
    heap.free_head = HEAP_START;
    heap.free_bytes = usable;
    heap.allocated_bytes = 0;
}

pub fn free_bytes() -> u64 {
    HEAP.lock().free_bytes
}

pub fn allocated_bytes() -> u64 {
    HEAP.lock().allocated_bytes
}

/// Walks the free list for the first block with usable size >= `required`,
/// splitting off the leftover as a new free block when it's worth keeping.
/// Returns the header address of the (now allocated) block.
unsafe fn alloc_block(heap: &mut Heap, required: u64) -> Option<u64> {
    let mut prev: Option<u64> = None;
    let mut cur = heap.free_head;

    while cur != NO_NEXT {
        let size = unsafe { read_size(cur) };
        let next = unsafe { read_next(cur) };

        if size >= required {
            let remainder = size - required;
            if remainder >= HEADER_SIZE + MIN_USABLE {
                let new_block = cur + HEADER_SIZE + required;
                unsafe {
                    write_size(new_block, remainder - HEADER_SIZE);
                    write_next(new_block, next);
                    write_size(cur, required);
                }
                relink(heap, prev, new_block);
                heap.free_bytes -= required + HEADER_SIZE;
            } else {
                relink(heap, prev, next);
                heap.free_bytes -= size;
            }
            heap.allocated_bytes += unsafe { read_size(cur) };
            return Some(cur);
        }

        prev = Some(cur);
        cur = next;
    }

    None
}

/// Points whatever came before `old_cur_replacement`'s original slot at the
/// new value: either the head pointer (if there was no previous block) or
/// the previous block's next pointer.
fn relink(heap: &mut Heap, prev: Option<u64>, new_value: u64) {
    match prev {
        Some(prev_addr) => unsafe { write_next(prev_addr, new_value) },
        None => heap.free_head = new_value,
    }
}

unsafe fn dealloc_block(heap: &mut Heap, header_addr: u64) {
    let size = unsafe { read_size(header_addr) };
    unsafe { write_next(header_addr, heap.free_head) };
    heap.free_head = header_addr;
    heap.free_bytes += size;
    heap.allocated_bytes -= size;
}

struct KernelHeap;

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.align() as u64 > MAX_SUPPORTED_ALIGN || layout.size() == 0 {
            return core::ptr::null_mut();
        }
        let required = core::cmp::max(align_up(layout.size() as u64, 8), MIN_USABLE);
        let mut heap = HEAP.lock();
        match unsafe { alloc_block(&mut heap, required) } {
            Some(header_addr) => (header_addr + HEADER_SIZE) as *mut u8,
            None => core::ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        let header_addr = ptr as u64 - HEADER_SIZE;
        let mut heap = HEAP.lock();
        unsafe { dealloc_block(&mut heap, header_addr) };
    }
}

#[global_allocator]
static ALLOCATOR: KernelHeap = KernelHeap;

#[alloc_error_handler]
fn alloc_error(layout: Layout) -> ! {
    // Same direct-serial approach as the panic handler in main.rs: this can
    // fire before/without anything else being in a known-good state, so it
    // talks to the UART itself rather than going through any shared state.
    use core::fmt::Write;
    let mut com1 = crate::serial::Serial::init();
    let _ = writeln!(com1, "rose: PANIC: heap allocation failed: {:?}", layout);
    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}
