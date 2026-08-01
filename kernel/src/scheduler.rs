// Scheduler, v0.1.
//
// Single fixed-priority round-robin queue, one timeslice for everyone,
// preempt on timer tick or on voluntary yield. No priority handling yet
// (see README, "Scheduler, v0.1" section); this is deliberately just
// enough to prove context switching and preemption work at all, same
// spirit as the heap's "no coalescing" simplification.
//
// Task 0 is not spawned; it's whatever was already running when
// `init()` is called (the boot/kernel_main execution context), reusing
// the existing boot stack. Every other task is spawned onto its own
// heap-allocated stack via `spawn()`.
//
// Two entry points switch tasks: `tick()`, called from inside the timer
// IRQ handler (IF already 0, interrupt gate), and `yield_now()`, called
// voluntarily from normal task context (IF=1). Both pick the same next
// task via round-robin and drive the same `context_switch`; they differ
// only in how they handle interrupts around the switch, see the comments
// on each below.
//
// Each Task now also carries the AddressSpace it runs in. `spawn`
// defaults to the kernel's own, matching every task before this
// feature; `spawn_in` picks a specific one. Both tick() and
// yield_now() reload CR3 for the incoming task right before
// context_switch, unconditionally for now (no same-AS skip yet, see
// the comment at each call site). This is what makes it possible for
// a future component to actually run isolated in its own AddressSpace
// under real preemption, not just a single manual switch.

use crate::cspace::CSpace;
use crate::mem::SpinLock;
use crate::paging::{self, AddressSpace};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

const TIMESLICE_TICKS: u32 = 5; // 50ms at 100Hz
const STACK_SIZE: usize = 16 * 1024;

struct Task {
    saved_rsp: u64,
    // None for task 0 (uses the pre-existing boot stack, nothing to
    // own/free here). Some(..) for spawned tasks: the Box keeps the
    // stack alive for as long as the Task exists, since nothing else
    // holds a reference to it.
    _stack: Option<Box<[u8]>>,
    // Every task before this feature ran in the kernel's own
    // AddressSpace implicitly, since nothing here ever touched CR3.
    // Now each task carries the one it should be running in; `spawn`
    // defaults to the kernel's, `spawn_in` picks any. Copy type (a
    // PML4 physical address), cheap to hold by value.
    address_space: AddressSpace,
    // Every task gets its own empty CSpace at creation. Nothing is
    // reachable through a syscall until something (boot handoff today,
    // a parent task later) explicitly grants a root capability into
    // it; see cspace.rs's own doc comment ("if it's not in your
    // CSpace, it doesn't exist for you").
    cspace: CSpace,
}

struct Scheduler {
    tasks: Vec<Task>,
    current: usize,
    ticks_left: u32,
}

static SCHEDULER: SpinLock<Scheduler> = SpinLock::new(Scheduler {
    tasks: Vec::new(),
    current: 0,
    ticks_left: TIMESLICE_TICKS,
});

static SWITCHES: AtomicU64 = AtomicU64::new(0);

/// Registers task 0 (the caller's own current execution context) as the
/// first entry in the run queue. Safety: call exactly once, before any
/// `spawn`, `tick`, or `yield_now`.
pub unsafe fn init() {
    let mut scheduler = SCHEDULER.lock();
    scheduler.tasks.push(Task {
        saved_rsp: 0,
        _stack: None,
        address_space: paging::kernel_address_space(),
        cspace: CSpace::new(),
    });
    scheduler.current = 0;
    scheduler.ticks_left = TIMESLICE_TICKS;
}

/// Allocates a stack for `entry` and adds it to the run queue, returning
/// its index. Only safe to call during boot setup, before any of these
/// tasks can actually be preempted: this takes indices/a lock on the
/// `tasks` Vec, and a concurrent `tick()`/`yield_now()` reading `tasks`
/// mid-push would be a data race with the Vec's own reallocation. Every
/// spawn call in this kernel happens from `kernel_main` before any
/// switch is possible, which satisfies that.
pub fn spawn(entry: fn()) -> usize {
    spawn_in(entry, paging::kernel_address_space())
}

/// Same as `spawn`, but the task runs in `address_space` instead of the
/// kernel's own. `address_space` must already cover whatever `entry`
/// touches before its first `yield_now`/preemption; see
/// `paging::new_address_space` and its callers for how to build and
/// populate one ahead of time.
pub fn spawn_in(entry: fn(), address_space: AddressSpace) -> usize {
    let mut stack: Box<[u8]> = alloc::vec![0u8; STACK_SIZE].into_boxed_slice();
    let stack_top = stack.as_mut_ptr() as u64 + STACK_SIZE as u64;
    // Fabricate the initial saved-context frame by hand, matching what
    // context_switch's own push/pop sequence expects to find. Written
    // downward from stack_top, so the first value here ends up at the
    // highest address and is the last thing popped (i.e. the return
    // address `ret` uses).
    //
    // Layout, high to low address:
    //   task_trampoline   <- return address for context_switch's `ret`
    //   entry as u64      <- lands in the "rbx" slot, popped last
    //   0 (rbp)
    //   0 (r12)
    //   0 (r13)
    //   0 (r14)
    //   0 (r15)           <- lowest address, popped first, becomes sp
    //
    // context_switch pops r15, r14, r13, r12, rbp, rbx, then `ret`s. So
    // rbx is popped last, right before the ret pulls the trampoline
    // address off the stack. task_trampoline reads its entry point out
    // of rbx; that's the trick that gets `entry` into the trampoline
    // without any other shared state.
    //
    // Written top-down, in the same order as this list (task_trampoline
    // first, landing at the highest address; r15 last, landing at the
    // lowest address, which becomes the final saved_rsp). No reversal:
    // the first entry here must end up furthest from the eventual sp,
    // since it's the last thing popped.
    let frame = [
        task_trampoline as *const () as u64,
        entry as *const () as u64,
        0u64, // rbp
        0u64, // r12
        0u64, // r13
        0u64, // r14
        0u64, // r15
    ];
    let mut sp = stack_top;
    for value in frame.iter() {
        sp -= 8;
        unsafe {
            (sp as *mut u64).write(*value);
        }
    }

    let mut scheduler = SCHEDULER.lock();
    scheduler.tasks.push(Task {
        saved_rsp: sp,
        _stack: Some(stack),
        address_space,
        cspace: CSpace::new(),
    });
    scheduler.tasks.len() - 1
}

/// Runs `f` against whichever task is current at the moment of the
/// call, i.e. the one that trapped in via the syscall path calling
/// this. Locks the same SCHEDULER lock as tick()/yield_now(); safe to
/// call from the syscall dispatch path since that only ever runs with
/// interrupts enabled from ring 3 (not from inside tick()'s own
/// already-locked section).
pub fn with_current_cspace<R>(f: impl FnOnce(&mut CSpace) -> R) -> R {
    let mut scheduler = SCHEDULER.lock();
    let current = scheduler.current;
    f(&mut scheduler.tasks[current].cspace)
}

/// Called only from the timer IRQ handler in idt.rs, once per tick, with
/// IF already 0 (interrupt gate). Decrements the current task's
/// timeslice and switches to the next task round-robin once it runs
/// out. No cli/sti needed: the resumed task's own path back out (either
/// this same interrupt path's `iretq`, or a later `yield_now`) is what
/// restores IF correctly for whenever it's scheduled again.
///
/// `init()` must run before interrupts are ever enabled, so `tasks` is
/// never actually empty here in a correctly-ordered boot. Guarded
/// anyway rather than trusting that ordering: this runs from inside a
/// real hardware interrupt on real machines, where a tick can land at
/// any instruction boundary the instant IF goes high, not just at
/// times this kernel's own code chooses to poll something. A modulo by
/// an empty queue would panic and halt the whole machine over what's
/// really just a boot-sequencing question, not a correctness one.
pub fn tick() {
    let (old_rsp_ptr, new_rsp, next_as) = {
        let mut scheduler = SCHEDULER.lock();
        if scheduler.tasks.is_empty() {
            return;
        }
        scheduler.ticks_left = scheduler.ticks_left.saturating_sub(1);
        if scheduler.ticks_left > 0 {
            return;
        }
        scheduler.ticks_left = TIMESLICE_TICKS;

        let old_index = scheduler.current;
        let next_index = (old_index + 1) % scheduler.tasks.len();
        scheduler.current = next_index;

        if old_index == next_index {
            return;
        }

        let old_rsp_ptr = &mut scheduler.tasks[old_index].saved_rsp as *mut u64;
        let new_rsp = scheduler.tasks[next_index].saved_rsp;
        let next_as = scheduler.tasks[next_index].address_space;
        (old_rsp_ptr, new_rsp, next_as)
        // Lock guard drops here, before the switch; context_switch
        // never returns to this stack frame until this task is
        // rescheduled, so holding the lock across it would deadlock
        // every other task's tick()/yield_now() in the meantime.
    };

    // Always reloads CR3, even when next_as is the same one already
    // loaded (true for every task today, since nothing yet calls
    // spawn_in with anything but the kernel's own AddressSpace).
    // switch() already does an unconditional full TLB flush regardless
    // of whether the PML4 actually changed, so skipping this on a
    // same-AS switch would only save the mov-to-cr3 itself, not the
    // flush; not worth the AddressSpace-equality check yet. Revisit if
    // this ever shows up as a real cost once components actually run
    // in separate spaces.
    unsafe {
        next_as.switch();
        context_switch(old_rsp_ptr, new_rsp);
    }
    SWITCHES.fetch_add(1, Ordering::Relaxed);
}

/// Voluntary version, callable from any normal task context with IF=1.
/// Brackets the whole selection+switch in cli/sti by hand:
///
/// - `cli` first: without it, a real timer tick could land while this
///   function holds the SCHEDULER lock below, and tick() would then
///   spin forever trying to re-lock it; the only thing that could ever
///   release that lock is this exact call, which is itself now paused
///   inside the interrupt it's spinning in. Single CPU, so that's a
///   guaranteed deadlock, not just a possible one.
/// - `sti` immediately after context_switch returns: a `ret`-based
///   switch never touches RFLAGS, so without this every task resumed
///   via yield_now would run forever with interrupts disabled, and
///   nothing would ever preempt it again.
pub fn yield_now() {
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack, preserves_flags));
    }

    let (old_rsp_ptr, new_rsp, next_as) = {
        let mut scheduler = SCHEDULER.lock();
        let old_index = scheduler.current;
        let next_index = (old_index + 1) % scheduler.tasks.len();
        scheduler.current = next_index;
        scheduler.ticks_left = TIMESLICE_TICKS;

        if old_index == next_index {
            unsafe {
                core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
            }
            return;
        }

        let old_rsp_ptr = &mut scheduler.tasks[old_index].saved_rsp as *mut u64;
        let new_rsp = scheduler.tasks[next_index].saved_rsp;
        let next_as = scheduler.tasks[next_index].address_space;
        (old_rsp_ptr, new_rsp, next_as)
    };

    // See the matching comment in tick(): always reloads CR3 for now,
    // same-AS case not special-cased yet.
    unsafe {
        next_as.switch();
        context_switch(old_rsp_ptr, new_rsp);
    }
    SWITCHES.fetch_add(1, Ordering::Relaxed);

    unsafe {
        core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
    }
}

pub fn switches() -> u64 {
    SWITCHES.load(Ordering::Relaxed)
}

/// xv6/swtch-style context switch: saves the callee-saved registers not
/// already preserved by the SysV ABI's own call/ret, swaps rsp, restores
/// the same set for whatever was previously saved at the new rsp, and
/// returns into it. Args arrive in rdi/rsp per SysV ABI since the caller
/// (tick/yield_now) is normal, non-naked Rust code.
#[unsafe(naked)]
unsafe extern "C" fn context_switch(old_rsp_ptr: *mut u64, new_rsp: u64) {
    core::arch::naked_asm!(
        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov [rdi], rsp",
        "mov rsp, rsi",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",
        "ret",
    )
}

/// Landing pad for a task's very first run. Never reached via a normal
/// call; spawn() fabricates a stack that makes context_switch's `ret`
/// land here directly, with the intended entry point sitting in rbx.
///
/// `sti` here is required because a cold-started task never goes
/// through iretq (that's only the interrupt-resume path) and never
/// goes through yield_now's own sti (this is its first instruction, not
/// a resume); so this is the one place a fresh task has to explicitly
/// turn its own interrupts back on.
///
/// No task-exit mechanism exists yet: v0.1 scope, same as the heap's
/// documented "no coalescing" simplification. A task that returns from
/// its entry function just parks in the halt loop below.
#[unsafe(naked)]
extern "C" fn task_trampoline() -> ! {
    core::arch::naked_asm!(
        "sti",
        "call rbx",
        "2:",
        "hlt",
        "jmp 2b",
    )
}
