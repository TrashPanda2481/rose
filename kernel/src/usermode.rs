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

use crate::console;
use crate::gdt;
use crate::syscall::{
    SYS_CSPACE_COPY, SYS_CSPACE_MINT, SYS_CSPACE_MOVE, SYS_CSPACE_REVOKE, SYS_ENDPOINT_CALL,
    SYS_ENDPOINT_RECEIVE, SYS_ENDPOINT_REPLY, SYS_ENDPOINT_SEND, SYS_FRAME_MAP,
    SYS_THREAD_CONFIGURE, SYS_UNTYPED_RETYPE,
};
use crate::untyped;
use abi::{Capability, KernelObjectId, ObjectType, Rights};

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
// Extended past the AddressSpace retype with an Endpoint retype,
// Endpoint IPC increment 1's own self-test step, cross-checked
// against on_untyped_syscall's UNTYPED_SYSCALL_STEPS table below
// (now seven rows, this one inserted at index 3):
//   10. RETYPE slot15 -> slot16, Endpoint (5), expect ok. Uses a
//       dedicated 1-frame Untyped pool granted at slot15 by main.rs's
//       usermode_selftest, separate from slot6's own 4-frame pool
//       (which steps 13-14 below deliberately exhaust; sharing a pool
//       would disturb that arithmetic). on_untyped_syscall's own
//       side-effect hook grants a RECEIVE-only copy of the resulting
//       Endpoint cap into slot8's CSpace object (the CSpace retyped
//       in step 8, still unclaimed at this point; see
//       untyped::grant_into_cspace) at that CSpace's own slot 1, so
//       program2's task inherits it once Configure (step 16 below)
//       claims that CSpace.
// Extended past the Endpoint retype with two Frame-Map steps, the
// actual fix for the Configure page-fault bug (see BUGS.md): the
// second program's code/stack were previously mapped only into
// user_as, a *different* AddressSpace object than the fresh one
// slot9 points to, so the task Configure spawned against slot9
// faulted on its very first instruction. Cross-checked against
// on_frame_map_syscall's MAP_SYSCALL_STEPS table below:
//   11. MAP    frame slot7  -> as slot9 @ 0xa00000 (STACK2_VADDR),
//       flags=WRITABLE|NO_EXECUTE (3) (reuses the Frame retyped in
//       step 7 above, which nothing had used until now; a fresh
//       zeroed frame is exactly correct for a stack)          -> ok
//   12. MAP    frame slot13 -> as slot9 @ 0x800000 (CODE2_VADDR),
//       flags=0 (R+X, no WRITABLE) (slot13: a pre-populated root
//       Frame cap over program2's actual instruction bytes, granted
//       by main.rs's usermode_selftest ahead of time, same
//       boot-handoff pattern as slot1/slot6; a freshly retyped
//       zeroed frame can't hold real code, and ring-3 has no way to
//       write frame contents via syscalls)                    -> ok
// Extended past the two Map steps with the remaining three Retype
// steps (unchanged target slots, just resequenced after Map):
//   13. RETYPE slot6 -> slot10, Thread       (4) (pool now empty) -> ok
//   14. RETYPE slot6 -> slot11, Frame        (1) (pool empty)     -> Exhausted
//   15. RETYPE slot6 -> slot12, PageTable    (2) (not retypeable) -> UnsupportedType
// Extended past the fifteen Retype/Map steps with one Configure
// step, cross-checked against on_configure_syscall's
// CONFIGURE_SYSCALL_STEPS table below:
//   16. CONFIGURE slot10 (Thread, from step 13) as=slot9 (AddressSpace,
//       from step 9), cspace=8 (the CSpace retyped in step 8, now
//       carrying the RECEIVE-only Endpoint cap on_untyped_syscall
//       pre-populated at its own slot 1; previously 0/fresh, before
//       this feature), entry=0x800000, stack_top=0xa01000 (second
//       program's code/stack pages, now actually mapped into slot9
//       itself by steps 11/12 above, not just into user_as)     -> ok
// Extended past the Configure step with one Send step, Endpoint IPC
// increment 1's own final step, cross-checked against
// on_endpoint_send_syscall's own single-row table below:
//   17. SEND on slot16 (the Endpoint retyped in step 10), label=
//       0x1234, data0=0xdead, data1=0xbeef. Runs immediately after
//       Configure, before program2's task (spawned by that same
//       Configure call) has executed at all, so no receiver is
//       parked yet: this always takes Send's blocking path (see
//       endpoint.rs's own doc comment), parking task0 here until
//       program2's own Receive step (below) finds it and wakes it
//       back up.
// Extended past the Send step with one Call step, Endpoint IPC
// increment 2's own final step, cross-checked against
// on_endpoint_call_syscall's own single-row table below:
//   18. CALL on slot16 (same Endpoint as step 17), label=0x5678,
//       data0=0xf00d, data1=0xba5e. Whether this takes call()'s own
//       fast or slow path for its send phase depends on real timer
//       preemption against program2's own second Receive step (see
//       program2's own doc comment below) -- either way, this then
//       always blocks a second time waiting for program2's matching
//       Reply. Because of that second, unconditional block,
//       on_endpoint_call_syscall's own hook, not
//       on_endpoint_send_syscall's, is now provably the last
//       self-test hook to actually fire; see its own doc comment for
//       why the aggregate report moved there.
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
    // 10: RETYPE slot15 -> slot16, Endpoint (5), expect ok. Uses the
    // dedicated 1-frame Untyped pool at slot15 (main.rs's
    // usermode_selftest), separate from slot6's own 4-frame pool.
    "mov rdi, 15",
    "mov rsi, 5",
    "mov rdx, 16",
    "mov rax, 0x1004",
    "int 0x80",
    // 11: MAP frame slot7 -> as slot9 @ 0xa00000 (STACK2_VADDR),
    // flags=3 (WRITABLE|NO_EXECUTE), expect ok. Reuses the Frame
    // retyped in step 7 above; a fresh zeroed frame is exactly
    // correct for a stack.
    "mov rdi, 7",
    "mov rsi, 9",
    "mov rdx, 0xa00000",
    "mov r10, 3",
    "mov rax, 0x1006",
    "int 0x80",
    // 12: MAP frame slot13 -> as slot9 @ 0x800000 (CODE2_VADDR),
    // flags=0 (R+X, no WRITABLE), expect ok. slot13 is a pre-
    // populated root Frame cap over program2's real instruction
    // bytes, granted ahead of time by main.rs's usermode_selftest.
    "mov rdi, 13",
    "mov rsi, 9",
    "mov rdx, 0x800000",
    "mov r10, 0",
    "mov rax, 0x1006",
    "int 0x80",
    // 13: RETYPE slot6 -> slot10, Thread (4), expect ok (pool of 4
    // frames now fully spent by steps 7 through 13)
    "mov rdi, 6",
    "mov rsi, 4",
    "mov rdx, 10",
    "mov rax, 0x1004",
    "int 0x80",
    // 14: RETYPE slot6 -> slot11, Frame (1), expect Exhausted (pool
    // already spent by steps 7 through 13)
    "mov rdi, 6",
    "mov rsi, 1",
    "mov rdx, 11",
    "mov rax, 0x1004",
    "int 0x80",
    // 15: RETYPE slot6 -> slot12, PageTable (2), expect UnsupportedType
    // (deliberately not retypeable, see untyped.rs module doc)
    "mov rdi, 6",
    "mov rsi, 2",
    "mov rdx, 12",
    "mov rax, 0x1004",
    "int 0x80",
    // 16: CONFIGURE slot10 (Thread) as=slot9 (AddressSpace), cspace=8
    // (the CSpace retyped in step 8, now carrying a RECEIVE-only
    // Endpoint cap on_untyped_syscall pre-populated at its own slot 1),
    // entry=0x800000, stack_top=0xa01000 (second program's code/stack,
    // now actually mapped into slot9 itself by steps 11/12 above, not
    // just into user_as)
    "mov rdi, 10",
    "mov rsi, 9",
    "mov rdx, 8",
    "mov r10, 0x800000",
    "mov r8, 0xa01000",
    "mov rax, 0x1005",
    "int 0x80",
    // 17: SEND on slot16 (the Endpoint retyped in step 10), label=
    // 0x1234, data0=0xdead, data1=0xbeef. Blocks: program2's own
    // Receive step (see user_program2 below) hasn't run yet.
    "mov rdi, 16",
    "mov rsi, 0x1234",
    "mov rdx, 0xdead",
    "mov r10, 0xbeef",
    "mov rax, 0x1007",
    "int 0x80",
    // 18: CALL on slot16 (same Endpoint), label=0x5678, data0=0xf00d,
    // data1=0xba5e. Blocks twice: once for the send phase (fast path
    // if program2's own second Receive is already parked by the time
    // this runs, slow path otherwise), then always a second time
    // waiting specifically for program2's own Reply.
    "mov rdi, 16",
    "mov rsi, 0x5678",
    "mov rdx, 0xf00d",
    "mov r10, 0xba5e",
    "mov rax, 0x1009",
    "int 0x80",
    "2:",
    "jmp 2b",
    "user_program_end:",
    ".previous",
);

// Second, much smaller ring-3 program: what step 16 above actually
// launches via Configure, into a brand-new task rather than via
// enter_user_mode. First job is to prove it got there at all: one
// syscall trap, using the pre-existing legacy sentinel value (1)
// rather than inventing a new tracking table just for a liveness
// check, so on_syscall's own counter/log is the proof this ran.
// Second job, added for Endpoint IPC increment 1: RECEIVE on slot1,
// the RECEIVE-only Endpoint cap on_untyped_syscall's own hook
// pre-populated into this task's CSpace (retyped at slot8 in step 8
// above, claimed by this task via Configure's own cspace=8 arg).
// This always finds task0 already parked on its own Send (step 17
// above ran first, before this task was even schedulable), so it
// takes the immediate delivery-in-place path (see endpoint.rs) and
// wakes task0 back up rather than blocking itself. rsi is set to 0
// explicitly ahead of this call (added for increment 2, see below):
// dispatch_endpoint_receive now always reads arg2 (reply_dst_cptr)
// from rsi, and 0 is the correct "don't care" sentinel for a
// Send-originated Receive like this one, which carries no reply_id
// to install a cap for either way.
//
// Third job, added for Endpoint IPC increment 2: a second RECEIVE on
// slot1, this time with reply_dst_cptr=2 (slot2, confirmed free --
// nothing else in this program uses it) in rsi, followed by a REPLY
// on slot2. Whichever order this second Receive and task0's own Call
// (user_program's own step 18, above) actually land in depends on
// real timer preemption; either order is handled correctly by
// endpoint::receive's/call's own fast-path/slow-path split (see
// endpoint.rs). dispatch_endpoint_receive's own side effect installs
// a Reply cap into this task's own CSpace at slot2 once this second
// Receive returns successfully, which the following Reply syscall
// then consumes.
core::arch::global_asm!(
    ".section .rodata.user_program2, \"a\"",
    ".global user_program2_start",
    ".global user_program2_end",
    "user_program2_start:",
    "mov rax, 1",
    "int 0x80",
    "mov rdi, 1",
    "mov rsi, 0",
    "mov rax, 0x1008",
    "int 0x80",
    // Second RECEIVE on slot1, reply_dst_cptr=2 (slot2). Matches
    // task0's own Call (user_program's step 18).
    "mov rdi, 1",
    "mov rsi, 2",
    "mov rax, 0x1008",
    "int 0x80",
    // REPLY on slot2 (the Reply cap the Receive just above installed
    // into this task's own CSpace), label=0x4321, data0=0xcafe,
    // data1=0xbabe -- a distinct payload from every other step's own,
    // so the self-test log can tell this response apart from the
    // original Call's request.
    "mov rdi, 2",
    "mov rsi, 0x4321",
    "mov rdx, 0xcafe",
    "mov r10, 0xbabe",
    "mov rax, 0x100a",
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
    let mut com1 = console::Dual::init();
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
    let mut com1 = console::Dual::init();
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

/// CSPACE_OBJECTS registry index of the CSpace retyped at slot8 in
/// step 8 of the self-test sequence, captured from that step's own
/// successful result (the registry index IS the retype's return
/// value, see untyped.rs's `retype`). Needed later, at step 3 (index)
/// below, to grant the freshly retyped Endpoint's RECEIVE-only copy
/// into that CSpace rather than into task0's own; u64::MAX is not a
/// valid registry index and marks "not yet captured".
static SLOT8_CSPACE_REGISTRY_ID: AtomicU64 = AtomicU64::new(u64::MAX);

/// One row per syscall in the fixed seven-step Retype sequence
/// appended to the ring-3 program above, run immediately after the
/// six CSpace steps; index must line up with call order exactly.
/// Extended past the original six rows with an Endpoint retype at
/// index 3 (between AddressSpace and Thread) for Endpoint IPC
/// increment 1; the three rows after it are otherwise unchanged.
const UNTYPED_SYSCALL_STEPS: [(&str, ExpectedOutcome); 7] = [
    ("retype slot6->slot7 Frame", ExpectedOutcome::Success),
    ("retype slot6->slot8 CSpace", ExpectedOutcome::Success),
    ("retype slot6->slot9 AddressSpace", ExpectedOutcome::Success),
    ("retype slot15->slot16 Endpoint", ExpectedOutcome::Success),
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
/// carrying the Retype syscall number. Verifies each of the seven
/// against UNTYPED_SYSCALL_STEPS in order and, once the last one
/// lands, prints the aggregated verdict and halts for good. This is
/// now the true final halt for the whole boot self-test chain (see
/// on_cspace_syscall's own comment on why its old halt moved here):
/// the CSpace sequence falls straight through into this one, and this
/// one's halt is what actually stops the CPU.
///
/// Two of the seven steps have side effects beyond verification, for
/// Endpoint IPC increment 1: step 1 (the CSpace retype) stashes its
/// own registry-index result into SLOT8_CSPACE_REGISTRY_ID, and step
/// 3 (the new Endpoint retype) uses that stashed id to grant a
/// RECEIVE-only copy of the freshly minted Endpoint into that
/// CSpace's own slot 1, via untyped::grant_into_cspace. Both run
/// after the existing ok/FAILED check below so a failed retype never
/// feeds a bogus id or object into the grant.
pub fn on_untyped_syscall(arg1: u64, arg2: u64, arg3: u64, result: u64) {
    let mut com1 = console::Dual::init();
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

    if ok && step == 1 {
        // Step 1 (index): retype slot6->slot8 CSpace. Stash the
        // registry index for step 3's own grant_into_cspace call.
        SLOT8_CSPACE_REGISTRY_ID.store(result, Ordering::Relaxed);
    }
    if ok && step == 3 {
        // Step 3 (index): retype slot15->slot16 Endpoint. Grant a
        // RECEIVE-only copy into slot8's CSpace (captured just above,
        // two steps ago) at that CSpace's own slot 1, so program2's
        // task inherits it once Configure claims that CSpace.
        let cspace_id = SLOT8_CSPACE_REGISTRY_ID.load(Ordering::Relaxed);
        let receive_cap = Capability {
            object_ref: KernelObjectId(result),
            object_type: ObjectType::Endpoint,
            rights: Rights::RECEIVE,
            badge: 0,
        };
        match untyped::grant_into_cspace(cspace_id, 1, receive_cap) {
            Ok(()) => {
                let _ = writeln!(
                    com1,
                    "rose: untyped syscall self-test: granted RECEIVE-only endpoint cap into cspace {} slot1, ok",
                    cspace_id
                );
            }
            Err(e) => {
                let _ = writeln!(
                    com1,
                    "rose: untyped syscall self-test: grant_into_cspace(cspace={}, slot=1) FAILED, code={}",
                    cspace_id,
                    e.code()
                );
                UNTYPED_SYSCALL_ALL_OK.store(false, Ordering::Relaxed);
            }
        }
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
    let mut com1 = console::Dual::init();
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
/// against CONFIGURE_SYSCALL_STEPS. The full cross-family aggregated
/// verdict used to be printed here once this step landed; it moved to
/// on_endpoint_send_syscall below for Endpoint IPC increment 1 (see
/// that function's own doc comment for why it, not this one, is now
/// provably the last hook to fire).
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
    let mut com1 = console::Dual::init();
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
        let configure_ok = CONFIGURE_SYSCALL_ALL_OK.load(Ordering::Relaxed);
        let _ = writeln!(
            com1,
            "rose: configure syscall self-test: sequence complete, overall {}",
            if configure_ok { "confirmed" } else { "FAILED" }
        );
        // No cross-family aggregate report here anymore: Configure's
        // own program-order position (step 16) is no longer the last
        // self-test hook to actually fire, now that Send (step 17)
        // runs after it. See on_endpoint_send_syscall below.
    }
}

static ENDPOINT_SEND_STEP: AtomicU64 = AtomicU64::new(0);
static ENDPOINT_SEND_ALL_OK: AtomicBool = AtomicBool::new(true);

/// One row per syscall in the fixed one-step Send sequence appended
/// to the ring-3 program above, run immediately after Configure.
const ENDPOINT_SEND_SYSCALL_STEPS: [(&str, ExpectedOutcome); 1] = [(
    "send slot16 label=0x1234 data0=0xdead data1=0xbeef",
    ExpectedOutcome::Success,
)];

/// Called from idt.rs's rose_exception_handler for every `int 0x80`
/// carrying the Endpoint Send syscall number. Verifies the single
/// step against ENDPOINT_SEND_SYSCALL_STEPS.
///
/// No cross-family aggregate report or halt here anymore: Send (step
/// 17) is no longer the last self-test hook to actually fire, now
/// that Call (step 18, appended for Endpoint IPC increment 2) runs
/// after it. See on_endpoint_call_syscall below.
pub fn on_endpoint_send_syscall(arg1: u64, arg2: u64, arg3: u64, arg4: u64, result: u64) {
    let mut com1 = console::Dual::init();
    let step = ENDPOINT_SEND_STEP.fetch_add(1, Ordering::Relaxed);

    let Some(&(label, ref expected)) = ENDPOINT_SEND_SYSCALL_STEPS.get(step as usize) else {
        let _ = writeln!(
            com1,
            "rose: endpoint send syscall self-test: unexpected step {} (num={:#x}), FAILED",
            step + 1,
            SYS_ENDPOINT_SEND
        );
        ENDPOINT_SEND_ALL_OK.store(false, Ordering::Relaxed);
        return;
    };

    let ok = match *expected {
        ExpectedOutcome::Success => (result as i64) >= 0,
        ExpectedOutcome::Error(code) => result == code,
    };
    if !ok {
        ENDPOINT_SEND_ALL_OK.store(false, Ordering::Relaxed);
    }

    let _ = writeln!(
        com1,
        "rose: endpoint send syscall self-test: step {} {} (args={:#x},{:#x},{:#x},{:#x}) result={:#x} {}",
        step + 1,
        label,
        arg1,
        arg2,
        arg3,
        arg4,
        result,
        if ok { "ok" } else { "FAILED" }
    );

    if step + 1 >= ENDPOINT_SEND_SYSCALL_STEPS.len() as u64 {
        let endpoint_send_ok = ENDPOINT_SEND_ALL_OK.load(Ordering::Relaxed);
        let _ = writeln!(
            com1,
            "rose: endpoint send syscall self-test: sequence complete, overall {}",
            if endpoint_send_ok { "confirmed" } else { "FAILED" }
        );
        // No cross-family aggregate report here anymore: Send's own
        // program-order position (step 17) is no longer the last
        // self-test hook to actually fire, now that Call (step 18)
        // runs after it. See on_endpoint_call_syscall below.
    }
}

static ENDPOINT_RECEIVE_STEP: AtomicU64 = AtomicU64::new(0);
static ENDPOINT_RECEIVE_ALL_OK: AtomicBool = AtomicBool::new(true);

/// One row per syscall in the now two-step Receive sequence appended
/// to program2's ring-3 asm above. Row 0 is the original
/// increment-1 Receive (immediately after program2's own liveness
/// syscall, matching task0's Send); row 1 is increment 2's own
/// second Receive (reply_dst_cptr=2), matching task0's own Call.
const ENDPOINT_RECEIVE_SYSCALL_STEPS: [(&str, ExpectedOutcome); 2] = [
    ("receive slot1", ExpectedOutcome::Success),
    ("receive slot1 reply_dst=2", ExpectedOutcome::Success),
];

/// Called from idt.rs's rose_exception_handler for every `int 0x80`
/// carrying the Endpoint Receive syscall number. Verifies the
/// current step against ENDPOINT_RECEIVE_SYSCALL_STEPS. No aggregate
/// report or halt here: this always fires chronologically before
/// on_endpoint_call_syscall's own log line below, both for row 0
/// (task0's Send call only actually returns, and only logs, after
/// this Receive call has found it parked and woken it back up) and
/// for row 1 (task0's Call only actually returns, and only logs,
/// after the matching Reply -- itself only reachable once this
/// second Receive has returned and handed program2 the Reply cap at
/// slot2), so the true final report belongs there, not here. See
/// that function's own doc comment.
pub fn on_endpoint_receive_syscall(
    arg1: u64,
    arg2: u64,
    result: u64,
    label_out: u64,
    data0: u64,
    data1: u64,
) {
    let mut com1 = console::Dual::init();
    let step = ENDPOINT_RECEIVE_STEP.fetch_add(1, Ordering::Relaxed);

    let Some(&(label, ref expected)) = ENDPOINT_RECEIVE_SYSCALL_STEPS.get(step as usize) else {
        let _ = writeln!(
            com1,
            "rose: endpoint receive syscall self-test: unexpected step {} (num={:#x}), FAILED",
            step + 1,
            SYS_ENDPOINT_RECEIVE
        );
        ENDPOINT_RECEIVE_ALL_OK.store(false, Ordering::Relaxed);
        return;
    };

    let ok = match *expected {
        ExpectedOutcome::Success => (result as i64) >= 0,
        ExpectedOutcome::Error(code) => result == code,
    };
    if !ok {
        ENDPOINT_RECEIVE_ALL_OK.store(false, Ordering::Relaxed);
    }

    let _ = writeln!(
        com1,
        "rose: endpoint receive syscall self-test: step {} {} (arg1={:#x},arg2={:#x}) result={:#x} label={:#x} data0={:#x} data1={:#x} {}",
        step + 1,
        label,
        arg1,
        arg2,
        result,
        label_out,
        data0,
        data1,
        if ok { "ok" } else { "FAILED" }
    );
}

static ENDPOINT_CALL_STEP: AtomicU64 = AtomicU64::new(0);
static ENDPOINT_CALL_ALL_OK: AtomicBool = AtomicBool::new(true);

/// One row per syscall in the fixed one-step Call sequence appended
/// to the ring-3 program above, run immediately after Send (step
/// 18, Endpoint IPC increment 2).
const ENDPOINT_CALL_SYSCALL_STEPS: [(&str, ExpectedOutcome); 1] = [(
    "call slot16 label=0x5678 data0=0xf00d data1=0xba5e",
    ExpectedOutcome::Success,
)];

static ENDPOINT_REPLY_STEP: AtomicU64 = AtomicU64::new(0);
static ENDPOINT_REPLY_ALL_OK: AtomicBool = AtomicBool::new(true);

/// One row per syscall in the fixed one-step Reply sequence appended
/// to program2's ring-3 asm above, run immediately after its second
/// Receive (Endpoint IPC increment 2).
const ENDPOINT_REPLY_SYSCALL_STEPS: [(&str, ExpectedOutcome); 1] = [(
    "reply slot2 label=0x4321 data0=0xcafe data1=0xbabe",
    ExpectedOutcome::Success,
)];

/// Called from idt.rs's rose_exception_handler for every `int 0x80`
/// carrying the Endpoint Reply syscall number. Verifies the single
/// step against ENDPOINT_REPLY_SYSCALL_STEPS. No aggregate report or
/// halt here: this always fires chronologically before
/// on_endpoint_call_syscall's own log line below -- `reply::reply`
/// (see reply.rs) wakes task0's own blocked Call *after* this
/// function's own hook has already returned control to program2,
/// but task0 cannot actually resume and log its own Call result
/// until the scheduler switches back to it, which cannot happen
/// before this Reply syscall itself has returned -- so the true
/// final report belongs there, not here. See that function's own
/// doc comment.
pub fn on_endpoint_reply_syscall(arg1: u64, arg2: u64, arg3: u64, arg4: u64, result: u64) {
    let mut com1 = console::Dual::init();
    let step = ENDPOINT_REPLY_STEP.fetch_add(1, Ordering::Relaxed);

    let Some(&(label, ref expected)) = ENDPOINT_REPLY_SYSCALL_STEPS.get(step as usize) else {
        let _ = writeln!(
            com1,
            "rose: endpoint reply syscall self-test: unexpected step {} (num={:#x}), FAILED",
            step + 1,
            SYS_ENDPOINT_REPLY
        );
        ENDPOINT_REPLY_ALL_OK.store(false, Ordering::Relaxed);
        return;
    };

    let ok = match *expected {
        ExpectedOutcome::Success => (result as i64) >= 0,
        ExpectedOutcome::Error(code) => result == code,
    };
    if !ok {
        ENDPOINT_REPLY_ALL_OK.store(false, Ordering::Relaxed);
    }

    let _ = writeln!(
        com1,
        "rose: endpoint reply syscall self-test: step {} {} (args={:#x},{:#x},{:#x},{:#x}) result={:#x} {}",
        step + 1,
        label,
        arg1,
        arg2,
        arg3,
        arg4,
        result,
        if ok { "ok" } else { "FAILED" }
    );
}

/// Called from idt.rs's rose_exception_handler for every `int 0x80`
/// carrying the Endpoint Call syscall number. Verifies the single
/// step against ENDPOINT_CALL_SYSCALL_STEPS and, once it lands,
/// prints the full aggregated verdict across all seven syscall
/// families (CSpace, Retype, Map, Configure, Endpoint
/// Receive/Send/Call/Reply) and halts for good.
///
/// This, not on_endpoint_send_syscall above, is provably the true
/// final hook in the whole self-test chain: `endpoint::call`'s own
/// second, unconditional block (see endpoint.rs) means this
/// function's body only resumes and runs this log line *after*
/// task0 has been parked a second time, switched away from, and
/// later woken back up by program2's own Reply call. on_endpoint_
/// reply_syscall's own log line for that Reply call is therefore
/// guaranteed to already be on the wire by the time this one fires,
/// exactly mirroring how Send previously superseded Configure's own
/// claim to this role.
pub fn on_endpoint_call_syscall(
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    result: u64,
    label_out: u64,
    data0: u64,
    data1: u64,
) {
    let mut com1 = console::Dual::init();
    let step = ENDPOINT_CALL_STEP.fetch_add(1, Ordering::Relaxed);

    let Some(&(label, ref expected)) = ENDPOINT_CALL_SYSCALL_STEPS.get(step as usize) else {
        let _ = writeln!(
            com1,
            "rose: endpoint call syscall self-test: unexpected step {} (num={:#x}), FAILED",
            step + 1,
            SYS_ENDPOINT_CALL
        );
        ENDPOINT_CALL_ALL_OK.store(false, Ordering::Relaxed);
        return;
    };

    let ok = match *expected {
        ExpectedOutcome::Success => (result as i64) >= 0,
        ExpectedOutcome::Error(code) => result == code,
    };
    if !ok {
        ENDPOINT_CALL_ALL_OK.store(false, Ordering::Relaxed);
    }

    let _ = writeln!(
        com1,
        "rose: endpoint call syscall self-test: step {} {} (args={:#x},{:#x},{:#x},{:#x}) result={:#x} label={:#x} data0={:#x} data1={:#x} {}",
        step + 1,
        label,
        arg1,
        arg2,
        arg3,
        arg4,
        result,
        label_out,
        data0,
        data1,
        if ok { "ok" } else { "FAILED" }
    );

    if step + 1 >= ENDPOINT_CALL_SYSCALL_STEPS.len() as u64 {
        let cspace_ok = CSPACE_SYSCALL_ALL_OK.load(Ordering::Relaxed);
        let untyped_ok = UNTYPED_SYSCALL_ALL_OK.load(Ordering::Relaxed);
        let map_ok = MAP_SYSCALL_ALL_OK.load(Ordering::Relaxed);
        let configure_ok = CONFIGURE_SYSCALL_ALL_OK.load(Ordering::Relaxed);
        let endpoint_receive_ok = ENDPOINT_RECEIVE_ALL_OK.load(Ordering::Relaxed);
        let endpoint_send_ok = ENDPOINT_SEND_ALL_OK.load(Ordering::Relaxed);
        let endpoint_call_ok = ENDPOINT_CALL_ALL_OK.load(Ordering::Relaxed);
        let endpoint_reply_ok = ENDPOINT_REPLY_ALL_OK.load(Ordering::Relaxed);
        let _ = writeln!(
            com1,
            "rose: endpoint call syscall self-test: sequence complete, overall {}",
            if endpoint_call_ok { "confirmed" } else { "FAILED" }
        );
        let _ = writeln!(
            com1,
            "rose: usermode self-test: full boot sequence {}",
            if cspace_ok
                && untyped_ok
                && map_ok
                && configure_ok
                && endpoint_receive_ok
                && endpoint_send_ok
                && endpoint_call_ok
                && endpoint_reply_ok
            {
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
