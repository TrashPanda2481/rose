// Untyped/Retype, v0.1.
//
// Untyped is a pool of individually-allocated physical frames, not yet
// shaped into anything (see docs/cores/kernel/README.md, "Untyped/
// Retype"). Retype pops one frame out of that pool and turns it into a
// Frame or CSpace object.
//
// Scope for this feature, and what's explicitly not here yet:
//   - The pool is frames pulled one at a time from mem::FRAME_ALLOCATOR,
//     not a contiguous physical region carved out ahead of it. Real
//     contiguous-region Untyped is boot handoff's job later, once boot
//     handoff is real (see docs/TRANSITION.md, Phase 1), not this
//     feature's.
//   - Frame, CSpace, AddressSpace, and Thread are retypeable.
//     PageTable/Endpoint/Notification/Reply/IrqHandler retyping are
//     each their own future feature, added to `retype`'s match arm
//     when that object type's turn comes. PageTable specifically is a
//     deliberate punt, not just "not implemented yet": paging.rs's
//     AddressSpace::map_page already manages intermediate page-table
//     levels internally, invisible to the capability model, and there's
//     no current need (SMP, page-table sharing) to reason about a
//     table level as its own object. Revisit when something actually
//     needs that, not before.
//   - CSpace and Thread retype each charge one frame out of the pool
//     as bookkeeping only; the actual struct still lives on the kernel
//     heap via Box (CSpace) or a module-local registry (Thread), not
//     placed in that physical frame. Documented asymmetry, not
//     silently dropped. AddressSpace retype also charges one
//     bookkeeping frame from the pool on top of the real PML4 frame
//     `paging::new_address_space` allocates separately from the global
//     frame allocator; same asymmetry, extended consistently rather
//     than given a special case.
//   - Revoking a retyped cap kills the cap and its derivation subtree
//     (cspace.rs's existing revoke, same mechanism every other cap
//     already uses); it does not return frames to FRAME_ALLOCATOR or
//     roll back the watermark. Reclamation semantics are an open
//     question, same category as cspace.rs's existing full-cascade-
//     vs-sealed-caps one.
//   - Frame caps produced here don't do anything yet; nothing calls
//     Map/Unmap on them. This proves a Frame cap can be minted from
//     real memory and tracked, not that it's usable in paging yet.
//   - A retyped Thread is a bare placeholder (`ThreadObject`, empty in
//     v0.1): nothing configures it, gives it a stack/entry point, or
//     makes it schedulable. It isn't wired into scheduler.rs's task
//     list at all yet; that wiring, plus the suspend/resume state a
//     Task needs before it's safe to add to the live round-robin
//     queue, is the Thread.Configure/Resume/Suspend/SetPriority
//     syscall feature's job, not this one's. This registry's index is
//     therefore a placeholder identity, not yet the "task index" the
//     abi crate's own KernelObjectId doc comment anticipates; that
//     future feature is what has to reconcile the two, either by
//     making them the same index space or by translating between them.
//   - A retyped AddressSpace is real and immediately usable as far as
//     paging.rs is concerned (it's the same `paging::AddressSpace` any
//     other caller gets from `new_address_space`), but nothing maps
//     anything into it here; it's an empty address space until some
//     future Frame.Map syscall populates it.

use crate::cspace::CSpace;
use crate::mem::{self, SpinLock};
use crate::paging;
use abi::ObjectType;
use alloc::boxed::Box;
use alloc::vec::Vec;

/// One pool of individually-allocated physical frames, not yet shaped
/// into anything. `watermark` is a bump index into `frames`: index
/// `watermark` is the next frame `retype` hands out. Never decreases
/// in v0.1, see module doc, no reclaim yet.
pub struct UntypedObject {
    frames: Vec<u64>,
    watermark: usize,
}

impl UntypedObject {
    /// Pulls `count` frames from the global frame allocator up front.
    /// `None` if the allocator can't supply that many; whatever it
    /// did hand out before running dry is freed back immediately
    /// rather than left half-populated with nothing to reference it.
    pub fn new(count: usize) -> Option<UntypedObject> {
        let mut frames = Vec::with_capacity(count);
        let mut allocator = mem::FRAME_ALLOCATOR.lock();
        for _ in 0..count {
            match allocator.alloc() {
                Some(phys) => frames.push(phys),
                None => {
                    for phys in frames {
                        unsafe { allocator.free(phys) };
                    }
                    return None;
                }
            }
        }
        Some(UntypedObject {
            frames,
            watermark: 0,
        })
    }

    fn take_frame(&mut self) -> Option<u64> {
        let phys = *self.frames.get(self.watermark)?;
        self.watermark += 1;
        Some(phys)
    }
}

/// Reasons Retype can fail. Own small code space, same pattern as
/// CSpaceError; see syscall.rs for how this gets encoded into a
/// single return register.
#[derive(Debug, PartialEq, Eq)]
pub enum UntypedError {
    /// The cptr didn't resolve to an Untyped cap (wrong object type,
    /// empty slot), or its KernelObjectId doesn't resolve to a live
    /// entry in this module's own registry.
    InvalidUntyped,
    /// Pool has no frames left at the requested watermark.
    Exhausted,
    /// `object_type` isn't one of the retypeable types listed in this
    /// module's own doc comment (Frame, CSpace, AddressSpace, Thread
    /// as of this feature).
    UnsupportedType,
}

impl UntypedError {
    /// Stable small integer per variant, same convention as
    /// `CSpaceError::code`. Kept next to the enum so the two never
    /// drift apart.
    pub fn code(&self) -> u8 {
        match self {
            UntypedError::InvalidUntyped => 1,
            UntypedError::Exhausted => 2,
            UntypedError::UnsupportedType => 3,
        }
    }
}

/// Global registry of live UntypedObjects, indexed by KernelObjectId
/// (a fresh incrementing counter). Unlike AddressSpace, which reuses
/// its PML4 physical address as a stable identity, Untyped has no
/// single natural address once its pool isn't one contiguous region,
/// so this registry is what a cap's KernelObjectId actually resolves
/// through.
static UNTYPED_OBJECTS: SpinLock<Vec<Option<UntypedObject>>> = SpinLock::new(Vec::new());

/// Global registry of CSpace objects created by Retype. Separate
/// counter/namespace from UNTYPED_OBJECTS: KernelObjectId's meaning is
/// per-ObjectType, not global, per abi's own doc comment on it.
static CSPACE_OBJECTS: SpinLock<Vec<Option<Box<CSpace>>>> = SpinLock::new(Vec::new());

/// A retyped-but-unconfigured Thread. Deliberately empty in v0.1: see
/// this module's own doc comment for why (no scheduler wiring, no
/// suspend/resume state, that's the Configure/Resume/Suspend/
/// SetPriority syscall feature's job). Exists as its own type rather
/// than a bare `()` placeholder so it has somewhere to grow into once
/// that feature lands, same spirit as every other "prove it once,
/// harden later" type in this codebase.
pub struct ThreadObject;

/// Global registry of Thread placeholders created by Retype. Separate
/// counter/namespace from UNTYPED_OBJECTS and CSPACE_OBJECTS, same
/// per-ObjectType KernelObjectId reasoning as CSPACE_OBJECTS above.
static THREAD_OBJECTS: SpinLock<Vec<Option<ThreadObject>>> = SpinLock::new(Vec::new());

/// Adds `object` to the registry and returns the id to use as its
/// Capability's KernelObjectId. Called from boot handoff (main.rs)
/// today, standing in for the real root task's Untyped grant until
/// there is one.
pub fn register_untyped(object: UntypedObject) -> u64 {
    let mut objects = UNTYPED_OBJECTS.lock();
    objects.push(Some(object));
    (objects.len() - 1) as u64
}

fn register_cspace(cspace: Box<CSpace>) -> u64 {
    let mut objects = CSPACE_OBJECTS.lock();
    objects.push(Some(cspace));
    (objects.len() - 1) as u64
}

fn register_thread(thread: ThreadObject) -> u64 {
    let mut objects = THREAD_OBJECTS.lock();
    objects.push(Some(thread));
    (objects.len() - 1) as u64
}

/// Retypes one frame out of the Untyped object at `untyped_id` into
/// `object_type`, returning the new object's KernelObjectId:
///   - Frame: the frame's own physical address.
///   - CSpace: a CSPACE_OBJECTS registry index.
///   - AddressSpace: the freshly built AddressSpace's own PML4
///     physical address (`paging::AddressSpace::raw()`), same "reuse
///     an existing stable identity" pattern as Frame, not a registry
///     index.
///   - Thread: a THREAD_OBJECTS registry index. Placeholder identity
///     only; see this module's own doc comment on why it isn't yet
///     the real scheduler task index.
/// Doesn't touch any CSpace slot directly; the caller (syscall.rs's
/// `dispatch_untyped`) is responsible for installing the resulting
/// Capability as a child of the Untyped cap, same derivation-tree
/// pattern copy/mint already use.
pub fn retype(untyped_id: u64, object_type: ObjectType) -> Result<u64, UntypedError> {
    if !matches!(
        object_type,
        ObjectType::Frame | ObjectType::CSpace | ObjectType::AddressSpace | ObjectType::Thread
    ) {
        return Err(UntypedError::UnsupportedType);
    }

    let phys = {
        let mut objects = UNTYPED_OBJECTS.lock();
        let index = untyped_id as usize;
        let slot = objects
            .get_mut(index)
            .and_then(|entry| entry.as_mut())
            .ok_or(UntypedError::InvalidUntyped)?;
        slot.take_frame().ok_or(UntypedError::Exhausted)?
    };

    match object_type {
        ObjectType::Frame => Ok(phys),
        ObjectType::CSpace => Ok(register_cspace(Box::new(CSpace::new()))),
        // `phys` above was only a bookkeeping charge against the pool
        // (see module doc); the real PML4 frame comes from
        // `new_address_space`'s own call into the global frame
        // allocator, a second, separate frame. `init()` (paging.rs)
        // must have already run by the time any Untyped pool exists
        // for a caller to retype from, so this unsafe contract is
        // always satisfied here.
        ObjectType::AddressSpace => Ok(unsafe { paging::new_address_space() }.raw()),
        ObjectType::Thread => Ok(register_thread(ThreadObject)),
        _ => unreachable!("checked above"),
    }
}
