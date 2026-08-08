// Thread.Configure, v0.1.
//
// Untyped/Retype (untyped.rs) can mint a Thread cap, but a freshly
// retyped Thread is just a placeholder registry entry: no
// AddressSpace, no CSpace, no entry point, not wired into scheduler.rs
// at all (see untyped.rs's own module doc). Configure is the syscall
// that turns one of those placeholders into a real, running
// scheduler task.
//
// Scope for this feature, and what's explicitly not here yet:
//   - No suspend/resume state. A Configure-created task is immediately
//     live and schedulable the moment this returns; there is no way to
//     build a thread "paused" and start it later. Resume/Suspend are a
//     future feature, not this one's.
//   - No priority. scheduler.rs is a single flat round-robin queue per
//     its own module doc; there is no Task.priority field to set.
//     SetPriority, if it ever exists, is a future feature layered on
//     top of a scheduler that actually has priority classes.
//   - Configure only ever runs once per Thread object. There is no
//     Reconfigure; a second Configure against the same Thread cap
//     fails closed (AlreadyConfigured), same one-shot spirit as an
//     Untyped-derived Frame or CSpace cap being consumed exactly once.

use crate::cspace::CSpace;
use crate::paging::AddressSpace;
use crate::scheduler;
use crate::untyped::{self, ClaimThreadError};
use alloc::boxed::Box;

/// Reasons Configure can fail. Own small code space, same pattern as
/// UntypedError/ClaimThreadError; see syscall.rs for how `code()` gets
/// offset and encoded into a single return register.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfigureError {
    /// `thread_id` didn't resolve to a live THREAD_OBJECTS entry.
    InvalidThread,
    /// The Thread at `thread_id` already has a task_index; Configure
    /// only ever runs once per Thread object, see module doc.
    AlreadyConfigured,
    /// The AddressSpace cptr didn't resolve to a usable AddressSpace
    /// cap (wrong object type, missing MAP right).
    InvalidAddressSpace,
    /// The CSpace cptr didn't resolve to a usable CSpace cap (wrong
    /// object type, missing GRANT right, or the CSPACE_OBJECTS entry
    /// it pointed at was already taken by an earlier Configure).
    InvalidCSpace,
}

impl ConfigureError {
    /// Stable small integer per variant, same convention as
    /// UntypedError::code/ClaimThreadError. Kept next to the enum so
    /// the two never drift apart; syscall.rs applies its own offset on
    /// top of this before encoding into the return register, the same
    /// way it does for every other error enum, so this module doesn't
    /// need to know anything about that band.
    pub fn code(&self) -> u8 {
        match self {
            ConfigureError::InvalidThread => 1,
            ConfigureError::AlreadyConfigured => 2,
            ConfigureError::InvalidAddressSpace => 3,
            ConfigureError::InvalidCSpace => 4,
        }
    }
}

impl From<ClaimThreadError> for ConfigureError {
    fn from(err: ClaimThreadError) -> ConfigureError {
        match err {
            ClaimThreadError::InvalidThread => ConfigureError::InvalidThread,
            ClaimThreadError::AlreadyConfigured => ConfigureError::AlreadyConfigured,
        }
    }
}

/// Turns the placeholder Thread at `thread_id` into a live scheduler
/// task running at `entry`/`stack_top` in `address_space`, owning
/// `cspace` (or a fresh empty one if `None`). Returns the new task's
/// scheduler index on success, same value `claim_thread` records back
/// against the Thread registry entry so later callers can look it up
/// through the original cap.
///
/// Checks-then-acts in this order: peek the Thread is unclaimed first
/// (cheap, read-only, fails fast on a bad thread_id before doing
/// anything else), spawn the new task (the actually consequential,
/// side-effecting step), then claim the Thread registry entry against
/// the task index that produced. See untyped.rs's own doc comment on
/// `peek_thread_unconfigured` for the accepted race window between the
/// peek and the claim; nothing else can call Configure concurrently
/// today so this is safe in practice, not just in theory.
pub fn configure(
    thread_id: u64,
    address_space: AddressSpace,
    cspace: Option<Box<CSpace>>,
    entry: u64,
    stack_top: u64,
) -> Result<usize, ConfigureError> {
    untyped::peek_thread_unconfigured(thread_id)?;

    let cspace = cspace.unwrap_or_else(|| Box::new(CSpace::new()));
    let task_index = scheduler::spawn_user(entry, stack_top, address_space, cspace);

    untyped::claim_thread(thread_id, task_index)?;

    Ok(task_index)
}
