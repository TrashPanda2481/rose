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
use crate::endpoint::{self, EndpointError, Message};
use crate::mem;
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
pub const SYS_FRAME_MAP: u64 = 0x1006;
pub const SYS_ENDPOINT_SEND: u64 = 0x1007;
pub const SYS_ENDPOINT_RECEIVE: u64 = 0x1008;

/// Frame-Map's own flags argument: a small, clean bitfield instead of
/// raw hardware PTE bits. paging.rs's PAGE_* constants are an
/// implementation detail this syscall boundary shouldn't leak, same
/// "userspace never sees the raw representation" reasoning as
/// Rights::bits() existing instead of exposing a derivation-tree
/// pointer directly.
pub const MAP_FLAG_WRITABLE: u64 = 1 << 0;
pub const MAP_FLAG_NO_EXECUTE: u64 = 1 << 1;

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

/// True if `num` is the Frame-Map syscall. Own predicate/dispatch
/// pair, same reasoning as `is_configure_syscall` above: MapError is
/// its own small enum, unrelated to CSpaceError/UntypedError/
/// ConfigureError.
pub fn is_frame_map_syscall(num: u64) -> bool {
    num == SYS_FRAME_MAP
}

/// True if `num` is the Endpoint-Send syscall. Own predicate/dispatch
/// pair, same reasoning as `is_frame_map_syscall` above: EndpointError
/// is its own small enum, unrelated to CSpaceError/UntypedError/
/// ConfigureError/MapError.
pub fn is_endpoint_send_syscall(num: u64) -> bool {
    num == SYS_ENDPOINT_SEND
}

/// True if `num` is the Endpoint-Receive syscall. Separate from
/// `is_endpoint_send_syscall` even though both dispatch through the
/// same EndpointError: idt.rs's own dispatch chain needs to route
/// Receive to a call that writes back three output registers
/// (rsi/rdx/r10), not just one (rax) like every syscall before it, so
/// it needs a syscall-number check specific to Receive to decide
/// which shape of call to make.
pub fn is_endpoint_receive_syscall(num: u64) -> bool {
    num == SYS_ENDPOINT_RECEIVE
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
        // MEDIUM fix: gate Retype on Rights::WRITE, matching every
        // other typed cap argument in this file (Configure checks
        // GRANT/MAP on its three args, Frame-Map checks MAP/WRITE).
        // Previously only object_type was checked, so mint-ing an
        // Untyped cap down to Rights::NONE (the standard way to hand a
        // narrowed-confinement view to a less-trusted delegate) had no
        // effect on Retype -- the "rights bound what you can do with a
        // cap" invariant held for every other object type but this
        // one. WRITE matches the boot-handoff grant (full_rights =
        // READ|WRITE|MAP), so this doesn't change behavior for any
        // existing caller.
        let untyped_cap = cspace
            .lookup(untyped_cptr)
            .filter(|cap| {
                cap.object_type == ObjectType::Untyped && cap.rights.contains(Rights::WRITE)
            })
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
            // Endpoint gets both SEND and RECEIVE on a fresh retype:
            // there's no source cap of the same type to narrow rights
            // from (same "brand new object" reasoning as every other
            // arm here), and the self-test's own on_untyped_syscall
            // hook is what narrows the copy it grants into the second
            // task's CSpace down to RECEIVE only, via
            // `untyped::grant_into_cspace` -- not this match arm.
            ObjectType::Endpoint => Rights::SEND.union(Rights::RECEIVE),
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

            // MEDIUM fix ("verify before commit"): check the thread
            // isn't already configured BEFORE the irreversible
            // untyped::take_cspace call below. take_cspace removes its
            // entry from CSPACE_OBJECTS on success with no way back;
            // calling it before this check meant a second Configure
            // against an already-configured thread, with a real
            // CSpace cptr, silently destroyed the caller's CSpace even
            // though the syscall reported failure. This same check
            // also still runs inside thread::configure right before
            // claim_thread, kept there too as the authoritative,
            // closest-to-the-actual-commit guard -- this one exists so
            // the failure is caught before take_cspace, not just
            // before spawn_user.
            untyped::peek_thread_unconfigured(thread_cap.object_ref.0)?;

            let as_cap = cspace
                .lookup(as_cptr)
                .filter(|cap| {
                    cap.object_type == ObjectType::AddressSpace
                        && cap.rights.contains(Rights::MAP)
                })
                .ok_or(ConfigureError::InvalidAddressSpace)?;

            let address_space = paging::AddressSpace::from_raw(as_cap.object_ref.0);

            // CRITICAL fix: reject the kernel's own AddressSpace as a
            // Configure target, same reasoning as Frame-Map's own
            // check (see dispatch_frame_map). Scheduling a task to run
            // against the kernel's live PML4 is never legitimate.
            if address_space.is_kernel() {
                return Err(ConfigureError::InvalidAddressSpace);
            }

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

            Ok((thread_cap.object_ref.0, address_space, cspace_box))
        },
    );

    let result = prepared.and_then(|(thread_id, address_space, cspace_box)| {
        thread::configure(thread_id, address_space, cspace_box, arg4, arg5)
    });

    encode_configure(result)
}

/// Reasons Frame-Map can fail. Own small code space, same pattern as
/// every other syscall's error enum here.
#[derive(Debug, PartialEq, Eq)]
pub enum MapError {
    /// The frame cptr didn't resolve to a usable Frame cap (wrong
    /// object type, missing MAP right), or the caller asked for a
    /// writable mapping (MAP_FLAG_WRITABLE) out of a Frame cap that
    /// was never granted WRITE.
    InvalidFrame,
    /// The AddressSpace cptr didn't resolve to a usable AddressSpace
    /// cap (wrong object type, missing MAP right).
    InvalidAddressSpace,
    /// `vaddr` isn't 4K-aligned. paging.rs's map_page has no
    /// alignment check of its own (every existing caller already
    /// passes aligned constants), so this syscall is the first place
    /// an unaligned value could actually arrive from outside the
    /// kernel's own control.
    Misaligned,
    /// `vaddr` falls in the PML4 range every AddressSpace shares BY
    /// PHYSICAL REFERENCE with the kernel's own tables (see
    /// `paging::is_shared_vaddr`). Mapping there from ANY AddressSpace
    /// writes into the same shared page-table structures the kernel
    /// itself runs from and every other AddressSpace also sees.
    VaddrNotPrivate,
    /// The AddressSpace cptr resolved to the kernel's own AddressSpace.
    /// Never a valid Frame-Map/Configure target: even a vaddr outside
    /// the shared range would still be writing into the kernel's own
    /// live PML4's private half, which nothing else populates today
    /// but is still the same page tables the kernel runs from.
    AddressSpaceIsKernel,
}

impl MapError {
    /// Stable small integer per variant, same convention as every
    /// other error enum's own `code()`.
    pub fn code(&self) -> u8 {
        match self {
            MapError::InvalidFrame => 1,
            MapError::InvalidAddressSpace => 2,
            MapError::Misaligned => 3,
            MapError::VaddrNotPrivate => 4,
            MapError::AddressSpaceIsKernel => 5,
        }
    }
}

/// Maps a Frame-Map Result onto the single-register return convention.
/// +30 offset: the next free band after CSpaceError's bare 1-4,
/// UntypedError's +10 (11-13), and ConfigureError's +20 (21-24), same
/// non-colliding-band convention as `encode_configure`'s own comment.
fn encode_map(result: Result<(), MapError>) -> u64 {
    match result {
        Ok(()) => 0,
        Err(e) => (-(e.code() as i64 + 30)) as u64,
    }
}

/// Runs Frame-Map against the calling task's own CSpace. Argument
/// meaning: arg1=frame cptr, arg2=AddressSpace cptr, arg3=virtual
/// address, arg4=flags (MAP_FLAG_* bits above).
///
/// This is the first syscall that calls `AddressSpace::map_page`
/// through the capability layer instead of a kernel-held raw handle;
/// it closes exactly the gap untyped.rs's own module doc flagged
/// ("Frame caps produced here don't do anything yet ... it's an empty
/// address space until some future Frame.Map syscall populates it").
///
/// Always adds PAGE_USER: this syscall exists to populate a target
/// AddressSpace with user-mapped pages, not to let a task remap the
/// kernel's own tables. Originally the only guard against a
/// kernel-target mapping; superseded (not removed -- still correct
/// for what it does) by two explicit checks below after an audit
/// found PAGE_USER alone doesn't stop a task from targeting the
/// kernel's own AddressSpace or its shared upper half: `vaddr` is
/// rejected outright if it falls in the shared range
/// (`paging::is_shared_vaddr`), and the target AddressSpace is
/// rejected outright if it's the kernel's own
/// (`AddressSpace::is_kernel`). See docs/cores/kernel/BUGS.md.
pub fn dispatch_frame_map(arg1: u64, arg2: u64, arg3: u64, arg4: u64) -> u64 {
    let frame_cptr = arg1 as u32;
    let as_cptr = arg2 as u32;
    let vaddr = arg3;
    let flag_bits = arg4;

    let result = scheduler::with_current_cspace(|cspace| -> Result<(), MapError> {
        let frame_cap = cspace
            .lookup(frame_cptr)
            .filter(|cap| {
                cap.object_type == ObjectType::Frame && cap.rights.contains(Rights::MAP)
            })
            .ok_or(MapError::InvalidFrame)?;

        let as_cap = cspace
            .lookup(as_cptr)
            .filter(|cap| {
                cap.object_type == ObjectType::AddressSpace && cap.rights.contains(Rights::MAP)
            })
            .ok_or(MapError::InvalidAddressSpace)?;

        if vaddr % mem::FRAME_SIZE != 0 {
            return Err(MapError::Misaligned);
        }

        // CRITICAL fix: reject any vaddr in the range every
        // AddressSpace shares BY PHYSICAL REFERENCE with the kernel's
        // own tables. Without this, a task holding only the default
        // rights `retype` already grants (any Frame cap, any
        // AddressSpace cap) could map an attacker-chosen frame
        // directly over kernel .text -- with PAGE_USER set, executable
        // -- from ring 3. See docs/cores/kernel/BUGS.md.
        if paging::is_shared_vaddr(vaddr) {
            return Err(MapError::VaddrNotPrivate);
        }

        let address_space = paging::AddressSpace::from_raw(as_cap.object_ref.0);

        // CRITICAL fix, independent of the vaddr check above: reject
        // the kernel's own AddressSpace as a target outright. The
        // vaddr check alone doesn't stop a task from mapping into the
        // kernel AddressSpace's own PRIVATE (index<256) half -- still
        // the exact same PML4 the kernel itself runs from.
        if address_space.is_kernel() {
            return Err(MapError::AddressSpaceIsKernel);
        }

        // Requesting a writable mapping out of a Frame cap that was
        // never granted WRITE would let a read-only capability become
        // writable memory just by picking a flag bit; reject it the
        // same "rights gate what you can do, not just whether the cap
        // exists" way every other syscall here already does.
        if flag_bits & MAP_FLAG_WRITABLE != 0 && !frame_cap.rights.contains(Rights::WRITE) {
            return Err(MapError::InvalidFrame);
        }

        let mut flags = paging::PAGE_USER;
        if flag_bits & MAP_FLAG_WRITABLE != 0 {
            flags |= paging::PAGE_WRITABLE;
        }
        // HIGH fix (W^X): a writable mapping is never also executable
        // through this syscall, regardless of whether the caller
        // separately asked for MAP_FLAG_NO_EXECUTE. Previously the two
        // flag bits were independent, so MAP_FLAG_WRITABLE alone
        // produced a page that was simultaneously writable and
        // executable -- the exact policy paging::build()'s own kernel-
        // image mapping documents itself as enforcing "from the very
        // first kernel-owned page table".
        if flag_bits & MAP_FLAG_NO_EXECUTE != 0 || flag_bits & MAP_FLAG_WRITABLE != 0 {
            flags |= paging::PAGE_NO_EXECUTE;
        }

        unsafe { address_space.map_page(vaddr, frame_cap.object_ref.0, flags) };
        Ok(())
    });

    encode_map(result)
}

/// Maps an EndpointError onto the single-register return convention.
/// +40 offset: the next free band after CSpaceError's bare 1-4,
/// UntypedError's +10 (11-13), ConfigureError's +20 (21-24), and
/// MapError's +30 (31-35), same non-colliding-band convention as
/// every earlier `encode_*` function's own comment.
fn encode_endpoint_error(e: EndpointError) -> u64 {
    (-(e.code() as i64 + 40)) as u64
}

/// Runs Endpoint-Send against the calling task's own CSpace. Argument
/// meaning: arg1=endpoint cptr, arg2=label, arg3=data0, arg4=data1.
/// Returns 0 on success (rax only; Send never hands back a value
/// beyond success/failure), negative EndpointError on failure.
///
/// Two-phase, same reasoning as `dispatch_configure`'s own comment:
/// the cap lookup/rights check happens inside a single
/// `with_current_cspace` call, and `endpoint::send` -- which may call
/// `scheduler::block_current_and_switch`, itself taking the same
/// SCHEDULER lock `with_current_cspace` is already holding for the
/// duration of its own closure -- only runs after that closure
/// returns. Calling it from inside the closure would be a
/// self-deadlock against a non-reentrant spinlock, not just untidy
/// nesting.
pub fn dispatch_endpoint_send(arg1: u64, arg2: u64, arg3: u64, arg4: u64) -> u64 {
    let endpoint_cptr = arg1 as u32;
    let label = arg2 as u32;
    let data0 = arg3;
    let data1 = arg4;

    let looked_up = scheduler::with_current_cspace(|cspace| {
        cspace
            .lookup(endpoint_cptr)
            .filter(|cap| {
                cap.object_type == ObjectType::Endpoint && cap.rights.contains(Rights::SEND)
            })
            .map(|cap| cap.object_ref.0)
    });

    let Some(endpoint_id) = looked_up else {
        return encode_endpoint_error(EndpointError::InvalidEndpoint);
    };

    let message = Message {
        label,
        data0,
        data1,
    };
    match endpoint::send(endpoint_id, message) {
        Ok(()) => 0,
        Err(e) => encode_endpoint_error(e),
    }
}

/// Runs Endpoint-Receive against the calling task's own CSpace.
/// Argument meaning: arg1=endpoint cptr. Unlike every syscall above,
/// this is the first one that hands back more than a single value on
/// success: the returned tuple is (rax, rsi, rdx, r10) exactly as
/// idt.rs's dispatch chain needs to write them back into the trapped
/// frame -- rax=0/error, rsi=label, rdx=data0, r10=data1 (0s on
/// failure, not meaningful).
///
/// Same two-phase reasoning as `dispatch_endpoint_send` above: the cap
/// lookup happens inside `with_current_cspace`; `endpoint::receive`
/// (which may block) runs only after that closure returns.
pub fn dispatch_endpoint_receive(arg1: u64) -> (u64, u64, u64, u64) {
    let endpoint_cptr = arg1 as u32;

    let looked_up = scheduler::with_current_cspace(|cspace| {
        cspace
            .lookup(endpoint_cptr)
            .filter(|cap| {
                cap.object_type == ObjectType::Endpoint && cap.rights.contains(Rights::RECEIVE)
            })
            .map(|cap| cap.object_ref.0)
    });

    let Some(endpoint_id) = looked_up else {
        return (encode_endpoint_error(EndpointError::InvalidEndpoint), 0, 0, 0);
    };

    match endpoint::receive(endpoint_id) {
        Ok(message) => (0, message.label as u64, message.data0, message.data1),
        Err(e) => (encode_endpoint_error(e), 0, 0, 0),
    }
}
