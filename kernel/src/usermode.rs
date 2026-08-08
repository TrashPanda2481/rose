// Ring 3 / user mode, v0.1.
//
// First code in the kernel that ever runs outside CPL0. Everything here
// is scaffolding for exactly one self-test: enter ring 3, prove two
// round-trip syscalls work (ring3 -> #GP-free int 0x80 -> ring0 handler
// -> back), then halt. No process model, no context saved to return to
// the interrupted kernel flow, no second user task. Those all need an
// AddressSpace/task abstraction that doesn't exist yet (see
// docs/cores/kernel/README.md, capability model, still spec only).
//
// enter_user_mode is one-way in this cut: iretq into ring 3 and there's
// nothing that ever iretqs back into whatever called it. on_syscall's
// second call halts the CPU directly instead of returning control to
// kernel_main. This is a deliberate, documented scope cut, not an
// oversight, see BUGS.md/README.md.

use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::gdt;
use crate::serial;
use crate::syscall::{
    SYS_CSPACE_COPY, SYS_CSPACE_MINT, SYS_CSPACE_MOVE, SYS_CSPACE_REVOKE, SYS_FRAME_MAP,
    SYS_THREAD_CONFIGURE, SYS_UNTYPED_RETYPE,
};

// Hand-written user-mode machine code, not compiled Rust: there's no
// user-mode runtime (no allocator, no panic handler, nothing) for a
// normal fn to run against yet, and a naked fn embedded in the kernel's
// own text section would still be mapped with kernel permissions, not
// USER. A dedicated asm blob with its own start/end labels is small
// enough to hand-place into a USER-mapped page via a plain memcpy,
// same linker-symbol convention paging.rs already uses for kernel
// section bounds.
// Extended past the original two legacy sentinel syscalls with a
// fixed sequence of six CSpace syscalls, all against slots the boot
// handoff in main.rs's usermode_selftest sets up ahead of time (a
// root cap over the kernel AddressSpace granted into this task's own
// CSpace slot 1). This has to live in the same program as the legacy
// two, not a separate self-test run after them: enter_user_mode is
// one-way, so nothing in kernel_main after usermode_selftest's call
// into it can ever run (see module docs). Hardcoded literal register
// values throughout, not symbolic constants: this is hand-written
// ring-3 asm with no linker visibility into kernel-side consts, same
// as the pre-existing 0xabcd1234/1 sentinels above it.
//
// Sequence and expected results, cross-checked against on_cspace_syscall's
// CSPACE_SYSCALL_STEPS table below; keep the two in sync if either changes:
//   1. COPY   slot1 -> slot2, rights=READ|WRITE|MAP (0xb)      -> 0 (ok)
//   2. MOVE   slot2 -> slot4                                   -> 0 (ok)
//   3. MINT   slot1 -> slot3, rights=READ (1), badge=0xc0ffee  -> 0 (ok)
//   4. COPY   slot1 -> slot3 (already occupied by the mint)    -> DestOccupied
//   5. REVOKE slot1 (cascades: clears slot3 and slot4)          -> 0 (ok)
//   6. COPY   slot1 -> slot5 (slot1 just revoked, now empty)    -> SourceEmpty
// Extended past the six CSpace steps with a fixed six-step Retype
// sequence, against slot6 (a root Untyped cap over a 4-frame pool,
// see main.rs's usermode_selftest). Cross-checked against
// on_untyped_syscall's UNTYPED_SYSCALL_STEPS table below:
//   7.  RETYPE slot6 -> slot7,  Frame        (1)                 -> ok
//   8.  RETYPE slot6 -> slot8,  CSpace       (9)                 -> ok
//   9.  RETYPE slot6 -> slot9,  AddressSpace (3)                 -> ok
// Extended past the AddressSpace retype with two Frame-Map steps,
// the actual fix for the Configure page-fault bug (see BUGS.md): the
// second program's code/stack were previously mapped only into
// user_as, a *different* AddressSpace object than the fresh one
// slot9 points to, so the task Configure spawned against slot9
// faulted on its very first instruction. Cross-checked against
// on_frame_map_syscall's MAP_SYSCALL_STEPS table below:
//   10. MAP    frame slot7  -> as slot9 @ 0xa00000 (STACK2_VADDR),
//       flags=WRITABLE|NO_EXECUTE (3) (reuses the Frame retyped in
//       step 7 above, which nothing had used until now; a fresh
//       zeroed frame is exactly correct for a stack)          -> ok
//   11. MAP    frame slot13 -> as slot9 @ 0x800000 (CODE2_VADDR),
//       flags=0 (R+X, no WRITABLE) (slot13: a pre-populated root
//       Frame cap over program2's actual instruction bytes, granted
//       by main.rs's usermode_selftest ahead of time, same
//       boot-handoff pattern as slot1/slot6; a freshly retyped
//       zeroed frame can't hold real code, and ring-3 has no way to
//       write frame contents via syscalls)                    -> ok
// Extended past the two Map steps with the remaining three Retype
// steps (unchanged target slots, just resequenced after Map):
//   12. RETYPE slot6 -> slot10, Thread       (4) (pool now empty) -> ok
//   13. RETYPE slot6 -> slot11, Frame        (1) (pool empty)     -> Exhausted
//   14. RETYPE slot6 -> slot12, PageTable    (2) (not retypeable) -> UnsupportedType
// Extended past the fourteen Retype/Map steps with one Configure
// step, cross-checked against on_configure_syscall's
// CONFIGURE_SYSCALL_STEPS table below:
//   15. CONFIGURE slot10 (Thread, from step 12) as=slot9 (AddressSpace,
//       from step 9), cspace=0 (fresh), entry=0x800000, stack_top=
//       0xa01000 (second program's code/stack pages, now actually
//       mapped into slot9 itself by steps 10/11 above, not just into
//       user_as)                                                -> ok
core::arch::global_asm!(
    ".section .rodata.user_program, \"a\"",
    ".global user_program_start",
    ".global user_program_end",
    "user_program_start:",
    "mov rax, 0xabcd1234",
    "int 0x80",
    "mov rax, 1",
    "int 0x80",
    // 1: COPY slot1 -> slot2, rights=READ|WRITE|MAP
    "mov rdi, 1",
    "mov rsi, 2",
    "mov rdx, 0xb",
    "mov rax, 0x1000",
    "int 0x80",
    // 2: MOVE slot2 -> slot4
    "mov rdi, 2",
    "mov rsi, 4",
    "mov rax, 0x1002",
    "int 0x80",
    // 3: MINT slot1 -> slot3, rights=READ, badge=0xc0ffee
    "mov rdi, 1",
    "mov rsi, 3",
    "mov rdx, 1",
    "mov r10, 0xc0ffee",
    "mov rax, 0x1001",
    "int 0x80",
    // 4: COPY slot1 -> slot3, expect DestOccupied (slot3 taken by mint)
    "mov rdi, 1",
    "mov rsi, 3",
    "mov rdx, 0xb",
    "mov rax, 0x1000",
    "int 0x80",
    // 5: REVOKE slot1 (cascades to slot3, slot4)
    "mov rdi, 1",
    "mov rax, 0x1003",
    "int 0x80",
    // 6: COPY slot1 -> slot5, expect SourceEmpty (slot1 just revoked)
    "mov rdi, 1",
    "mov rsi, 5",
    "mov rdx, 0xb",
    "mov rax, 0x1000",
    "int 0x80",
    // 7: RETYPE slot6 -> slot7, Frame (1), expect ok
    "mov rdi, 6",
    "mov rsi, 1",
    "mov rdx, 7",
    "mov rax, 0x1004",
    "int 0x80",
    // 8: RETYPE slot6 -> slot8, CSpace (9), expect ok
    "mov rdi, 6",
    "mov rsi, 9",
    "mov rdx, 8",
    "mov rax, 0x1004",
    "int 0x80",
    // 9: RETYPE slot6 -> slot9, AddressSpace (3), expect ok
    "mov rdi, 6",
    "mov rsi, 3",
    "mov rdx, 9",
    "mov rax, 0x1004",
    "int 0x80",
    // 10: MAP frame slot7 -> as slot9 @ 0xa00000 (STACK2_VADDR),
    // flags=3 (WRITABLE|NO_EXECUTE), expect ok. Reuses the Frame
    // retyped in step 7 above; a fresh zeroed frame is exactly
    // correct for a stack.
    "mov rdi, 7",
    "mov rsi, 9",
    "mov rdx, 0xa00000",
    "mov r10, 3",
    "mov rax, 0x1006",
    "int 0x80",
    // 11: MAP frame slot13 -> as slot9 @ 0x800000 (CODE2_VADDR),
    // flags=0 (R+X, no WRITABLE), expect ok. slot13 is a pre-
    // populated root Frame cap over program2's real instruction
    // bytes, granted ahead of time by main.rs's usermode_selftest.
    "mov rdi, 13",
    "mov rsi, 9",
    "mov rdx, 0x800000",
    "mov r10, 0",
    "mov rax, 0x1006",
    "int 0x80",
    // 12: RETYPE slot6 -> slot10, Thread (4), expect ok (pool of 4
    // frames now fully spent by steps 7 through 12)
    "mov rdi, 6",
    "mov rsi, 4",
    "mov rdx, 10",
    "mov rax, 0x1004",
    "int 0x80",
    // 13: RETYPE slot6 -> slot11, Frame (1), expect Exhausted (pool
    // already spent by steps 7 through 12)
    "mov rdi, 6",
    "mov rsi, 1",
    "mov rdx, 11",
    "mov rax, 0x1004",
    "int 0x80",
    // 14: RETYPE slot6 -> slot12, PageTable (2), expect UnsupportedType
    // (deliberately not retypeable, see untyped.rs module doc)
    "mov rdi, 6",
    "mov rsi, 2",
    "mov rdx, 12",
    "mov rax, 0x1004",
    "int 0x80",
    // 15: CONFIGURE slot10 (Thread) as=slot9 (AddressSpace), cspace=0
    // (fresh), entry=0x800000, stack_top=0xa01000 (second program's
    // code/stack, now actually mapped into slot9 itself by steps
    // 10/11 above, not just into user_as)
    "mov rdi, 10",
    "mov rsi, 9",
    "mov rdx, 0",
    "mov r10, 0x800000",
    "mov r8, 0xa01000",
    "mov rax, 0x1005",
    "int 0x80",
    "2:",
    "jmp 2b",
    "user_program_end:",
    ".previous",
);

// Second, much smaller ring-3 program: what step 13 above actually
// launches via Configure, into a brand-new task rather than via
// enter_user_mode. Only job is to prove it got there at all: one
// syscall trap, using the pre-existing legacy sentinel value (1)
// rather than inventing a new tracking table just for a liveness
// check, so on_syscall's own counter/log is the proof this ran.
core::arch::global_asm!(
    ".section .rodata.user_program2, \"a\"",
    ".global user_program2_start",
    ".global user_program2_end",
    "user_program2_start:",
    "mov rax, 1",
    "int 0x80",
    "2:",
    "jmp 2b",
    "user_program2_end:",
    ".previous",
);

unsafe extern "C" {
    static user_program2_start: u8;
    static user_program2_end: u8;
}

/// Byte length/pointer for the second embedded ring-3 program, same
/// convention as `program_bytes` above. Configure's own self-test step
/// (step 13 in the first program) is what launches this one; main.rs's
/// usermode_selftest copies it into a second code page in the same
/// way it already copies the first program into its own.
pub fn program2_bytes() -> &'static [u8] {
    unsafe {
        let start = core::ptr::addr_of!(user_program2_start) as *const u8;
        let end = core::ptr::addr_of!(user_program2_end) as *const u8;
        let len = end as usize - start as usize;
        core::slice::from_raw_parts(start, len)
    }
}

unsafe extern "C" {
    static user_program_start: u8;
    static user_program_end: u8;
}

/// Byte length of the embedded program, for the caller to copy exactly
/// that many bytes into the USER-mapped code page.
pub fn program_bytes() -> &'static [u8] {
    unsafe {
        let start = core::ptr::addr_of!(user_program_start) as *const u8;
        let end = core::ptr::addr_of!(user_program_end) as *const u8;
        let len = end as usize - start as usize;
        core::slice::from_raw_parts(start, len)
    }
}

static SYSCALL_COUNT: AtomicU64 = AtomicU64::new(0);

/// Called from idt.rs's rose_exception_handler on every `int 0x80`
/// carrying one of the two legacy sentinel values. Just logs; the
/// halt this used to do on the second call moved to
/// on_cspace_syscall's own final step, since the CSpace sequence now
/// runs after these two in the same ring-3 program and needs to be
/// the thing that actually stops the CPU for good. See module docs.
pub fn on_syscall(rax: u64) {
    let mut com1 = serial::Serial::init();
    let count = SYSCALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    let _ = writeln!(
        com1,
        "rose: usermode self-test: syscall #{} from ring 3, rax={:#x}",
        count, rax
    );
}

static CSPACE_SYSCALL_STEP: AtomicU64 = AtomicU64::new(0);
static CSPACE_SYSCALL_ALL_OK: AtomicBool = AtomicBool::new(true);

/// One row per syscall in the fixed six-step sequence appended to the
/// ring-3 program above; index must line up with call order exactly.
/// `expected_result` uses the same encoding syscall::encode produces:
/// 0 for success, `(-(code as i64)) as u64` for a CSpaceError.
const CSPACE_SYSCALL_STEPS: [(&str, u64, u64); 6] = [
    ("copy slot1->slot2 rights=rwm", SYS_CSPACE_COPY, 0),
    ("move slot2->slot4", SYS_CSPACE_MOVE, 0),
    ("mint slot1->slot3 rights=r badge=0xc0ffee", SYS_CSPACE_MINT, 0),
    (
        "copy slot1->slot3 (dest occupied by mint)",
        SYS_CSPACE_COPY,
        (-3i64) as u64,
    ),
    ("revoke slot1 (cascades slot3, slot4)", SYS_CSPACE_REVOKE, 0),
    (
        "copy slot1->slot5 (source just revoked)",
        SYS_CSPACE_COPY,
        (-2i64) as u64,
    ),
];

/// Called from idt.rs's rose_exception_handler for every `int 0x80`
/// carrying a CSpace syscall number. Verifies each of the six against
/// CSPACE_SYSCALL_STEPS in order and, once the last one lands, prints
/// the aggregated verdict and halts for good. This is now the true
/// final halt for the whole boot self-test chain: enter_user_mode is
/// one-way, so this ring-3 program's own forever-loop tail
/// (`2: jmp 2b`) never actually gets a chance to run once this halt
/// fires first, same as the legacy on_syscall halt used to be reached
/// before this feature.
pub fn on_cspace_syscall(num: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64, result: u64) {
    let mut com1 = serial::Serial::init();
    let step = CSPACE_SYSCALL_STEP.fetch_add(1, Ordering::Relaxed);

    let Some(&(label, expected_num, expected_result)) =
        CSPACE_SYSCALL_STEPS.get(step as usize)
    else {
        // More CSpace syscalls arrived than the fixed sequence
        // expects; the ring-3 program above is the only source of
        // these, so this would mean it and this table drifted apart.
        // Log and fail closed rather than indexing out of bounds.
        let _ = writeln!(
            com1,
            "rose: cspace syscall self-test: unexpected step {} (num={:#x}), FAILED",
            step + 1,
            num
        );
        CSPACE_SYSCALL_ALL_OK.store(false, Ordering::Relaxed);
        return;
    };

    let ok = num == expected_num && result == expected_result;
    if !ok {
        CSPACE_SYSCALL_ALL_OK.store(false, Ordering::Relaxed);
    }

    let _ = writeln!(
        com1,
        "rose: cspace syscall self-test: step {} {} (num={:#x} args={:#x},{:#x},{:#x},{:#x}) result={:#x} expected={:#x} {}",
        step + 1,
        label,
        num,
        arg1,
        arg2,
        arg3,
        arg4,
        result,
        expected_result,
        if ok { "ok" } else { "FAILED" }
    );

    if step + 1 >= CSPACE_SYSCALL_STEPS.len() as u64 {
        let all_ok = CSPACE_SYSCALL_ALL_OK.load(Ordering::Relaxed);
        let _ = writeln!(
            com1,
            "rose: cspace syscall self-test: sequence complete, overall {}",
            if all_ok { "confirmed" } else { "FAILED" }
        );
        // No halt here anymore: the ring-3 program's ordering table
        // above now continues straight into the four Retype steps
        // after this sixth CSpace one, so control must fall back out
        // to whatever called this (idt.rs's handler, then back to
        // ring 3 via iretq) instead of stopping the CPU. The true
        // final halt moved to on_untyped_syscall below.
    }
}

/// Outcome an untyped syscall self-test step expects, for the table
/// below. Unlike CSPACE_SYSCALL_STEPS's plain `u64`, a successful
/// Retype's return value (a physical address for Frame, a
/// CSPACE_OBJECTS registry index for CSpace, see untyped.rs's
/// `retype`) isn't known ahead of time the way a fixed CSpaceError
/// code is; `Success` just checks the result is non-negative
/// (something was actually minted) rather than pinning an exact id.
enum ExpectedOutcome {
    Success,
    Error(u64),
}

static UNTYPED_SYSCALL_STEP: AtomicU64 = AtomicU64::new(0);
static UNTYPED_SYSCALL_ALL_OK: AtomicBool = AtomicBool::new(true);

/// One row per syscall in the fixed six-step Retype sequence
/// appended to the ring-3 program above, run immediately after the
/// six CSpace steps; index must line up with call order exactly.
const UNTYPED_SYSCALL_STEPS: [(&str, ExpectedOutcome); 6] = [
    ("retype slot6->slot7 Frame", ExpectedOutcome::Success),
    ("retype slot6->slot8 CSpace", ExpectedOutcome::Success),
    ("retype slot6->slot9 AddressSpace", ExpectedOutcome::Success),
    (
        "retype slot6->slot10 Thread (pool now empty)",
        ExpectedOutcome::Success,
    ),
    (
        "retype slot6->slot11 Frame (pool exhausted)",
        ExpectedOutcome::Error((-12i64) as u64),
    ),
    (
        "retype slot6->slot12 PageTable (unsupported type, deliberate punt)",
        ExpectedOutcome::Error((-13i64) as u64),
    ),
];

/// Called from idt.rs's rose_exception_handler for every `int 0x80`
/// carrying the Retype syscall number. Verifies each of the six
/// against UNTYPED_SYSCALL_STEPS in order and, once the last one
/// lands, prints the aggregated verdict and halts for good. This is
/// now the true final halt for the whole boot self-test chain (see
/// on_cspace_syscall's own comment on why its old halt moved here):
/// the CSpace sequence falls straight through into this one, and this
/// one's halt is what actually stops the CPU.
pub fn on_untyped_syscall(arg1: u64, arg2: u64, arg3: u64, result: u64) {
    let mut com1 = serial::Serial::init();
    let step = UNTYPED_SYSCALL_STEP.fetch_add(1, Ordering::Relaxed);

    let Some(&(label, ref expected)) = UNTYPED_SYSCALL_STEPS.get(step as usize) else {
        // More Retype syscalls arrived than the fixed sequence
        // expects; same fail-closed reasoning as on_cspace_syscall's
        // matching branch above.
        let _ = writeln!(
            com1,
            "rose: untyped syscall self-test: unexpected step {} (num={:#x}), FAILED",
            step + 1,
            SYS_UNTYPED_RETYPE
        );
        UNTYPED_SYSCALL_ALL_OK.store(false, Ordering::Relaxed);
        return;
    };

    let ok = match *expected {
        ExpectedOutcome::Success => (result as i64) >= 0,
        ExpectedOutcome::Error(code) => result == code,
    };
    if !ok {
        UNTYPED_SYSCALL_ALL_OK.store(false, Ordering::Relaxed);
    }

    let _ = writeln!(
        com1,
        "rose: untyped syscall self-test: step {} {} (args={:#x},{:#x},{:#x}) result={:#x} {}",
        step + 1,
        label,
        arg1,
        arg2,
        arg3,
        result,
        if ok { "ok" } else { "FAILED" }
    );

    if step + 1 >= UNTYPED_SYSCALL_STEPS.len() as u64 {
        let untyped_ok = UNTYPED_SYSCALL_ALL_OK.load(Ordering::Relaxed);
        let _ = writeln!(
            com1,
            "rose: untyped syscall self-test: sequence complete, overall {}",
            if untyped_ok { "confirmed" } else { "FAILED" }
        );
        // No halt here anymore: the ring-3 program's ordering table
        // now continues straight into the Configure step (13) right
        // after this sixth Retype one, same reasoning as
        // on_cspace_syscall's own comment on why its halt moved here
        // in the first place. The true final aggregate report (and
        // the reason this still doesn't halt) is in
        // on_configure_syscall below: halting here or there would stop
        // the CPU before the timer ever gets a chance to preempt task
        // 0 into the brand-new task Configure creates, which is the
        // entire point of this feature.
    }
}

static MAP_SYSCALL_STEP: AtomicU64 = AtomicU64::new(0);
static MAP_SYSCALL_ALL_OK: AtomicBool = AtomicBool::new(true);

/// One row per syscall in the fixed two-step Frame-Map sequence
/// appended to the ring-3 program above, run immediately after the
/// AddressSpace retype (step 9) and before the remaining three
/// Retype steps; index must line up with call order exactly.
const MAP_SYSCALL_STEPS: [(&str, ExpectedOutcome); 2] = [
    (
        "map frame slot7 -> as slot9 @ 0xa00000 (STACK2_VADDR), flags=WRITABLE|NO_EXECUTE",
        ExpectedOutcome::Success,
    ),
    (
        "map frame slot13 -> as slot9 @ 0x800000 (CODE2_VADDR), flags=none (R+X)",
        ExpectedOutcome::Success,
    ),
];

/// Called from idt.rs's rose_exception_handler for every `int 0x80`
/// carrying the Frame-Map syscall number. Verifies each of the two
/// against MAP_SYSCALL_STEPS in order; same shape as
/// on_untyped_syscall just above. This is the actual fix for the
/// Configure page-fault bug (see BUGS.md): once both of these land,
/// slot9's AddressSpace has real mappings at CODE2_VADDR/STACK2_VADDR,
/// not just user_as.
pub fn on_frame_map_syscall(arg1: u64, arg2: u64, arg3: u64, arg4: u64, result: u64) {
    let mut com1 = serial::Serial::init();
    let step = MAP_SYSCALL_STEP.fetch_add(1, Ordering::Relaxed);

    let Some(&(label, ref expected)) = MAP_SYSCALL_STEPS.get(step as usize) else {
        let _ = writeln!(
            com1,
            "rose: frame map syscall self-test: unexpected step {} (num={:#x}), FAILED",
            step + 1,
            SYS_FRAME_MAP
        );
        MAP_SYSCALL_ALL_OK.store(false, Ordering::Relaxed);
        return;
    };

    let ok = match *expected {
        ExpectedOutcome::Success => (result as i64) >= 0,
        ExpectedOutcome::Error(code) => result == code,
    };
    if !ok {
        MAP_SYSCALL_ALL_OK.store(false, Ordering::Relaxed);
    }

    let _ = writeln!(
        com1,
        "rose: frame map syscall self-test: step {} {} (args={:#x},{:#x},{:#x},{:#x}) result={:#x} {}",
        step + 1,
        label,
        arg1,
        arg2,
        arg3,
        arg4,
        result,
        if ok { "ok" } else { "FAILED" }
    );

    if step + 1 >= MAP_SYSCALL_STEPS.len() as u64 {
        let map_ok = MAP_SYSCALL_ALL_OK.load(Ordering::Relaxed);
        let _ = writeln!(
            com1,
            "rose: frame map syscall self-test: sequence complete, overall {}",
            if map_ok { "confirmed" } else { "FAILED" }
        );
        // No halt here, same reasoning as on_untyped_syscall's own
        // comment: the ring-3 program's ordering table continues
        // straight into the remaining Retype steps and then Configure,
        // and the true final aggregate report is in
        // on_configure_syscall below.
    }
}

static CONFIGURE_SYSCALL_STEP: AtomicU64 = AtomicU64::new(0);
static CONFIGURE_SYSCALL_ALL_OK: AtomicBool = AtomicBool::new(true);

/// One row per syscall in the fixed one-step Configure sequence
/// appended to the ring-3 program above, run immediately after the
/// twelve Retype steps; index must line up with call order exactly.
const CONFIGURE_SYSCALL_STEPS: [(&str, ExpectedOutcome); 1] = [(
    "configure slot10(thread) as=slot9 cspace=fresh entry=0x800000 stack_top=0xa01000",
    ExpectedOutcome::Success,
)];

/// Called from idt.rs's rose_exception_handler for every `int 0x80`
/// carrying the Configure syscall number. Verifies the single step
/// against CONFIGURE_SYSCALL_STEPS and, once it lands, prints the
/// full aggregated verdict across all three syscall families
/// (CSpace, Retype, Configure).
///
/// Deliberately still no halt here, unlike on_cspace_syscall's and
/// on_untyped_syscall's now-retired ones: Configure just made a
/// brand-new task schedulable (scheduler::spawn_user), and that task
/// only ever gets to run at all via a future timer preemption away
/// from this ring-3 program's own `2: jmp 2b` tail. Halting the CPU
/// here, even after logging a correct verdict, would make that
/// preemption impossible and the second program's own liveness
/// syscall (see program2_bytes, proven via on_syscall's existing
/// counter/log) would never actually fire. Letting both tasks spin
/// forever and relying on the test harness's own timeout (see
/// tools/smoke-test.sh) is the honest tradeoff for this cut; a real
/// exit/shutdown path is a future feature's job, not this one's.
pub fn on_configure_syscall(arg1: u64, arg2: u64, arg3: u64, arg4: u64, arg5: u64, result: u64) {
    let mut com1 = serial::Serial::init();
    let step = CONFIGURE_SYSCALL_STEP.fetch_add(1, Ordering::Relaxed);

    let Some(&(label, ref expected)) = CONFIGURE_SYSCALL_STEPS.get(step as usize) else {
        let _ = writeln!(
            com1,
            "rose: configure syscall self-test: unexpected step {} (num={:#x}), FAILED",
            step + 1,
            SYS_THREAD_CONFIGURE
        );
        CONFIGURE_SYSCALL_ALL_OK.store(false, Ordering::Relaxed);
        return;
    };

    let ok = match *expected {
        ExpectedOutcome::Success => (result as i64) >= 0,
        ExpectedOutcome::Error(code) => result == code,
    };
    if !ok {
        CONFIGURE_SYSCALL_ALL_OK.store(false, Ordering::Relaxed);
    }

    let _ = writeln!(
        com1,
        "rose: configure syscall self-test: step {} {} (args={:#x},{:#x},{:#x},{:#x},{:#x}) result={:#x} {}",
        step + 1,
        label,
        arg1,
        arg2,
        arg3,
        arg4,
        arg5,
        result,
        if ok { "ok" } else { "FAILED" }
    );

    if step + 1 >= CONFIGURE_SYSCALL_STEPS.len() as u64 {
        let cspace_ok = CSPACE_SYSCALL_ALL_OK.load(Ordering::Relaxed);
        let untyped_ok = UNTYPED_SYSCALL_ALL_OK.load(Ordering::Relaxed);
        let map_ok = MAP_SYSCALL_ALL_OK.load(Ordering::Relaxed);
        let configure_ok = CONFIGURE_SYSCALL_ALL_OK.load(Ordering::Relaxed);
        let _ = writeln!(
            com1,
            "rose: configure syscall self-test: sequence complete, overall {}",
            if configure_ok { "confirmed" } else { "FAILED" }
        );
        let _ = writeln!(
            com1,
            "rose: usermode self-test: full boot sequence {}",
            if cspace_ok && untyped_ok && map_ok && configure_ok {
                "confirmed"
            } else {
                "FAILED"
            }
        );
    }
}

/// Drops from ring 0 to ring 3 via iretq and never returns: there is no
/// caller to return to in this v0.1 cut (see module docs), and even if
/// there were, nothing currently arranges a way back short of another
/// interrupt. `entry`/`user_stack_top` must already be mapped USER
/// (code: R+X+U; stack: R+W+U) in the currently active address space,
/// and `gdt::set_kernel_stack` must already have been called so the
/// first ring3->ring0 trap (the first `int 0x80`) has a valid rsp0 to
/// land on.
///
/// Safety: caller must guarantee both of the above; getting either
/// wrong turns the very first instruction fetch or the very first trap
/// into a fault with no working stack to report it from.
pub unsafe fn enter_user_mode(entry: u64, user_stack_top: u64) -> ! {
    // Push order is SS, RSP, RFLAGS, CS, RIP so RIP ends up topmost
    // (last pushed = lowest address = first popped), matching iretq's
    // pop order and idt.rs's InterruptFrame field order (rip, cs,
    // rflags, rsp, ss from low to high address).
    core::arch::asm!(
        "push {ss}",
        "push {rsp}",
        "push {rflags}",
        "push {cs}",
        "push {rip}",
        "iretq",
        ss = in(reg) gdt::USER_DATA_SELECTOR as u64,
        rsp = in(reg) user_stack_top,
        // IF=1 (bit 1) plus the always-set reserved bit 1: ring 3 code
        // still needs interrupts enabled so a timer tick or the
        // syscall trap itself can be serviced normally.
        rflags = in(reg) 0x202u64,
        cs = in(reg) gdt::USER_CODE_SELECTOR as u64,
        rip = in(reg) entry,
        options(noreturn),
    );
}
