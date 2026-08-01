#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

mod cspace;
mod gdt;
mod heap;
mod idt;
mod mem;
mod paging;
mod pic;
mod scheduler;
mod serial;
mod syscall;
mod timer;
mod untyped;
mod usermode;

use abi::{Capability, KernelObjectId, ObjectType, Rights};
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

    address_space_selftest(&mut com1);

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

    scheduled_address_space_selftest(&mut com1);

    cspace_selftest(&mut com1);

    usermode_selftest(&mut com1)
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

    let kernel_as = paging::kernel_address_space();
    let phys = mem::FRAME_ALLOCATOR
        .lock()
        .alloc()
        .expect("rose: paging self-test: out of memory");

    unsafe {
        kernel_as.map_page(TEST_VIRT, phys, paging::PAGE_WRITABLE | paging::PAGE_NO_EXECUTE);
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
        kernel_as.unmap_page(TEST_VIRT);
        mem::FRAME_ALLOCATOR.lock().free(phys);
    }
    let _ = writeln!(com1, "rose: paging self-test: unmapped, frame returned to allocator");
}

/// Builds a second address space alongside the kernel's own, proves the
/// kernel side can't see a page mapped only in the new one and vice
/// versa (via `is_mapped`, a read-only walk, so proving "not mapped"
/// never risks a self-inflicted page fault from touching the address
/// directly), switches CR3 into it, writes and reads back a pattern
/// through the now-valid virtual pointer, then switches back to the
/// kernel. First reversible address-space switch in the project, done
/// entirely in ring 0 so it's safe either direction. No explicit
/// initial switch back to the kernel address space is needed going in,
/// since `paging::init()` already put it there at boot.
fn address_space_selftest(com1: &mut serial::Serial) {
    // Distinct from usermode.rs's 0x400000/0x600000 on purpose, though a
    // collision would be harmless anyway: different PML4 roots mean the
    // same virtual address in two AddressSpaces maps to two entirely
    // separate leaf PTEs.
    const TEST_VIRT: u64 = 0x0000_0000_0020_0000;
    const PATTERN: u64 = 0xfeed_face_1234_5678;

    let kernel_as = paging::kernel_address_space();
    let user_as = unsafe { paging::new_address_space() };

    let phys = mem::FRAME_ALLOCATOR
        .lock()
        .alloc()
        .expect("rose: address space self-test: out of memory");
    unsafe {
        user_as.map_page(TEST_VIRT, phys, paging::PAGE_WRITABLE | paging::PAGE_NO_EXECUTE);
    }

    let isolated = unsafe { !kernel_as.is_mapped(TEST_VIRT) && user_as.is_mapped(TEST_VIRT) };
    let _ = writeln!(
        com1,
        "rose: address space self-test: isolation {}",
        if isolated { "confirmed" } else { "FAILED" }
    );

    unsafe {
        user_as.switch();
    }
    let ptr = TEST_VIRT as *mut u64;
    unsafe {
        ptr.write_volatile(PATTERN);
    }
    let readback = unsafe { ptr.read_volatile() };
    unsafe {
        kernel_as.switch();
    }
    let _ = writeln!(
        com1,
        "rose: address space self-test: wrote {:#018x}, read {:#018x}, {}",
        PATTERN,
        readback,
        if readback == PATTERN { "match" } else { "MISMATCH" }
    );

    unsafe {
        user_as.unmap_page(TEST_VIRT);
        mem::FRAME_ALLOCATOR.lock().free(phys);
    }
    let _ = writeln!(
        com1,
        "rose: address space self-test: unmapped, frame returned to allocator, back on kernel address space"
    );
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

// TSS.rsp0 target: the stack the CPU auto-loads on the user self-test's
// ring3->ring0 syscall trap. Same pattern as gdt.rs's own
// DOUBLE_FAULT_STACK, a separate static rather than reusing whatever
// stack kernel_main happens to be running on, since that one is still
// in use by the very call frame that's about to go one-way into ring 3.
const KERNEL_SCRATCH_STACK_SIZE: usize = 4096 * 4;
static mut KERNEL_SCRATCH_STACK: [u8; KERNEL_SCRATCH_STACK_SIZE] = [0; KERNEL_SCRATCH_STACK_SIZE];

/// Maps a code page and a stack page as USER, copies the hand-written
/// asm program from usermode.rs into the code page, points TSS.rsp0 at
/// a kernel scratch stack, then drops to ring 3. Never returns:
/// enter_user_mode is one-way in this v0.1 cut, and on_syscall's second
/// call is what actually halts the CPU for good. See usermode.rs module
/// docs. This is why it's the last thing kernel_main calls, and why the
/// old trailing `loop { hlt }` that used to sit here is gone, it would
/// have been unreachable code once this diverges instead.
fn usermode_selftest(com1: &mut serial::Serial) -> ! {
    // Both arbitrary, page-aligned, and far below the kernel's
    // higher-half virtual range and the HHDM range alike, guaranteeing
    // a fresh PML4 branch (see paging.rs's next_level PAGE_USER fix;
    // nothing already mapped up here could collide).
    const CODE_VADDR: u64 = 0x0000_0000_0040_0000;
    const STACK_VADDR: u64 = 0x0000_0000_0060_0000;

    // A real second AddressSpace now, not the kernel's currently-active
    // one (see address_space_selftest above, first thing to exercise
    // this primitive). CR3 doesn't point here yet, so the code page
    // below can't be written by dereferencing CODE_VADDR directly.
    let user_as = unsafe { paging::new_address_space() };

    let code_phys = mem::FRAME_ALLOCATOR
        .lock()
        .alloc()
        .expect("rose: usermode self-test: out of memory (code)");
    unsafe {
        // R+X+U from the start, no WRITABLE ever: the write below goes
        // through the frame's permanent HHDM alias instead of through
        // CODE_VADDR, so there's no supervisor-write-to-non-writable-
        // page fault to work around anymore (that was only ever a
        // problem when the kernel wrote into its own currently-active
        // mapping of the code page; see BUGS.md for the original
        // ring3 version of this bug, now obsolete but left in place).
        user_as.map_page(CODE_VADDR, code_phys, paging::PAGE_USER);
    }
    let program = usermode::program_bytes();
    unsafe {
        let code_virt = paging::phys_to_virt(code_phys) as *mut u8;
        core::ptr::copy_nonoverlapping(program.as_ptr(), code_virt, program.len());
    }

    let stack_phys = mem::FRAME_ALLOCATOR
        .lock()
        .alloc()
        .expect("rose: usermode self-test: out of memory (stack)");
    unsafe {
        user_as.map_page(
            STACK_VADDR,
            stack_phys,
            paging::PAGE_USER | paging::PAGE_WRITABLE | paging::PAGE_NO_EXECUTE,
        );
    }
    let user_stack_top = STACK_VADDR + mem::FRAME_SIZE;

    unsafe {
        let scratch_top =
            core::ptr::addr_of!(KERNEL_SCRATCH_STACK) as u64 + KERNEL_SCRATCH_STACK_SIZE as u64;
        gdt::set_kernel_stack(scratch_top);
    }

    // Boot handoff stand-in: task 0's own CSpace slot 1 gets a root
    // cap over the kernel AddressSpace, exactly like cspace_selftest's
    // grant_root above but through the scheduler-owned CSpace this
    // time, since it's task 0's copy that the ring-3 program's own
    // syscalls (see usermode.rs) will copy/mint/move/revoke against.
    // Without this, the first CSpace syscall the program issues would
    // just come back SourceEmpty against an empty slot 1.
    let full_rights = Rights::READ.union(Rights::WRITE).union(Rights::MAP);
    let root_cap = Capability {
        object_ref: KernelObjectId(paging::kernel_address_space().raw()),
        object_type: ObjectType::AddressSpace,
        rights: full_rights,
        badge: 0,
    };
    scheduler::with_current_cspace(|cspace| cspace.grant_root(1, root_cap))
        .expect("rose: usermode self-test: cspace grant_root failed");

    // Same boot-handoff stand-in, extended for Retype: task 0's own
    // CSpace slot 6 gets a root cap over a fresh 2-frame Untyped pool
    // (untyped.rs), so the ring-3 program's four Retype steps (see
    // usermode.rs) have something real to retype from instead of
    // hitting InvalidUntyped against an empty slot 6. Two frames, not
    // one: the sequence deliberately retypes twice successfully (a
    // Frame and a CSpace) before a third retype is expected to hit
    // Exhausted, so the pool has to run out exactly then, not sooner.
    let untyped_object = untyped::UntypedObject::new(2)
        .expect("rose: usermode self-test: out of memory (untyped pool)");
    let untyped_id = untyped::register_untyped(untyped_object);
    let untyped_cap = Capability {
        object_ref: KernelObjectId(untyped_id),
        object_type: ObjectType::Untyped,
        rights: full_rights,
        badge: 0,
    };
    scheduler::with_current_cspace(|cspace| cspace.grant_root(6, untyped_cap))
        .expect("rose: usermode self-test: cspace grant_root (untyped) failed");

    let _ = writeln!(
        com1,
        "rose: usermode self-test: entering ring 3 at {:#018x}, stack top {:#018x}",
        CODE_VADDR, user_stack_top
    );

    // Handing off to a manual CR3 switch alone would leave task 0's own
    // Task.address_space field pointing at the kernel's own AddressSpace
    // (set once at scheduler::init(), never touched since); the next
    // real preemption would reload CR3 from that stale field, straight
    // out from under this still-running ring-3 program. Updating the
    // field and reloading CR3 together via set_current_address_space
    // keeps the two in sync. See BUGS.md, "real hardware timer
    // preemption during ring-3 self-test reverts CR3 to the kernel
    // AddressSpace".
    scheduler::set_current_address_space(user_as);
    unsafe { usermode::enter_user_mode(CODE_VADDR, user_stack_top) }
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

const SCHEDULED_AS_TEST_VIRT: u64 = 0x0000_0000_0030_0000;
const SCHEDULED_AS_PATTERN_A: u64 = 0x1111_1111_2222_2222;
const SCHEDULED_AS_PATTERN_B: u64 = 0x3333_3333_4444_4444;
static TASK_C_RESULT: AtomicU64 = AtomicU64::new(0);
static TASK_D_RESULT: AtomicU64 = AtomicU64::new(0);
static SCHEDULED_AS_TASKS_DONE: AtomicU64 = AtomicU64::new(0);

// Same virtual address as task_d, deliberately: the whole point is that
// each of these runs under a different CR3, so the two writes land in
// two entirely separate physical frames despite sharing a vaddr. Writes
// its own pattern once, yields several times so real round-robin
// switching (including task_a/task_b/task 0 also in the ring by this
// point) happens in between, then reads back and stores the result for
// the parent to check. Loops forever afterward rather than parking,
// same reason as task_a/task_b above: a task stuck outside yield_now
// would freeze the whole ring.
fn task_c() {
    let ptr = SCHEDULED_AS_TEST_VIRT as *mut u64;
    unsafe {
        ptr.write_volatile(SCHEDULED_AS_PATTERN_A);
    }
    for _ in 0..5u64 {
        scheduler::yield_now();
    }
    let readback = unsafe { ptr.read_volatile() };
    TASK_C_RESULT.store(readback, Ordering::Relaxed);
    SCHEDULED_AS_TASKS_DONE.fetch_add(1, Ordering::Relaxed);
    loop {
        scheduler::yield_now();
    }
}

fn task_d() {
    let ptr = SCHEDULED_AS_TEST_VIRT as *mut u64;
    unsafe {
        ptr.write_volatile(SCHEDULED_AS_PATTERN_B);
    }
    for _ in 0..5u64 {
        scheduler::yield_now();
    }
    let readback = unsafe { ptr.read_volatile() };
    TASK_D_RESULT.store(readback, Ordering::Relaxed);
    SCHEDULED_AS_TASKS_DONE.fetch_add(1, Ordering::Relaxed);
    loop {
        scheduler::yield_now();
    }
}

/// Proves the scheduler actually reloads CR3 per task, not just that a
/// single manual `AddressSpace::switch()` works in isolation (already
/// covered by `address_space_selftest`). Builds two AddressSpaces, maps
/// the same virtual address in each to a different physical frame with
/// a different pattern, then spawns one task into each via
/// `scheduler::spawn_in`. By the time both report done, they've been
/// preempted and resumed several times each, interleaved with task_a,
/// task_b, and task 0's own wait loop, all of which keep running in the
/// kernel's own AddressSpace throughout. If either task ever read back
/// the other's pattern, or the kernel's own code somehow faulted mid-
/// ring, CR3 wasn't actually following the scheduler.
///
/// No unmap/free of the two test frames afterward: task_c/task_d loop
/// forever like task_a/task_b do, so the mappings need to stay valid
/// for the rest of boot anyway. Same no-teardown scope cut as
/// everywhere else address spaces show up so far.
fn scheduled_address_space_selftest(com1: &mut serial::Serial) {
    let as_a = unsafe { paging::new_address_space() };
    let as_b = unsafe { paging::new_address_space() };

    let phys_a = mem::FRAME_ALLOCATOR
        .lock()
        .alloc()
        .expect("rose: scheduled address space self-test: out of memory");
    let phys_b = mem::FRAME_ALLOCATOR
        .lock()
        .alloc()
        .expect("rose: scheduled address space self-test: out of memory");

    unsafe {
        as_a.map_page(
            SCHEDULED_AS_TEST_VIRT,
            phys_a,
            paging::PAGE_WRITABLE | paging::PAGE_NO_EXECUTE,
        );
        as_b.map_page(
            SCHEDULED_AS_TEST_VIRT,
            phys_b,
            paging::PAGE_WRITABLE | paging::PAGE_NO_EXECUTE,
        );
    }

    scheduler::spawn_in(task_c, as_a);
    scheduler::spawn_in(task_d, as_b);

    while SCHEDULED_AS_TASKS_DONE.load(Ordering::Relaxed) < 2 {
        scheduler::yield_now();
    }

    let a_ok = TASK_C_RESULT.load(Ordering::Relaxed) == SCHEDULED_AS_PATTERN_A;
    let b_ok = TASK_D_RESULT.load(Ordering::Relaxed) == SCHEDULED_AS_PATTERN_B;
    let _ = writeln!(
        com1,
        "rose: scheduled address space self-test: task_c read {:#018x} ({}), task_d read {:#018x} ({}), isolation across scheduling {}",
        TASK_C_RESULT.load(Ordering::Relaxed),
        if a_ok { "match" } else { "MISMATCH" },
        TASK_D_RESULT.load(Ordering::Relaxed),
        if b_ok { "match" } else { "MISMATCH" },
        if a_ok && b_ok { "confirmed" } else { "FAILED" }
    );
}

/// Proves the derivation tree in cspace.rs actually works, not just
/// that a single grant round-trips. Grants a root capability over the
/// kernel's own AddressSpace (standing in for the real "Boot handoff"
/// root grant until there's a real root task to receive it), copies
/// it to a sibling slot, mints a narrowed-rights badged child into a
/// third slot, then revokes the original and checks all three slots
/// emptied out. No syscall involved; this calls cspace::CSpace's
/// methods directly, see the module doc in cspace.rs for what's still
/// missing before that's possible.
fn cspace_selftest(com1: &mut serial::Serial) {
    let mut cspace = cspace::CSpace::new();
    let kernel_as = paging::kernel_address_space();
    let full_rights = Rights::READ.union(Rights::WRITE).union(Rights::MAP);
    let root_cap = Capability {
        object_ref: KernelObjectId(kernel_as.raw()),
        object_type: ObjectType::AddressSpace,
        rights: full_rights,
        badge: 0,
    };

    cspace
        .grant_root(1, root_cap)
        .expect("rose: cspace self-test: grant_root failed");
    cspace
        .copy(1, 2, full_rights)
        .expect("rose: cspace self-test: copy failed");

    // Re-home the copy from slot 2 to slot 4. Exercises move_slot's
    // tree-repointing: slot 2 must go empty and slot 4 must still be
    // tracked as a child of slot 1, so a later revoke(1) still reaches
    // it under its new name.
    cspace
        .move_slot(2, 4)
        .expect("rose: cspace self-test: move_slot failed");
    let move_cleared_src = cspace.lookup(2).is_none();

    let minted_rights = Rights::READ;
    let minted_badge: u64 = 0xc0ffee;
    cspace
        .mint(1, 3, minted_rights, minted_badge)
        .expect("rose: cspace self-test: mint failed");

    let cap1 = cspace.lookup(1).expect("rose: cspace self-test: slot 1 missing before revoke");
    let cap4 = cspace.lookup(4).expect("rose: cspace self-test: slot 4 missing before revoke");
    let cap3 = cspace.lookup(3).expect("rose: cspace self-test: slot 3 missing before revoke");

    let refs_match = cap1.object_ref == cap4.object_ref && cap4.object_ref == cap3.object_ref;
    let mint_narrowed =
        cap3.rights == minted_rights && cap3.badge == minted_badge && cap3.rights != cap1.rights;

    cspace
        .revoke(1)
        .expect("rose: cspace self-test: revoke failed");

    let all_cleared =
        cspace.lookup(1).is_none() && cspace.lookup(3).is_none() && cspace.lookup(4).is_none();

    let ok = move_cleared_src && refs_match && mint_narrowed && all_cleared;
    let _ = writeln!(
        com1,
        "rose: cspace self-test: move cleared source slot {}, copy/mint object_ref {}, mint narrowed rights+badge {}, revoke cascade cleared moved+minted+root {}, overall {}",
        if move_cleared_src { "ok" } else { "FAILED" },
        if refs_match { "match" } else { "MISMATCH" },
        if mint_narrowed { "ok" } else { "FAILED" },
        if all_cleared { "ok" } else { "FAILED" },
        if ok { "confirmed" } else { "FAILED" }
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
