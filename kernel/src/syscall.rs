// CSpace syscall dispatch. Pure mechanism only: decodes a syscall
// number and four argument registers into a CSpace operation on the
// calling task's own CSpace, and encodes the Result back into a
// single u64 return value. No logging, no self-test knowledge; that
// lives in usermode.rs's on_cspace_syscall, which wraps this.
//
// ABI: rax = syscall number on entry, args in rdi/rsi/rdx/r10 (not
// rcx: rcx is clobbered by the `syscall` instruction on real
// hardware, though this kernel currently enters via `int 0x80`, not
// `syscall`; r10 is used in its place anyway to keep the convention
// future-proof for a real `syscall`/`sysret` path later). Return
// value in rax on exit: 0 or positive means success, negative means
// an error whose magnitude is the CSpaceError code.

use crate::cspace::CSpaceError;
use crate::scheduler;
use abi::Rights;

pub const SYS_CSPACE_COPY: u64 = 0x1000;
pub const SYS_CSPACE_MINT: u64 = 0x1001;
pub const SYS_CSPACE_MOVE: u64 = 0x1002;
pub const SYS_CSPACE_REVOKE: u64 = 0x1003;

/// True if `num` is one of the syscall numbers this module handles.
/// idt.rs uses this to route between the new CSpace path and the
/// legacy sentinel-value self-test path, which uses unrelated values
/// (0xabcd1234, 1) that predate this ABI.
pub fn is_cspace_syscall(num: u64) -> bool {
    matches!(
        num,
        SYS_CSPACE_COPY | SYS_CSPACE_MINT | SYS_CSPACE_MOVE | SYS_CSPACE_REVOKE
    )
}

/// Maps a CSpace op's Result onto the single-register return
/// convention described above.
fn encode(result: Result<(), CSpaceError>) -> u64 {
    match result {
        Ok(()) => 0,
        Err(e) => (-(e.code() as i64)) as u64,
    }
}

/// Runs `num` against the calling task's own CSpace (via
/// `scheduler::with_current_cspace`) and returns the encoded result.
/// Argument meaning depends on `num`:
///   COPY:   arg1=src, arg2=dst, arg3=rights bits
///   MINT:   arg1=src, arg2=dst, arg3=rights bits, arg4=badge
///   MOVE:   arg1=src, arg2=dst
///   REVOKE: arg1=target
/// `num` not matching any SYS_CSPACE_* here just falls through to the
/// InvalidSlot encoding; callers are expected to have already checked
/// `is_cspace_syscall` before reaching this, so that arm only exists
/// so this function is total.
pub fn dispatch(num: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64) -> u64 {
    let result = scheduler::with_current_cspace(|cspace| match num {
        SYS_CSPACE_COPY => {
            let rights = Rights::from_bits(arg3 as u8);
            cspace.copy(arg1 as u32, arg2 as u32, rights)
        }
        SYS_CSPACE_MINT => {
            let rights = Rights::from_bits(arg3 as u8);
            cspace.mint(arg1 as u32, arg2 as u32, rights, arg4)
        }
        SYS_CSPACE_MOVE => cspace.move_slot(arg1 as u32, arg2 as u32),
        SYS_CSPACE_REVOKE => cspace.revoke(arg1 as u32),
        _ => Err(CSpaceError::InvalidSlot),
    });
    encode(result)
}
