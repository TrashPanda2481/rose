// Reply objects, Endpoint IPC v0.1 increment 2 (Call/Reply).
//
// A Reply is the kernel-generated, one-shot object a Call's matching
// Receive gets handed a capability to, and that a later Reply syscall
// consumes to deliver the response and wake the original caller.
// Unlike every other object type in this codebase, a Reply is never
// created via Untyped/Retype (see untyped.rs's module doc, unchanged
// by this feature): it always comes into existence as a side effect
// of endpoint.rs's own `call()`, before that function's first block,
// so a Receive that arrives before Call's send phase even delivers
// still has a fully-formed reply_id to attach to the message.
//
// Registry shape mirrors endpoint.rs's own ENDPOINT_OBJECTS: a plain
// Vec<Option<T>>, indexed by registry id. Unlike ENDPOINT_OBJECTS's
// own senders/receivers queues (which shift under remove(0)),
// nothing here ever removes an element from the middle -- `take_reply`
// only ever clears the exact slot the caller already owns by
// identity (its own reply_id, captured before it ever blocked), so
// there's no equivalent to endpoint::receive()'s own re-find-by-
// task_index dance.

use crate::endpoint::Message;
use crate::mem::SpinLock;
use crate::scheduler;
use alloc::vec::Vec;

/// One outstanding Call waiting on its own eventual Reply: which
/// scheduler task is blocked, and the message slot `reply()` writes
/// into. `message` starts `None` and is filled in exactly once, by
/// `reply()`.
pub struct ReplyObject {
    pub task_index: usize,
    pub message: Option<Message>,
}

/// Global registry of live ReplyObjects, indexed by a plain registry
/// id. Not a KernelObjectId/Capability the way Endpoint/Frame/etc.
/// are minted (see module doc: no Retype involved); syscall.rs's
/// `dispatch_endpoint_receive` is what wraps a registry id from here
/// into a Capability{object_type: Reply, ...} and installs it
/// directly into the receiving task's own CSpace.
static REPLY_OBJECTS: SpinLock<Vec<Option<ReplyObject>>> = SpinLock::new(Vec::new());

/// Adds `object` to the registry and returns its id. Called from
/// endpoint.rs's `call()`, before that function's own send phase.
pub fn register_reply(object: ReplyObject) -> u64 {
    let mut objects = REPLY_OBJECTS.lock();
    objects.push(Some(object));
    (objects.len() - 1) as u64
}

/// Reasons a Reply can fail. Own small code space, same pattern as
/// every other syscall-adjacent error enum in this codebase; see
/// syscall.rs for how `code()` gets offset (+50 band, the next free
/// one after EndpointError's own +40).
#[derive(Debug, PartialEq, Eq)]
pub enum ReplyError {
    /// The reply_id didn't resolve to a live REPLY_OBJECTS entry:
    /// either garbage, or (the expected steady-state case) already
    /// consumed by an earlier Reply against the same one-shot slot,
    /// since `take_reply` clears the entry outright rather than
    /// leaving a stale message behind.
    InvalidReply,
}

impl ReplyError {
    /// Stable small integer per variant, same convention as every
    /// other error enum's own `code()`.
    pub fn code(&self) -> u8 {
        match self {
            ReplyError::InvalidReply => 1,
        }
    }
}

/// Writes `message` into the ReplyObject at `reply_id` and wakes its
/// waiting task. Does NOT clear the registry slot itself: the woken
/// Call's own resume path (`take_reply`, below) is what clears it,
/// one-shot enforced structurally rather than by this function
/// racing to tear the entry down before the wake it just issued has
/// even had a chance to matter.
pub fn reply(reply_id: u64, message: Message) -> Result<(), ReplyError> {
    let mut objects = REPLY_OBJECTS.lock();
    let entry = objects
        .get_mut(reply_id as usize)
        .and_then(|entry| entry.as_mut())
        .ok_or(ReplyError::InvalidReply)?;

    entry.message = Some(message);
    let task_index = entry.task_index;
    drop(objects);
    scheduler::wake(task_index);
    Ok(())
}

/// Removes and returns the message from the ReplyObject at
/// `reply_id`, clearing the registry slot for good. Called from
/// `call()`'s own resume path, after its second
/// `scheduler::block_current_and_switch()` call returns. `None` means
/// either a garbage id or (structurally impossible in the current
/// caller, but handled rather than assumed away) a slot that was
/// already taken.
pub fn take_reply(reply_id: u64) -> Option<Message> {
    let mut objects = REPLY_OBJECTS.lock();
    let entry = objects.get_mut(reply_id as usize)?;
    let object = entry.take()?;
    object.message
}
