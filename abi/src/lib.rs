// Shared between the kernel and (eventually) userspace. no_std, no
// deps, on purpose: this crate only exists so object types, rights,
// and cap layout are never hand-duplicated on both sides of the
// syscall boundary. See docs/cores/kernel/README.md, "Naming/versioning".
#![no_std]

/// Kernel object kinds a Capability can point at. Matches the "Object
/// types, v0.1" table in the kernel README exactly, plus CSpace, which
/// the table was missing even though "Boot handoff" already grants the
/// root task a cap to its own CSpace. Added here rather than worked
/// around silently; see kernel CHANGELOG for this feature.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ObjectType {
    Untyped,
    Frame,
    PageTable,
    AddressSpace,
    Thread,
    Endpoint,
    Notification,
    Reply,
    IrqHandler,
    CSpace,
}

impl ObjectType {
    /// Raw discriminant, for a syscall ABI (Retype, see
    /// kernel/src/untyped.rs) to pass this across the ring 3/ring 0
    /// boundary as a plain register value, same convention as
    /// `Rights::bits()`/`from_bits`. Written as an explicit match
    /// rather than `as u8`: this is the first time a variant's
    /// numeric value crosses a real ABI boundary, so the mapping is
    /// pinned here on purpose instead of quietly riding on whatever
    /// order the variants happen to be declared in above.
    pub const fn to_u8(self) -> u8 {
        match self {
            ObjectType::Untyped => 0,
            ObjectType::Frame => 1,
            ObjectType::PageTable => 2,
            ObjectType::AddressSpace => 3,
            ObjectType::Thread => 4,
            ObjectType::Endpoint => 5,
            ObjectType::Notification => 6,
            ObjectType::Reply => 7,
            ObjectType::IrqHandler => 8,
            ObjectType::CSpace => 9,
        }
    }

    /// Inverse of `to_u8`. `None` for any value with no matching
    /// variant; a syscall handler decoding a register value has no
    /// other way to reject garbage than this.
    pub const fn from_u8(value: u8) -> Option<ObjectType> {
        match value {
            0 => Some(ObjectType::Untyped),
            1 => Some(ObjectType::Frame),
            2 => Some(ObjectType::PageTable),
            3 => Some(ObjectType::AddressSpace),
            4 => Some(ObjectType::Thread),
            5 => Some(ObjectType::Endpoint),
            6 => Some(ObjectType::Notification),
            7 => Some(ObjectType::Reply),
            8 => Some(ObjectType::IrqHandler),
            9 => Some(ObjectType::CSpace),
            _ => None,
        }
    }
}

/// Bitfield, not an enum: a cap can hold any combination. Plain u8
/// wrapper instead of pulling in a bitflags dependency, this crate
/// stays zero-dep on purpose.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rights(u8);

impl Rights {
    pub const NONE: Rights = Rights(0);
    pub const READ: Rights = Rights(1 << 0);
    pub const WRITE: Rights = Rights(1 << 1);
    pub const GRANT: Rights = Rights(1 << 2);
    pub const MAP: Rights = Rights(1 << 3);
    pub const SEND: Rights = Rights(1 << 4);
    pub const RECEIVE: Rights = Rights(1 << 5);

    pub const fn union(self, other: Rights) -> Rights {
        Rights(self.0 | other.0)
    }

    pub const fn contains(self, other: Rights) -> bool {
        self.0 & other.0 == other.0
    }

    /// True if `self` grants nothing `superset` doesn't already grant.
    /// Used by `copy`/`mint` to enforce "equal or lower rights" per
    /// the Deriving capabilities rule; a derived cap can never gain a
    /// bit its source didn't have.
    pub const fn is_subset_of(self, superset: Rights) -> bool {
        self.0 & !superset.0 == 0
    }

    /// Raw bitfield value, for a syscall ABI to pass across the ring
    /// 3/ring 0 boundary as a plain register value; there's no other
    /// way for userspace to name a rights set. See `from_bits`.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Builds a Rights from a raw register value. No validation: every
    /// bit above the five defined ones is simply meaningless and
    /// ignored by every check here, same as an unused CPU flag bit.
    pub const fn from_bits(bits: u8) -> Rights {
        Rights(bits)
    }
}

/// Meaning is per-ObjectType, not a single global namespace: an
/// AddressSpace cap's id is that AddressSpace's PML4 physical address
/// (already unique by construction). A Thread cap's id is a registry
/// index into the kernel's own THREAD_OBJECTS table (kernel/src/
/// untyped.rs), not a scheduler task index directly; Configure
/// (kernel/src/thread.rs) separately records which task index that
/// registry entry maps to once configured, kept as a second lookup
/// rather than folded into this id, so the cap's identity never has to
/// change even though what it resolves to gains a second meaning partway
/// through the object's life. No shared generic kernel-object heap yet,
/// that's Untyped/Retype's job, a later feature; this just reuses
/// whatever stable identity the referenced object already has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KernelObjectId(pub u64);

/// Local, per-CSpace slot index. 0 is reserved as the null cap and is
/// never a valid occupied slot; matches the common convention (seL4
/// does the same) rather than a Rose-specific choice.
pub type CPtr = u32;

/// Unforgeable kernel-mediated reference to an object, plus rights.
/// Userspace never sees `object_ref` directly and can't forge one;
/// this type only exists inside the kernel and inside messages the
/// kernel itself constructs. See docs/cores/kernel/README.md,
/// "Capability".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capability {
    pub object_ref: KernelObjectId,
    pub object_type: ObjectType,
    pub rights: Rights,
    pub badge: u64,
}
