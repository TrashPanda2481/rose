//! Teaching fake apps: small pieces of code that deliberately break a safe
//! pattern, so the framework can be watched catching them before it is
//! trusted. Each is behind its own feature flag and is never part of a normal
//! build.
//!
//! Rule 1 (range): a user mapping's target address must be bounds-checked
//! (4K-aligned, outside the kernel-shared half) before it reaches the page
//! mapper. In the typed boundary (see paging.rs) that check is the only
//! constructor for `UserVaddr`, and `UserAddressSpace::map_user_page` accepts
//! only a `UserVaddr`. So the violation below -- handing the mapper a raw
//! address that is a perfectly legal u64 aimed at the kernel-shared half
//! (0xffff_8000_0000_0000) -- must FAIL TO COMPILE. Building with
//! `--features fake_app_rule1` is the test: a clean build here would mean the
//! type has a hole. This is the live wound from SYS_FRAME_MAP (see
//! docs/cores/kernel/BUGS.md), now caught at compile time instead of by audit.

use crate::paging::{UserAddressSpace, UserMapFlags};

/// The deliberate break. The first argument is a raw u64 where a `UserVaddr`
/// is required. There is no way to mint a `UserVaddr` for a shared-half
/// address (its field is private and `try_user` rejects it), and a raw
/// integer does not coerce to one, so this does not compile. That is the
/// whole point: the rule is enforced by the signature, not by memory.
#[allow(dead_code)]
pub fn rule1_wrong_half(space: &UserAddressSpace, frame_phys: u64, flags: UserMapFlags) {
    // 0xffff_8000_0000_0000 is a valid 64-bit number pointed at the wrong
    // (kernel-shared) half. Expected `UserVaddr`, found integer.
    space.map_user_page(0xffff_8000_0000_0000, frame_phys, flags);
}
