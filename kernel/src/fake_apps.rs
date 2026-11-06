//! Teaching fake apps: small pieces of code that deliberately break a safe
//! pattern, so the framework can be watched catching them before it is
//! trusted. Each is behind its own feature flag and is never part of a normal
//! build. Each tests exactly one door in the typed user-mapping boundary
//! (paging.rs), so a regression in one door is not masked by the other.
//!
//! Rule 1 (range) is enforced by two independent things, so there are two
//! fake apps:
//!
//! * `fake_app_rule1` -- the SIGNATURE door. `map_user_page` takes a
//!   `UserVaddr`, so handing it a raw u64 aimed at the kernel-shared half must
//!   fail to compile (E0308, expected `UserVaddr`, found integer). This is the
//!   live wound from SYS_FRAME_MAP (docs/cores/kernel/BUGS.md), now caught at
//!   compile time instead of by audit.
//!
//! * `fake_app_rule1_forge` -- the PRIVATE-FIELD door, the load-bearing brick.
//!   `UserVaddr`'s only constructor is the checked `try_user`, and its field is
//!   private to `paging`. Forging the type by hand from here (a sibling
//!   module) must fail to compile (E0423, constructor not visible, private
//!   field). If this one ever compiles, field privacy has been lost -- a
//!   one-line `pub` away -- and the whole "one locked door" argument is gone.
//!
//! Each test is "run" by building with its feature on and confirming the build
//! FAILS. A clean build with either feature means that door has opened.

use crate::paging::{UserAddressSpace, UserMapFlags};

/// Signature door. The first argument is a raw u64 where a `UserVaddr` is
/// required; a raw integer does not coerce to one, so this does not compile.
#[cfg(feature = "fake_app_rule1")]
#[allow(dead_code)]
pub fn rule1_wrong_half(space: &UserAddressSpace, frame_phys: u64, flags: UserMapFlags) {
    // 0xffff_8000_0000_0000 is a valid 64-bit number pointed at the wrong
    // (kernel-shared) half. Expected `UserVaddr`, found integer.
    space.map_user_page(0xffff_8000_0000_0000, frame_phys, flags);
}

/// Private-field door. Try to forge the type directly instead of going through
/// `try_user` -- the "key" rustc's own E0308 help suggests. From a sibling
/// module the tuple constructor is not visible (the field is private to
/// `paging`), so this must fail with E0423. The day it compiles is the day the
/// field was made `pub` or a crate-visible constructor was added.
#[cfg(feature = "fake_app_rule1_forge")]
#[allow(dead_code)]
pub fn rule1_forge_vaddr(space: &UserAddressSpace, frame_phys: u64, flags: UserMapFlags) {
    // Same legal-but-wrong-half number, now smuggled in by hand-constructing
    // the typed value. The private field refuses it.
    let forged = crate::paging::UserVaddr(0xffff_8000_0000_0000);
    space.map_user_page(forged, frame_phys, flags);
}
