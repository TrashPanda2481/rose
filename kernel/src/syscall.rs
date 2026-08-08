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

use crate::cspace::{CSpace, CSpaceError};
use crate::paging;
use crate::scheduler;
use crate::thread::{self, ConfigureError};
use crate::untyped::{self, UntypedError};
use abi::{Capability, KernelObjectId, ObjectType, Rights};
use alloc::boxed::Box;

pub const SYS_CSPACE_COPY: u64 = 0x1000;
pub const SYS_CSPACE_MINT: u64 = 0x1001;
pub const SYS_CSPACE_MOVE: u64 = 0x1002;
pub const SYS_CSPACE_REVOKE: u64 = 0x1003;
pub const SYS_UNTYPED_RETYPE: u64 = 0x1004;
pub const SYS_THREAD_CONFIGURE: u64 = 0x1005;

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

/// True if `num` is the Retype syscall. Own predicate, own dispatch
/// function, rather than folding into `is_cspace_syscall`/`dispatch`
/// above: Retype's failure modes come from two different error enums
/// (CSpaceError and UntypedError, see `dispatch_untyped`'s `encode_untyped`),
/// which the CSpace-only `encode` above has no way to represent.
pub fn is_untyped_syscall(num: u64) -> bool {
    num == SYS_UNTYPED_RETYPE
}

/// True if `num` is the Configure syscall. Own predicate/dispatch pair,
/// same reasoning as `is_untyped_syscall` above: Configure's own
/// ConfigureError is a third, unrelated error enum, and its success
/// value (a scheduler task index) has nothing to do with either
/// CSpaceError's or UntypedError's own encoding.
pub fn is_configure_syscall(num: u64) -> bool {
    num == SYS_THREAD_CONFIGURE
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

/// Failure from Retype's dispatch, one variant per error enum it can
/// surface. Kept local to this function's own encoding rather than
/// merged into either CSpaceError or UntypedError: neither of those
/// enums should have to know about the other's existence just because
/// one syscall happens to touch both.
enum RetypeFailure {
    CSpace(CSpaceError),
    Untyped(UntypedError),
}

/// Maps a Retype Result onto the single-register return convention:
/// 0 or positive is success, carrying the new object's
/// KernelObjectId (a physical address for Frame, a registry index for
/// CSpace, see untyped.rs's `retype`) rather than just 0, since
/// there's a real value worth handing back this time. Negative means
/// failure; CSpaceError codes are returned as-is (1-4, same meaning as
/// `encode` above), UntypedError codes are offset by +10 (11-13) so
/// the two enums' codes never collide in the one shared register, see
/// `RetypeFailure`.
fn encode_untyped(result: Result<u64, RetypeFailure>) -> u64 {
    match result {
        Ok(id) => id,
        Err(RetypeFailure::CSpace(e)) => (-(e.code() as i64)) as u64,
        Err(RetypeFailure::Untyped(e)) => (-(e.code() as i64 + 10)) as u64,
    }
}

/// Runs Retype against the calling task's own CSpace. Argument
/// meaning: arg1=untyped cptr, arg2=object_type (raw ObjectType::to_u8
/// value), arg3=dst cptr for the newly minted cap. Order of checks
/// matters and is deliberate: everything that can be checked without
/// consuming a frame from the Untyped pool (both cptrs resolve, the
/// source cap really is Untyped, `object_type` is a real variant,
/// `dst` is empty) happens before calling `untyped::retype`, because
/// a frame `retype` hands out has no way back into the pool if the
/// install after it turns out to be rejected (see untyped.rs's module
/// doc, and `CSpace::slot_is_available`'s doc, on reclamation not
/// existing yet).
pub fn dispatch_untyped(arg1: u64, arg2: u64, arg3: u64) -> u64 {
    let untyped_cptr = arg1 as u32;
    let dst_cptr = arg3 as u32;

    let result = scheduler::with_current_cspace(|cspace| -> Result<u64, RetypeFailure> {
        let untyped_cap = cspace
            .lookup(untyped_cptr)
            .filter(|cap| cap.object_type == ObjectType::Untyped)
            .ok_or(RetypeFailure::Untyped(UntypedError::InvalidUntyped))?;

        let object_type = ObjectType::from_u8(arg2 as u8)
            .ok_or(RetypeFailure::Untyped(UntypedError::UnsupportedType))?;

        if untyped_cptr == dst_cptr {
            return Err(RetypeFailure::CSpace(CSpaceError::InvalidSlot));
        }
        cspace
            .slot_is_available(dst_cptr)
            .map_err(RetypeFailure::CSpace)?;

        let new_id = untyped::retype(untyped_cap.object_ref.0, object_type)
            .map_err(RetypeFailure::Untyped)?;

        // Rights on the freshly minted cap: whatever a cap of this
        // object_type needs to be useful at all, since there's no
        // source cap of the same type to narrow from (unlike
        // copy/mint, this mints a reference to a brand new object,
        // see cspace.rs's `install_child` doc). Frame gets
        // READ/WRITE/MAP since paging will eventually need all three
        // to map it; CSpace gets GRANT since installing further caps
        // into it is the only operation a CSpace cap itself gates.
        // AddressSpace gets MAP, the right a future Frame.Map syscall
        // will need to check on the *target* AddressSpace cap before
        // letting a Frame be mapped into it. Thread gets GRANT, same
        // loose "the one right this type's ops gate" convention as
        // CSpace; provisional until the Configure/Resume/Suspend/
        // SetPriority syscalls actually define per-op permission
        // checks, at which point this may need to split into more
        // than one bit.
        let rights = match object_type {
            ObjectType::Frame => Rights::READ.union(Rights::WRITE).union(Rights::MAP),
            ObjectType::CSpace => Rights::GRANT,
            ObjectType::AddressSpace => Rights::MAP,
            ObjectType::Thread => Rights::GRANT,
            _ => unreachable!("untyped::retype already rejected every other ObjectType"),
        };
        let new_cap = Capability {
            object_ref: KernelObjectId(new_id),
            object_type,
            rights,
            badge: 0,
        };

        cspace
            .install_child(untyped_cptr, dst_cptr, new_cap)
            .map_err(RetypeFailure::CSpace)?;

        Ok(new_id)
    });

    encode_untyped(result)
}

/// Maps a Configure Result onto the single-register return convention:
/// 0 or positive is success, carrying the new task's scheduler index.
/// Negative means failure; ConfigureError codes are offset by +20
/// (21-24) so they never collide with CSpaceError's 1-4 or
/// UntypedError's 11-13 in the one shared register, same banding
/// convention as `encode_untyped`'s own +10 offset for UntypedError.
fn encode_configure(result: Result<usize, ConfigureError>) -> u64 {
    match result {
        Ok(idx) => idx as u64,
        Err(e) => (-(e.code() as i64 + 20)) as u64,
    }
}

/// Runs Configure against the calling task's own CSpace. Argument
/// meaning: arg1=thread cptr, arg2=AddressSpace cptr, arg3=CSpace
/// cptr (0 means "none, build a fresh empty CSpace instead"),
/// arg4=entry, arg5=stack_top.
///
/// Two-phase, same reasoning as `dispatch_untyped`'s ordering comment:
/// every cap lookup/rights check happens first, inside a single
/// `with_current_cspace` call, and only once all three cptrs resolve
/// does this call out to `thread::configure` (outside that closure).
/// This isn't just style: `thread::configure` calls
/// `scheduler::spawn_user`, which takes the same SCHEDULER lock
/// `with_current_cspace` is already holding for the duration of its
/// own closure; calling it from inside that closure would be a
/// self-deadlock against a non-reentrant spinlock, not just untidy
/// nesting.
pub fn dispatch_configure(arg1: u64, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> u64 {
    let thread_cptr = arg1 as u32;
    let as_cptr = arg2 as u32;
    let cspace_cptr = arg3 as u32;

    let prepared = scheduler::with_current_cspace(
        |cspace| -> Result<(u64, paging::AddressSpace, Option<Box<CSpace>>), ConfigureError> {
            let thread_cap = cspace
                .lookup(thread_cptr)
                .filter(|cap| {
                    cap.object_type == ObjectType::Thread && cap.rights.contains(Rights::GRANT)
                })
                .ok_or(ConfigureError::InvalidThread)?;

            let as_cap = cspace
                .lookup(as_cptr)
                .filter(|cap| {
                    cap.object_type == ObjectType::AddressSpace
                        && cap.rights.contains(Rights::MAP)
                })
                .ok_or(ConfigureError::InvalidAddressSpace)?;

            let cspace_box = if cspace_cptr == 0 {
                None
            } else {
                let cs_cap = cspace
                    .lookup(cspace_cptr)
                    .filter(|cap| {
                        cap.object_type == ObjectType::CSpace
                            && cap.rights.contains(Rights::GRANT)
                    })
                    .ok_or(ConfigureError::InvalidCSpace)?;
                Some(
                    untyped::take_cspace(cs_cap.object_ref.0)
                        .ok_or(ConfigureError::InvalidCSpace)?,
                )
            };

            let address_space = paging::AddressSpace::from_raw(as_cap.object_ref.0);
            Ok((thread_cap.object_ref.0, address_space, cspace_box))
        },
    );

    let result = prepared.and_then(|(thread_id, address_space, cspace_box)| {
        thread::configure(thread_id, address_space, cspace_box, arg4, arg5)
    });

    encode_configure(result)
}
