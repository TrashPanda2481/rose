// CSpace: one component's capability table, addressed by local
// integers (CPtr). Implements the "CSpace" and "Deriving capabilities"
// sections of docs/cores/kernel/README.md.
//
// Scope for this feature: the derivation tree (copy/mint/revoke) and
// rights-narrowing are real and self-tested. What's deliberately not
// here yet:
//   - No syscall reaches any of this from ring 3. Nothing calls these
//     methods except this file's own self-test. Wiring a CSpace onto
//     Task and dispatching a syscall into it is the next feature, not
//     this one; there's no ring-3 CPtr to resolve until then.
//   - `move_slot` only re-homes a cap within the SAME CSpace. Real
//     cross-component Move (a cap actually leaving one component's
//     table and landing in another's) needs a delivery mechanism, and
//     that's what IPC is for. IPC doesn't exist yet, so the
//     cross-component case isn't reachable to test either. This still
//     exercises the "sender loses it, one slot occupies at a time"
//     part of Move, just not the cross-CSpace part.
//   - No Untyped/Retype. The Capability objects installed here point
//     at kernel objects that already exist by other means (an
//     AddressSpace's own PML4 phys address as its KernelObjectId, see
//     `paging::AddressSpace::raw`), not at freshly-retyped memory.
//     Building a real kernel-object heap out of Untyped is its own
//     feature, deferred until something other than a self-test needs
//     to retype memory into a new object.
//     Update: Untyped/Retype landed as its own feature (see
//     kernel/src/untyped.rs). `install_child` below is what lets
//     Retype's syscall install a freshly-created Frame/CSpace cap as
//     a child of the Untyped cap it came from, reusing this file's
//     existing derivation tree instead of building a second one.

use abi::{Capability, CPtr, Rights};

/// v0.1 fixed size, matches the CPtr's practical range for a single
/// component this early; growable CSpaces are a later concern, same
/// category of scope cut as the heap's fixed 1MiB region. Slot 0 is
/// never used, CPtr 0 is the reserved null cap per abi's doc comment.
const CSPACE_SLOTS: usize = 64;

struct CapSlot {
    cap: Capability,
    /// Slot this one was derived from via copy/mint, in this same
    /// CSpace. `None` for a root grant with no parent (nothing to
    /// cascade-revoke into it from above).
    parent: Option<CPtr>,
    /// Every slot derived from this one, direct children only;
    /// `revoke` walks this recursively. Vec, not a fixed array: v0.1
    /// doesn't bound fan-out, correctness first, a real component
    /// with a big derivation tree can revisit this later.
    children: alloc::vec::Vec<CPtr>,
}

pub struct CSpace {
    slots: [Option<CapSlot>; CSPACE_SLOTS],
}

/// Reasons a CSpace operation can fail. Kept small on purpose: this
/// isn't a syscall ABI yet (see module doc), just enough for the
/// self-test and whatever wires a syscall handler into this later to
/// report something sane.
#[derive(Debug, PartialEq, Eq)]
pub enum CSpaceError {
    /// CPtr 0, or >= CSPACE_SLOTS.
    InvalidSlot,
    /// Source slot has no capability in it.
    SourceEmpty,
    /// Destination slot already holds a capability; every op here
    /// requires an empty destination, no silent overwrite.
    DestOccupied,
    /// Requested rights aren't a subset of the source cap's rights.
    RightsEscalation,
}

impl CSpaceError {
    /// Stable small integer per variant. Used by the syscall layer
    /// (syscall.rs) to encode a failure into a single return register:
    /// 0 or positive means success, negative means an error whose
    /// magnitude is this code. Kept next to the enum so the two never
    /// drift apart; nothing outside this file should invent its own
    /// mapping.
    pub fn code(&self) -> u8 {
        match self {
            CSpaceError::InvalidSlot => 1,
            CSpaceError::SourceEmpty => 2,
            CSpaceError::DestOccupied => 3,
            CSpaceError::RightsEscalation => 4,
        }
    }
}

impl CSpace {
    pub fn new() -> CSpace {
        CSpace {
            slots: [(); CSPACE_SLOTS].map(|_| None),
        }
    }

    fn check_slot(cptr: CPtr) -> Result<usize, CSpaceError> {
        let index = cptr as usize;
        if cptr == 0 || index >= CSPACE_SLOTS {
            return Err(CSpaceError::InvalidSlot);
        }
        Ok(index)
    }

    /// Reads back the capability in `cptr`, if any. This is a plain
    /// lookup, not itself a rights check; whoever calls an operation
    /// on the returned cap is responsible for checking its rights,
    /// same as every other method here.
    pub fn lookup(&self, cptr: CPtr) -> Option<Capability> {
        let index = Self::check_slot(cptr).ok()?;
        self.slots[index].as_ref().map(|slot| slot.cap)
    }

    /// Installs `cap` directly into `dst` with no parent. Only meant
    /// for a root grant (the handoff point authority comes from
    /// nothing, matching "Boot handoff": this stands in for that
    /// until there's a real root task to receive it). Every other
    /// capability in the system should trace back to a `copy`/`mint`
    /// of something that ultimately came in through here.
    pub fn grant_root(&mut self, dst: CPtr, cap: Capability) -> Result<(), CSpaceError> {
        let index = Self::check_slot(dst)?;
        if self.slots[index].is_some() {
            return Err(CSpaceError::DestOccupied);
        }
        self.slots[index] = Some(CapSlot {
            cap,
            parent: None,
            children: alloc::vec::Vec::new(),
        });
        Ok(())
    }

    /// New slot, same object, rights unchanged (or explicitly
    /// narrowed via `rights`), badge unchanged. Sibling of `src` in
    /// the derivation tree: both get revoked together if their common
    /// parent is revoked, but revoking one doesn't touch the other.
    pub fn copy(
        &mut self,
        src: CPtr,
        dst: CPtr,
        rights: Rights,
    ) -> Result<(), CSpaceError> {
        self.derive(src, dst, rights, None)
    }

    /// Same as `copy`, but records `dst` as a child of `src` (not a
    /// sibling) and sets `badge`. This is the only op that changes
    /// badge; a plain `copy` always keeps the source's badge.
    pub fn mint(
        &mut self,
        src: CPtr,
        dst: CPtr,
        rights: Rights,
        badge: u64,
    ) -> Result<(), CSpaceError> {
        self.derive(src, dst, rights, Some(badge))
    }

    fn derive(
        &mut self,
        src: CPtr,
        dst: CPtr,
        rights: Rights,
        new_badge: Option<u64>,
    ) -> Result<(), CSpaceError> {
        let src_index = Self::check_slot(src)?;
        let dst_index = Self::check_slot(dst)?;
        if src_index == dst_index {
            return Err(CSpaceError::InvalidSlot);
        }
        let src_cap = self.slots[src_index]
            .as_ref()
            .ok_or(CSpaceError::SourceEmpty)?
            .cap;
        if self.slots[dst_index].is_some() {
            return Err(CSpaceError::DestOccupied);
        }
        if !rights.is_subset_of(src_cap.rights) {
            return Err(CSpaceError::RightsEscalation);
        }

        let mut derived = src_cap;
        derived.rights = rights;
        if let Some(badge) = new_badge {
            derived.badge = badge;
        }

        // Both copy and mint record dst as a child of src: "revoke
        // destroys the cap and everything derived from it" only holds
        // if every copy is tracked as something derived from its
        // source, not floated off as an untracked sibling. A plain
        // copy and a mint differ in badge/rights, not in whether
        // revoking the source should take them down too.
        let parent = Some(src);

        self.slots[dst_index] = Some(CapSlot {
            cap: derived,
            parent,
            children: alloc::vec::Vec::new(),
        });

        if let Some(parent) = parent {
            let parent_index = Self::check_slot(parent)?;
            self.slots[parent_index]
                .as_mut()
                .expect("rose: cspace: parent slot vanished mid-derive")
                .children
                .push(dst);
        }

        Ok(())
    }

    /// Checks `dst` is a real, currently-empty slot, without
    /// installing anything into it. `lookup` alone can't answer this:
    /// it returns `None` for both an invalid cptr and a valid empty
    /// one, and Retype (untyped.rs's `retype`, called from
    /// syscall.rs's `dispatch_untyped`) needs to tell those apart
    /// *before* consuming a frame from the Untyped pool, not after;
    /// a frame consumed against a `dst` that turns out to be invalid
    /// has no way back into the pool (see untyped.rs's module doc on
    /// reclamation not existing yet).
    pub fn slot_is_available(&self, cptr: CPtr) -> Result<(), CSpaceError> {
        let index = Self::check_slot(cptr)?;
        if self.slots[index].is_some() {
            return Err(CSpaceError::DestOccupied);
        }
        Ok(())
    }

    /// Installs `cap` into `dst`, recorded as a child of `parent` in
    /// the derivation tree, same bookkeeping `copy`/`mint` already do
    /// via `derive`. `parent` must already hold a capability; `dst`
    /// must be empty. Used by Retype (untyped.rs's `retype`, wired up
    /// through syscall.rs's `dispatch_untyped`) to install a freshly
    /// created Frame/CSpace cap as a child of the Untyped cap it was
    /// retyped from, so revoking the Untyped cascades into it for
    /// free, same as any other derived cap. Doesn't itself check
    /// `cap`'s rights against `parent`'s: unlike `derive`, this isn't
    /// narrowing a view of the same object, it's installing a
    /// reference to a brand new one, so there's no source rights set
    /// to be a subset of.
    pub fn install_child(
        &mut self,
        parent: CPtr,
        dst: CPtr,
        cap: Capability,
    ) -> Result<(), CSpaceError> {
        let parent_index = Self::check_slot(parent)?;
        let dst_index = Self::check_slot(dst)?;
        if parent_index == dst_index {
            return Err(CSpaceError::InvalidSlot);
        }
        if self.slots[parent_index].is_none() {
            return Err(CSpaceError::SourceEmpty);
        }
        if self.slots[dst_index].is_some() {
            return Err(CSpaceError::DestOccupied);
        }

        self.slots[dst_index] = Some(CapSlot {
            cap,
            parent: Some(parent),
            children: alloc::vec::Vec::new(),
        });
        self.slots[parent_index]
            .as_mut()
            .expect("rose: cspace: parent slot vanished mid-install")
            .children
            .push(dst);
        Ok(())
    }

    /// Re-homes the capability in `src` to `dst`: `dst` ends up with
    /// exactly what `src` had (object, rights, badge, tree position),
    /// `src` ends up empty. One occupant at a time, no duplication,
    /// which is the part of Move this feature can actually exercise
    /// without IPC; see module doc for the cross-CSpace half that
    /// isn't reachable yet.
    pub fn move_slot(&mut self, src: CPtr, dst: CPtr) -> Result<(), CSpaceError> {
        let src_index = Self::check_slot(src)?;
        let dst_index = Self::check_slot(dst)?;
        if src_index == dst_index {
            return Err(CSpaceError::InvalidSlot);
        }
        if self.slots[src_index].is_none() {
            return Err(CSpaceError::SourceEmpty);
        }
        if self.slots[dst_index].is_some() {
            return Err(CSpaceError::DestOccupied);
        }

        let moved = self.slots[src_index].take().unwrap();

        // Repoint the parent's child-list entry, and every child's
        // parent-pointer, from src to dst; the tree shape doesn't
        // change, only which CPtr names this node.
        if let Some(parent) = moved.parent {
            let parent_index = Self::check_slot(parent)?;
            if let Some(parent_slot) = self.slots[parent_index].as_mut() {
                for child in parent_slot.children.iter_mut() {
                    if *child == src {
                        *child = dst;
                    }
                }
            }
        }
        for child in moved.children.iter() {
            let child_index = Self::check_slot(*child)?;
            if let Some(child_slot) = self.slots[child_index].as_mut() {
                child_slot.parent = Some(dst);
            }
        }

        self.slots[dst_index] = Some(moved);
        Ok(())
    }

    /// Destroys the capability in `target` and everything derived
    /// from it, direct and indirect. Full cascade, no sealed-cap
    /// escape; matches the v0.1 default the README already commits
    /// to under "Deriving capabilities" (revisit only once there's an
    /// object store worth the EROS-style alternative).
    pub fn revoke(&mut self, target: CPtr) -> Result<(), CSpaceError> {
        let index = Self::check_slot(target)?;
        if self.slots[index].is_none() {
            return Err(CSpaceError::SourceEmpty);
        }
        self.revoke_subtree(target)?;
        Ok(())
    }

    // Iterative, not recursive: revoke used to recurse once per tree
    // level, so teardown depth rode on the kernel stack. Bounded today
    // (one CSpace, CSPACE_SLOTS deep at most), but a long derivation
    // chain, or one spanning CSpaces once IPC/Move can carry caps
    // between them, would eventually be able to overflow it. An explicit
    // work stack moves that depth onto the heap, where it's bounded by
    // available memory rather than by a fixed kernel stack. This does
    // not make revoke preemptible or bound its total work; a large tree
    // is still a long non-preemptible operation (a real interrupt-
    // latency concern, tracked separately), it just can no longer take
    // out the stack on the way.
    fn revoke_subtree(&mut self, target: CPtr) -> Result<(), CSpaceError> {
        // Unlink the root of the subtree from its surviving parent before
        // tearing anything down, so no stale CPtr lingers in an ancestor
        // that outlives this revoke. Every other node in the subtree has
        // its parent inside the subtree, so those links vanish along with
        // the parent slots themselves and need no separate unlink; the
        // recursive version unlinked each node individually, which was
        // correct but redundant for everything below the root.
        let index = Self::check_slot(target)?;
        let root_parent = self.slots[index].as_ref().and_then(|s| s.parent);
        if let Some(parent) = root_parent {
            let parent_index = Self::check_slot(parent)?;
            if let Some(parent_slot) = self.slots[parent_index].as_mut() {
                parent_slot.children.retain(|c| *c != target);
            }
        }

        // Clear the root and every descendant. Order within the subtree
        // doesn't affect the result: all of them end up empty, and the
        // one externally-visible link (root -> surviving parent) is
        // already gone above. `take()` empties a slot and hands back its
        // children to enqueue; a slot that comes back `None` (a slot
        // already cleared, which a well-formed single-parent tree never
        // reaches twice) is simply skipped, so a node is idempotent to
        // process.
        let mut work: alloc::vec::Vec<CPtr> = alloc::vec::Vec::new();
        work.push(target);
        while let Some(cptr) = work.pop() {
            let node_index = match Self::check_slot(cptr) {
                Ok(node_index) => node_index,
                Err(_) => continue,
            };
            if let Some(slot) = self.slots[node_index].take() {
                for child in slot.children {
                    work.push(child);
                }
            }
        }
        Ok(())
    }
}
