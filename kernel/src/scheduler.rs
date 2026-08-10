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
use crate::gdt;
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
    // The top of this task's own, private ring0 entry stack: the value
    // TSS.RSP0 must hold whenever this task is the one that might trap
    // from ring 3. Every task before this feature shared one single
    // global RSP0 (set once in main.rs), which was safe as long as
    // only one ring-3-resident task ever existed at a time; once a
    // second one showed up, both traps used the same buffer and could
    // clobber each other's parked resume state, including the
    // hardware-pushed exception frame, on a preemption mid-handler.
    // See BUGS.md's now-fixed entry on the post-syscall page fault.
    // Task 0 gets a real value at boot via `set_current_kernel_stack`,
    // not just 0; every spawned task gets its own already-allocated
    // stack's top, the same one `saved_rsp` is fabricated against.
    kernel_stack_top: u64,
    // Every task gets its own CSpace at creation. Nothing is
    // reachable through a syscall until something (boot handoff today,
    // a parent task later) explicitly grants a root capability into
    // it; see cspace.rs's own doc comment ("if it's not in your
    // CSpace, it doesn't exist for you"). Boxed rather than inline:
    // `spawn_user` takes ownership of a caller-supplied CSpace
    // (thread::configure, via untyped::take_cspace) rather than always
    // building a fresh one, and a Box is what lets that already-boxed
    // object move straight in without an extra copy.
    cspace: Box<CSpace>,
    // Set by `block_current_and_switch` (endpoint.rs's Send/Receive,
    // v0.1 increment 1), cleared by `wake`. A blocked task is skipped
    // by `next_runnable_index` on every future tick()/yield_now()/
    // block_current_and_switch() selection, until something wakes it
    // back up. Always false for every task before this feature; no
    // existing caller ever sets it.
    blocked: bool,
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

/// Scans forward from `from` (exclusive) for the first task that
/// isn't `blocked`, wrapping around `tasks` at most once. Falls back
/// to `from` itself if nothing else qualifies (every other task
/// blocked, or `tasks.len() == 1`): a documented v0.1 gap, same
/// spirit as this scheduler's other known cuts (no priority, no
/// starvation handling). Every task spawned by `spawn`/`spawn_in`
/// (the self-test's own task_a/task_b/task_c/task_d loops) never
/// blocks; they only ever yield, so `block_current_and_switch` always
/// has at least one such candidate to find as long as any of those
/// still exist in the run queue. Replaces the old bare `(from + 1) %
/// len` used by `tick()`/`yield_now()` before this feature, which had
/// no notion of a task being ineligible to run.
fn next_runnable_index(tasks: &[Task], from: usize) -> usize {
    let len = tasks.len();
    for offset in 1..=len {
        let candidate = (from + offset) % len;
        if !tasks[candidate].blocked {
            return candidate;
        }
    }
    from
}

/// Reads the caller's current RFLAGS, clears IF, and returns the
/// pre-cli flags so `restore_flags` can put things back exactly as
/// they were. Used to bracket every SCHEDULER-locking critical section
/// that isn't already known by construction to run at a single fixed
/// IF state (unlike `set_current_address_space`/
/// `set_current_kernel_stack`, each of which has exactly one caller
/// confirmed to always run at IF=1, a blind cli/sti pair is correct
/// and simpler there). `with_current_cspace`/`spawn`/`spawn_in`/
/// `spawn_user` all have call sites at both IF=1 (normal ring-0 setup
/// code, e.g. main.rs's boot-handoff grants) and IF=0 (from inside the
/// vector-0x80 syscall interrupt gate, idt.rs, which clears IF for its
/// whole handler) -- a blind `sti` in the IF=0 case would incorrectly
/// re-enable interrupts mid-handler, before its own `iretq` restores
/// the real saved state. Save-and-restore is correct for both cases
/// without needing to know which one a given call is.
#[inline(always)]
unsafe fn save_flags_and_cli() -> u64 {
    let flags: u64;
    unsafe {
        core::arch::asm!(
            "pushfq",
            "pop {}",
            "cli",
            out(reg) flags,
        );
    }
    flags
}

/// Restores IF to whatever `save_flags_and_cli` observed -- `sti` only
/// if the caller actually had interrupts enabled to begin with, a
/// no-op otherwise. See `save_flags_and_cli`'s doc comment.
#[inline(always)]
unsafe fn restore_flags(flags: u64) {
    if flags & (1 << 9) != 0 {
        unsafe {
            core::arch::asm!("sti");
        }
    }
}

/// Registers task 0 (the caller's own current execution context) as the
/// first entry in the run queue. Safety: call exactly once, before any
/// `spawn`, `tick`, or `yield_now`.
pub unsafe fn init() {
    let mut scheduler = SCHEDULER.lock();
    scheduler.tasks.push(Task {
        saved_rsp: 0,
        _stack: None,
        address_space: paging::kernel_address_space(),
        kernel_stack_top: 0,
        cspace: Box::new(CSpace::new()),
        blocked: false,
    });
    scheduler.current = 0;
    scheduler.ticks_left = TIMESLICE_TICKS;
}

/// Allocates a stack for `entry` and adds it to the run queue, returning
/// its index.
///
/// SU1 fix: this doc comment used to claim it's "only safe to call
/// during boot setup, before any of these tasks can actually be
/// preempted" -- that was already false by the time
/// `scheduled_address_space_selftest` started calling `spawn_in` well
/// after `pic::init()`/`timer::init()` had enabled interrupts (see
/// main.rs). Without a cli guard around the lock+push below, a real
/// timer tick landing mid-critical-section made `tick()` spin forever
/// on this same non-reentrant SCHEDULER lock from inside an
/// interrupt-gate ISR -- a permanent hang, single core, nothing else
/// could ever release it. `save_flags_and_cli`/`restore_flags` (same
/// helper `with_current_cspace` uses) closes that window; safe to call
/// with interrupts already on (the actual case today) or already off.
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

    let flags = unsafe { save_flags_and_cli() };
    let index = {
        let mut scheduler = SCHEDULER.lock();
        scheduler.tasks.push(Task {
            saved_rsp: sp,
            _stack: Some(stack),
            address_space,
            kernel_stack_top: stack_top,
            cspace: Box::new(CSpace::new()),
            blocked: false,
        });
        scheduler.tasks.len() - 1
    };
    unsafe { restore_flags(flags) };
    index
}

/// Builds a brand-new task that lands directly in ring 3 on its first
/// run, instead of `spawn_in`'s ring-0 `entry: fn()`. Used exclusively
/// by `thread::configure`: a Configure syscall supplies its own
/// `address_space` (already mapped with whatever the caller wants
/// `entry`/`user_stack_top` to point at) and `cspace` (either a fresh
/// one thread::configure built, or one taken from a caller-supplied
/// CSpace cap via `untyped::take_cspace`), and this just wires those
/// into a schedulable Task the same way `spawn_in` does for a kernel
/// task.
///
/// Same fabricated-frame trick as `spawn_in`, with two changes: the
/// return address is `user_task_trampoline` instead of
/// `task_trampoline`, and the rbp slot (always 0 in `spawn_in`'s
/// frame, since a ring-0 `fn()` entry takes no arguments) carries
/// `user_stack_top` instead. `user_task_trampoline` reads `entry` out
/// of rbx (same convention as `task_trampoline`) and `user_stack_top`
/// out of rbp, then builds the iretq frame from those plus the fixed
/// ring-3 GDT selectors.
pub fn spawn_user(
    entry: u64,
    user_stack_top: u64,
    address_space: AddressSpace,
    cspace: Box<CSpace>,
) -> usize {
    let mut stack: Box<[u8]> = alloc::vec![0u8; STACK_SIZE].into_boxed_slice();
    let stack_top = stack.as_mut_ptr() as u64 + STACK_SIZE as u64;
    let frame = [
        user_task_trampoline as *const () as u64,
        entry,
        user_stack_top, // rbp slot, repurposed; see doc comment above
        0u64,           // r12
        0u64,           // r13
        0u64,           // r14
        0u64,           // r15
    ];
    let mut sp = stack_top;
    for value in frame.iter() {
        sp -= 8;
        unsafe {
            (sp as *mut u64).write(*value);
        }
    }

    // SU1-class fix, same reasoning as spawn_in: cli-guard the lock.
    // spawn_user's one real caller (thread::configure, via
    // dispatch_configure) runs from inside the vector-0x80 interrupt
    // handler itself (IF already 0 there, a genuine interrupt gate),
    // so this has to be save_flags_and_cli/restore_flags, not a blind
    // cli/sti pair -- a blind `sti` here would incorrectly turn
    // interrupts back on before the handler's own `iretq` restores
    // the real saved state, same hazard `with_current_cspace` has.
    let flags = unsafe { save_flags_and_cli() };
    let index = {
        let mut scheduler = SCHEDULER.lock();
        scheduler.tasks.push(Task {
            saved_rsp: sp,
            _stack: Some(stack),
            address_space,
            kernel_stack_top: stack_top,
            cspace,
            blocked: false,
        });
        scheduler.tasks.len() - 1
    };
    unsafe { restore_flags(flags) };
    index
}

/// Runs `f` against whichever task is current at the moment of the
/// call. Locks the same SCHEDULER lock as tick()/yield_now().
///
/// SU2 fix: called from two genuinely different interrupt contexts,
/// which is exactly why this needs `save_flags_and_cli`/
/// `restore_flags` rather than a blind cli/sti pair. From
/// syscall.rs's dispatch_* functions, this runs from inside the
/// vector-0x80 handler itself -- a real interrupt gate (idt.rs,
/// `Entry::set_user`, type 0xE), so IF is already 0 for the whole
/// handler regardless of the interrupted context's own saved rflags;
/// a blind `sti` at the end here would incorrectly turn interrupts
/// back on in the middle of that handler, before its own `iretq`
/// restores the real saved state. From main.rs's boot-handoff
/// `grant_root` calls, this runs as normal ring-0 code with IF=1
/// already, well after `pic::init()`/`timer::init()` -- there,
/// without cli, a real timer tick landing mid-critical-section would
/// make `tick()` spin forever on this same non-reentrant SpinLock
/// from inside an interrupt-gate ISR (IF stuck at 0), a permanent
/// hang, since nothing else on this single core could ever release
/// it. save_flags_and_cli/restore_flags is correct for both: it
/// disables interrupts unconditionally for the critical section, then
/// only re-enables them if they were actually on to begin with.
pub fn with_current_cspace<R>(f: impl FnOnce(&mut CSpace) -> R) -> R {
    let flags = unsafe { save_flags_and_cli() };
    let result = {
        let mut scheduler = SCHEDULER.lock();
        let current = scheduler.current;
        f(&mut scheduler.tasks[current].cspace)
    };
    unsafe { restore_flags(flags) };
    result
}

/// Updates whichever task is current at the moment of the call to run
/// in `new_as` from now on, and reloads CR3 into it immediately, both
/// under the same cli/sti bracket so no real timer tick can land
/// between the two and see a mismatched pair. Needed for exactly one
/// caller today: `usermode_selftest`'s manual switch into a second
/// AddressSpace right before dropping to ring 3. Without this, the
/// Task's own `address_space` field (whatever `init`/`spawn` set it to)
/// never learns about the change, and the next real preemption reloads
/// CR3 from that stale field instead, straight back to whatever it was
/// before, out from under a still-running ring-3 program. Same
/// cli/sti reasoning as `yield_now`'s own bracket: a tick landing
/// mid-update could see a half-written Task, or spin forever trying to
/// re-lock SCHEDULER against itself.
pub fn set_current_address_space(new_as: AddressSpace) {
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack, preserves_flags));
    }
    {
        let mut scheduler = SCHEDULER.lock();
        let current = scheduler.current;
        scheduler.tasks[current].address_space = new_as;
    }
    unsafe {
        new_as.switch();
        core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
    }
}

/// Updates whichever task is current at the moment of the call to trap
/// through `rsp0` (TSS.RSP0) from now on, and applies it to the TSS
/// immediately, both under the same cli/sti bracket as
/// `set_current_address_space` and for the identical reason: without
/// applying it immediately, a real timer tick landing between the
/// Task-field update and the TSS write could resume this same task via
/// a stale TSS.RSP0 still pointing at whatever the previous holder left
/// it as. Needed for exactly one caller today: `main.rs`'s boot path,
/// replacing its old one-time raw `gdt::set_kernel_stack` call so task
/// 0's own `kernel_stack_top` field is recorded, not just applied once
/// and then forgotten by the scheduler.
pub fn set_current_kernel_stack(rsp0: u64) {
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack, preserves_flags));
    }
    {
        let mut scheduler = SCHEDULER.lock();
        let current = scheduler.current;
        scheduler.tasks[current].kernel_stack_top = rsp0;
    }
    unsafe {
        gdt::set_kernel_stack(rsp0);
        core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
    }
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
    let (old_rsp_ptr, new_rsp, next_as, next_kernel_stack) = {
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
        let next_index = next_runnable_index(&scheduler.tasks, old_index);
        scheduler.current = next_index;

        if old_index == next_index {
            return;
        }

        let old_rsp_ptr = &mut scheduler.tasks[old_index].saved_rsp as *mut u64;
        let new_rsp = scheduler.tasks[next_index].saved_rsp;
        let next_as = scheduler.tasks[next_index].address_space;
        let next_kernel_stack = scheduler.tasks[next_index].kernel_stack_top;
        (old_rsp_ptr, new_rsp, next_as, next_kernel_stack)
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
    //
    // Also always reloads TSS.RSP0 to the incoming task's own private
    // entry stack, unconditionally, same reasoning as CR3 above: this
    // is what gives every ring-3-resident task its own trap buffer
    // instead of the old single shared one, closing the collision this
    // was fixed for. Must happen before context_switch, same as the AS
    // switch: once context_switch runs, this stack frame doesn't
    // resume again until this task is rescheduled.
    unsafe {
        next_as.switch();
        gdt::set_kernel_stack(next_kernel_stack);
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

    let (old_rsp_ptr, new_rsp, next_as, next_kernel_stack) = {
        let mut scheduler = SCHEDULER.lock();
        let old_index = scheduler.current;
        let next_index = next_runnable_index(&scheduler.tasks, old_index);
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
        let next_kernel_stack = scheduler.tasks[next_index].kernel_stack_top;
        (old_rsp_ptr, new_rsp, next_as, next_kernel_stack)
    };

    // See the matching comments in tick(): always reloads CR3 for now,
    // same-AS case not special-cased yet, and always reloads TSS.RSP0
    // to the incoming task's own private entry stack.
    unsafe {
        next_as.switch();
        gdt::set_kernel_stack(next_kernel_stack);
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

/// Returns whichever task index is current at the moment of the call.
/// Same save_flags_and_cli/restore_flags bracket as
/// `with_current_cspace`, for the identical reason: endpoint.rs's
/// `send`/`receive` call this from inside the vector-0x80 interrupt
/// gate (IF=0) as well as, in principle, any future ring-0 caller at
/// IF=1, so a blind cli/sti pair isn't safe here either.
pub fn current_index() -> usize {
    let flags = unsafe { save_flags_and_cli() };
    let result = {
        let scheduler = SCHEDULER.lock();
        scheduler.current
    };
    unsafe { restore_flags(flags) };
    result
}

/// Voluntarily blocks the calling task and switches to whatever
/// `next_runnable_index` finds next, if anything. Only caller today
/// is endpoint.rs's `send`/`receive`, both reached through the
/// vector-0x80 syscall interrupt gate (idt.rs), which clears IF for
/// its whole handler; `save_flags_and_cli`/`restore_flags` is required
/// here for the same reason `with_current_cspace`/`spawn_user` need
/// it (see their own doc comments), not a blind cli/sti pair.
///
/// If no other task is currently runnable (`next_runnable_index`
/// falls back to the caller's own index), this un-blocks the caller
/// immediately and returns without ever touching `context_switch`: a
/// task blocking with nothing else in the run queue to hand the CPU
/// to would otherwise "switch" to itself and never resume, since
/// `context_switch`'s own save/restore dance assumes it's always
/// switching away from the caller, not into a stack frame the caller
/// itself is still sitting on.
pub fn block_current_and_switch() {
    let flags = unsafe { save_flags_and_cli() };

    let (old_rsp_ptr, new_rsp, next_as, next_kernel_stack) = {
        let mut scheduler = SCHEDULER.lock();
        let old_index = scheduler.current;
        scheduler.tasks[old_index].blocked = true;
        let next_index = next_runnable_index(&scheduler.tasks, old_index);

        if next_index == old_index {
            // Nothing else runnable; can't actually block. Undo the
            // blocked flag immediately rather than leaving this task
            // marked blocked forever with nothing that would ever
            // call wake() on it.
            scheduler.tasks[old_index].blocked = false;
            unsafe { restore_flags(flags) };
            return;
        }

        scheduler.current = next_index;
        scheduler.ticks_left = TIMESLICE_TICKS;

        let old_rsp_ptr = &mut scheduler.tasks[old_index].saved_rsp as *mut u64;
        let new_rsp = scheduler.tasks[next_index].saved_rsp;
        let next_as = scheduler.tasks[next_index].address_space;
        let next_kernel_stack = scheduler.tasks[next_index].kernel_stack_top;
        (old_rsp_ptr, new_rsp, next_as, next_kernel_stack)
    };

    unsafe {
        next_as.switch();
        gdt::set_kernel_stack(next_kernel_stack);
        context_switch(old_rsp_ptr, new_rsp);
    }
    SWITCHES.fetch_add(1, Ordering::Relaxed);

    unsafe { restore_flags(flags) };
}

/// Clears `blocked` on the task at `task_index`, making it eligible
/// for `next_runnable_index` again. Doesn't force an immediate
/// switch: whichever task calls this (endpoint.rs's `send`/`receive`,
/// same interrupt-gate context as `block_current_and_switch`, see its
/// own doc comment for why `save_flags_and_cli`/`restore_flags` is
/// required here too) keeps running until its own next natural switch
/// point; the woken task resumes whenever an ordinary tick/yield_now
/// round-robin reaches its index.
pub fn wake(task_index: usize) {
    let flags = unsafe { save_flags_and_cli() };
    {
        let mut scheduler = SCHEDULER.lock();
        if let Some(task) = scheduler.tasks.get_mut(task_index) {
            task.blocked = false;
        }
    }
    unsafe { restore_flags(flags) };
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

/// Landing pad for a Configure-spawned task's very first run, same
/// reasoning as `task_trampoline` (reached via context_switch's `ret`
/// on first run, never through iretq or yield_now's own sti, so `sti`
/// has to come first here too) but dropping straight into ring 3
/// instead of calling a ring-0 fn. Replicates `enter_user_mode`'s
/// exact push order (SS, RSP, RFLAGS, CS, RIP, so RIP ends up topmost
/// per iretq's own pop order) by hand, since a naked fn can't call
/// into a normal `unsafe fn` mid-asm: `entry` (RIP) comes in via rbx
/// and `user_stack_top` (RSP) via rbp, exactly as `spawn_user`'s
/// fabricated frame laid them out; SS/CS/RFLAGS are the same fixed
/// ring-3 values `enter_user_mode` uses, embedded as naked_asm consts
/// since there's no register budget left to spare for them here.
#[unsafe(naked)]
extern "C" fn user_task_trampoline() -> ! {
    core::arch::naked_asm!(
        "sti",
        "push {user_ss}",
        "push rbp",
        "push {rflags}",
        "push {user_cs}",
        "push rbx",
        "iretq",
        user_ss = const gdt::USER_DATA_SELECTOR as u64,
        rflags = const 0x202u64,
        user_cs = const gdt::USER_CODE_SELECTOR as u64,
    )
}
