#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

mod gdt;
mod heap;
mod idt;
mod mem;
mod paging;
mod pic;
mod scheduler;
mod serial;
mod timer;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt::Write;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};
use limine::BaseRevision;
use limine::RequestsEndMarker;
use limine::RequestsStartMarker;
use limine::memmap;
use limine::request::ExecutableAddressRequest;
use limine::request::HhdmRequest;
use limine::request::MemmapRequest;

#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[unsafe(link_section = ".requests")]
static MEMMAP_REQUEST: MemmapRequest = MemmapRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub(crate) static EXECUTABLE_ADDRESS_REQUEST: ExecutableAddressRequest =
    ExecutableAddressRequest::new();

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START_MARKER: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END_MARKER: RequestsEndMarker = RequestsEndMarker::new();

/// This is the only place authority comes from nothing (see kernel core
/// invariants doc, boot handoff section). Limine hands control here.
#[unsafe(no_mangle)]
unsafe extern "C" fn kernel_main() -> ! {
    assert!(BASE_REVISION.is_supported());

    let mut com1 = serial::Serial::init();
    let _ = writeln!(com1, "rose: boot ok");
    let _ = writeln!(com1, "rose: base revision supported");
    let _ = writeln!(com1, "rose: hello, hardware");

    let memmap_response = MEMMAP_REQUEST
        .response()
        .expect("rose: bootloader gave no memory map");
    let entries = memmap_response.entries();
    let _ = writeln!(com1, "rose: memory map: {} entries", entries.len());

    let mut usable_bytes: u64 = 0;
    for entry in entries {
        let _ = writeln!(
            com1,
            "rose:   base={:#018x} length={:#012x} type={}",
            entry.base,
            entry.length,
            memmap_type_name(entry.type_)
        );
        if entry.type_ == memmap::MEMMAP_USABLE {
            usable_bytes += entry.length;
        }
    }
    let _ = writeln!(com1, "rose: usable memory: {} KiB", usable_bytes / 1024);

    let hhdm_response = HHDM_REQUEST
        .response()
        .expect("rose: bootloader gave no HHDM offset");
    let hhdm_offset = hhdm_response.offset;
    let _ = writeln!(com1, "rose: hhdm offset={:#018x}", hhdm_offset);

    unsafe {
        mem::init(hhdm_offset, entries);
    }

    frame_allocator_selftest(&mut com1);

    unsafe {
        gdt::init();
        idt::init();
    }
    let _ = writeln!(com1, "rose: gdt/idt loaded");

    idt_selftest(&mut com1);

    unsafe {
        paging::init(hhdm_offset, entries);
    }
    let _ = writeln!(com1, "rose: page tables loaded, kernel-owned");

    paging_selftest(&mut com1);

    unsafe {
        heap::init();
    }
    let _ = writeln!(com1, "rose: heap loaded, {} bytes free", heap::free_bytes());

    heap_selftest(&mut com1);

    // Must happen before pic::init()/timer::init() ever let IF go high:
    // scheduler::tick() runs unconditionally from every timer IRQ, and
    // on real hardware (unlike this sandbox's QEMU/TCG) the first tick
    // can land the instant sti executes. An empty task list at that
    // point used to panic (modulo by zero); see BUGS.md. Registering
    // task 0 here first means tick() always has at least one task, so
    // it's a safe no-op switch until spawn() adds more later.
    unsafe {
        scheduler::init();
    }

    unsafe {
        pic::init();
    }
    let _ = writeln!(com1, "rose: pic remapped, irq0 unmasked");

    unsafe {
        timer::init();
    }
    let _ = writeln!(com1, "rose: timer programmed at 100hz, interrupts enabled");

    timer_selftest(&mut com1);

    scheduler_selftest(&mut com1);

    loop {
        core::arch::asm!("hlt");
    }
}

/// Deliberately triggers a breakpoint exception. If GDT/IDT/TSS are wired
/// correctly, the handler in idt.rs logs it and returns here normally,
/// proving the whole chain works without having to crash the kernel to
/// prove it, same spirit as the frame allocator self-test above.
fn idt_selftest(com1: &mut serial::Serial) {
    let _ = writeln!(com1, "rose: idt self-test: triggering breakpoint");
    unsafe {
        core::arch::asm!("int3");
    }
    let _ = writeln!(com1, "rose: idt self-test: resumed after breakpoint, ok");
}

/// Allocates a frame, maps it at a throwaway virtual address, writes a
/// pattern through the new mapping, reads it back, unmaps, and confirms
/// the frame goes back to the allocator cleanly. Proves map_page/unmap_page
/// work on the kernel's own tables, not just that the CR3 switch succeeded
/// without immediately faulting.
fn paging_selftest(com1: &mut serial::Serial) {
    const TEST_VIRT: u64 = 0xffff_a000_0000_0000;
    const PATTERN: u64 = 0xdead_beef_cafe_f00d;

    let phys = mem::FRAME_ALLOCATOR
        .lock()
        .alloc()
        .expect("rose: paging self-test: out of memory");

    unsafe {
        paging::map_page(TEST_VIRT, phys, paging::PAGE_WRITABLE | paging::PAGE_NO_EXECUTE);
    }

    let ptr = TEST_VIRT as *mut u64;
    unsafe {
        ptr.write_volatile(PATTERN);
    }
    let readback = unsafe { ptr.read_volatile() };
    let _ = writeln!(
        com1,
        "rose: paging self-test: wrote {:#018x}, read {:#018x}, {}",
        PATTERN,
        readback,
        if readback == PATTERN { "match" } else { "MISMATCH" }
    );

    unsafe {
        paging::unmap_page(TEST_VIRT);
        mem::FRAME_ALLOCATOR.lock().free(phys);
    }
    let _ = writeln!(com1, "rose: paging self-test: unmapped, frame returned to allocator");
}

/// Exercises the heap through the actual `alloc` crate types it exists to
/// support, not just the raw GlobalAlloc entry points: a Box (single fixed
/// allocation) and a Vec that's pushed past its initial capacity (forces
/// at least one grow, which is alloc+copy+dealloc under the hood via
/// GlobalAlloc's default `realloc`). Everything goes out of scope at the
/// end of this function, so the free-byte count logged after should match
/// the one logged right after heap::init().
fn heap_selftest(com1: &mut serial::Serial) {
    let boxed = Box::new(0xdead_beef_u64);
    let _ = writeln!(com1, "rose: heap self-test: boxed value = {:#x}", *boxed);

    let mut v: Vec<u64> = Vec::new();
    for i in 0..64u64 {
        v.push(i * i);
    }
    let sum: u64 = v.iter().sum();
    let _ = writeln!(
        com1,
        "rose: heap self-test: vec len={} sum={}",
        v.len(),
        sum
    );
    let _ = writeln!(
        com1,
        "rose: heap self-test: {} bytes allocated, {} bytes free",
        heap::allocated_bytes(),
        heap::free_bytes()
    );

    drop(boxed);
    drop(v);
    let _ = writeln!(
        com1,
        "rose: heap self-test: after drop, {} bytes allocated, {} bytes free",
        heap::allocated_bytes(),
        heap::free_bytes()
    );
}

fn rdtsc() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        core::arch::asm!("rdtsc", out("eax") low, out("edx") high, options(nomem, nostack));
    }
    ((high as u64) << 32) | (low as u64)
}

/// Waits for 50 ticks (0.5s at 100Hz) to go by, proving PIC remap + IDT
/// vector 32 dispatch + PIT programming all work end to end via a real
/// hardware interrupt. Bounded by an RDTSC-measured ~2 real seconds
/// rather than waiting unboundedly: this project's QEMU/TCG sandbox (see
/// BUGS.md) never delivers IRQ0 to the CPU despite correct
/// PIC/PIT programming, so an unbounded wait here would hang boot
/// forever there. If the timeout is hit, falls back to a
/// software-triggered `int 0x20` to at least confirm the IDT dispatch,
/// PIC EOI, and tick-counting path are wired correctly, which is
/// everything except the actual hardware delivery.
///
/// The cycle budget below uses an assumed CEILING on host TSC frequency,
/// not a floor: cycle_budget / actual_hz only comes out to >= WAIT_SECONDS
/// if actual_hz <= the assumed value. An earlier version of this used a
/// 500MHz floor here, which on a real multi-GHz host made the whole wait
/// bail out after a few hundred ms instead of the intended 2s, so a
/// working hardware timer (confirmed via a VirtualBox boot where the
/// tick counter visibly advanced) still got reported as "no hardware
/// irq0" because the test gave up too early. See BUGS.md.
fn timer_selftest(com1: &mut serial::Serial) {
    const MAX_ASSUMED_HZ: u64 = 8_000_000_000;
    const WAIT_SECONDS: u64 = 2;
    let cycle_budget = MAX_ASSUMED_HZ * WAIT_SECONDS;

    let start_ticks = timer::ticks();
    let target_ticks = start_ticks + 50;
    let start_tsc = rdtsc();

    // Countdown printed to serial so a real wait (this loop can run
    // several real seconds on a host slower than the assumed ceiling,
    // see the comment above) doesn't look identical to a hang. Divides
    // the assumed-seconds budget into WAIT_SECONDS chunks and prints
    // once per chunk crossed; ticks against the same TSC/ceiling math
    // the loop's own bailout uses, not a real-time clock, so on a host
    // slower than the ceiling the printed countdown runs slower than
    // an actual second per step.
    let mut seconds_left = WAIT_SECONDS;
    let mut next_countdown_at = cycle_budget / WAIT_SECONDS;

    while timer::ticks() < target_ticks {
        let elapsed = rdtsc().wrapping_sub(start_tsc);
        if elapsed > cycle_budget {
            break;
        }
        if elapsed >= next_countdown_at && seconds_left > 0 {
            seconds_left -= 1;
            let _ = writeln!(com1, "rose: timer self-test: {}s remaining...", seconds_left);
            next_countdown_at += cycle_budget / WAIT_SECONDS;
        }
        core::hint::spin_loop();
    }

    if timer::ticks() >= target_ticks {
        let _ = writeln!(
            com1,
            "rose: timer self-test: {} ticks elapsed via hardware irq0, {}ms since boot",
            timer::ticks() - start_ticks,
            timer::ms_since_boot()
        );
    } else {
        unsafe {
            core::arch::asm!("int 0x20");
        }
        let _ = writeln!(
            com1,
            "rose: timer self-test: no hardware irq0 within ~{}s, verified vector 32 dispatch via software int instead (ticks={})",
            WAIT_SECONDS,
            timer::ticks()
        );
    }
}

static TASK_A_COUNT: AtomicU64 = AtomicU64::new(0);
static TASK_B_COUNT: AtomicU64 = AtomicU64::new(0);
static TASKS_DONE: AtomicU64 = AtomicU64::new(0);

// Round-robin never skips a task that stops yielding; it just always
// advances to (current+1) % len, whether or not that task ever calls
// yield_now again. So after signaling done, these keep yielding forever
// instead of parking in a spin loop; parking here would freeze every
// other ring member, including task 0's own wait loop below, since
// nothing would ever rotate control past a task stuck outside
// yield_now. See BUGS.md, "scheduler self-test hangs after first task
// finishes".
fn task_a() {
    for _ in 0..10u64 {
        TASK_A_COUNT.fetch_add(1, Ordering::Relaxed);
        scheduler::yield_now();
    }
    TASKS_DONE.fetch_add(1, Ordering::Relaxed);
    loop {
        scheduler::yield_now();
    }
}

fn task_b() {
    for _ in 0..10u64 {
        TASK_B_COUNT.fetch_add(1, Ordering::Relaxed);
        scheduler::yield_now();
    }
    TASKS_DONE.fetch_add(1, Ordering::Relaxed);
    loop {
        scheduler::yield_now();
    }
}

/// Spawns two tasks that each count to 10 and voluntarily yield in
/// between, then cooperatively yields itself (task 0) until both are
/// done. Fully deterministic round-robin ping-pong (task0 -> task_a ->
/// task_b -> task0 -> ...) regardless of whether hardware timer
/// interrupts fire at all, since it's entirely voluntary. Real hardware
/// ticks, where they land (confirmed working in VirtualBox, not in this
/// sandbox's QEMU/TCG, see TROUBLESHOOTING.md), interleave harmlessly
/// through the same round-robin rule tick() and yield_now() share, no
/// special-casing needed. This is the portable proof point; hardware
/// preemption itself is only visible opportunistically via switches()
/// counting higher than the 20 voluntary yields below would alone.
fn scheduler_selftest(com1: &mut serial::Serial) {
    // scheduler::init() already ran earlier in kernel_main, before
    // interrupts were enabled; calling it again here would double-push
    // task 0.
    scheduler::spawn(task_a);
    scheduler::spawn(task_b);

    while TASKS_DONE.load(Ordering::Relaxed) < 2 {
        scheduler::yield_now();
    }

    let _ = writeln!(
        com1,
        "rose: scheduler self-test: task_a={} task_b={} switches={}",
        TASK_A_COUNT.load(Ordering::Relaxed),
        TASK_B_COUNT.load(Ordering::Relaxed),
        scheduler::switches()
    );
}

fn frame_allocator_selftest(com1: &mut serial::Serial) {
    let mut allocator = mem::FRAME_ALLOCATOR.lock();
    let _ = writeln!(
        com1,
        "rose: frame allocator: {} total frames, {} free",
        allocator.total_count(),
        allocator.free_count()
    );

    let a = allocator.alloc();
    let b = allocator.alloc();
    let c = allocator.alloc();
    let _ = writeln!(com1, "rose: alloc test: got {:?} {:?} {:?}", a, b, c);

    if let Some(phys) = b {
        unsafe {
            allocator.free(phys);
        }
    }
    let d = allocator.alloc();
    let _ = writeln!(
        com1,
        "rose: alloc after free: got {:?}, expected same as freed frame",
        d
    );

    let _ = writeln!(
        com1,
        "rose: frame allocator: {} free after self-test",
        allocator.free_count()
    );
}

/// Human-readable label for a Limine memmap entry type. Serial-log only for
/// now; the frame allocator built on top of this will care about usable vs.
/// not, not the exact label.
fn memmap_type_name(type_: u64) -> &'static str {
    match type_ {
        memmap::MEMMAP_USABLE => "usable",
        memmap::MEMMAP_RESERVED => "reserved",
        memmap::MEMMAP_ACPI_RECLAIMABLE => "acpi_reclaimable",
        memmap::MEMMAP_ACPI_NVS => "acpi_nvs",
        memmap::MEMMAP_BAD_MEMORY => "bad_memory",
        memmap::MEMMAP_BOOTLOADER_RECLAIMABLE => "bootloader_reclaimable",
        memmap::MEMMAP_EXECUTABLE_AND_MODULES => "kernel_and_modules",
        memmap::MEMMAP_FRAMEBUFFER => "framebuffer",
        memmap::MEMMAP_MAPPED_RESERVED => "mapped_reserved",
        _ => "unknown",
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Panic path has to work even before any real driver exists, so it
    // talks to the UART directly rather than going through anything else.
    let mut com1 = serial::Serial::init();
    let _ = writeln!(com1, "rose: PANIC: {}", info);
    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}
