// Endpoint IPC, v0.1 increment 2 (adds Call; Reply itself lives in
// reply.rs).
//
// Register-only Send/Receive/Call; no shared buffer, no cap transfer
// beyond the single Reply cap Receive can hand back (see reply.rs's
// own module doc). See docs/cores/kernel/README.md, "IPC": this
// implements a subset of that spec (label plus two data words,
// instead of the full length/cap_count/data[]/caps[] struct there);
// the smaller shape is what a self-test can exercise end to end right
// now, not a change to the eventual spec, see this feature's own
// CHANGELOG entry.
//
// Delivery mechanism: whichever side blocks first (sender or
// receiver) parks itself as a Waiter on this endpoint with its own
// message slot; the side that arrives second finds that Waiter
// directly and delivers into it (or reads out of it) in place, rather
// than routing through any separate mailbox. This handles both
// Send-first and Receive-first orderings correctly from whichever
// side's perspective actually needs the non-blocking fast path.
//
// Scope for this feature, and what's explicitly not here yet:
//   - Send always blocks if no receiver is already waiting: v0.1 has
//     no queue depth, no async Send, matching the README's "Send:
//     blocks if no receiver waiting / no queue space" for the
//     zero-queue-space case specifically. Call's own send phase
//     (below) reuses this exact same fast-path/slow-path split.
//   - No badge on the receiving side yet. The README says "badge
//     tells you which minted cap the sender used"; this increment's
//     Message has no badge field, since nothing here mints Endpoint
//     caps with distinct badges yet either. Future increment's job.
//   - FIFO within each queue (senders/receivers), first arrival served
//     first; no priority ordering (matches scheduler.rs's own v0.1
//     flat round-robin, no priority to order by yet).

use crate::mem::SpinLock;
use crate::reply::{self, ReplyObject};
use crate::scheduler;
use alloc::vec::Vec;

/// Fixed-size two-word message, this increment's subset of the
/// README's fuller `Message` struct (see module doc).
#[derive(Clone, Copy, Debug)]
pub struct Message {
    pub label: u32,
    pub data0: u64,
    pub data1: u64,
}

/// One parked side of a rendezvous: which scheduler task is blocked,
/// and the message slot the other side delivers into (or reads out
/// of) directly, in place, without a separate mailbox. `message`
/// starts `Some` for a parked sender (its own outgoing message) and
/// `None` for a parked receiver (until some later Send fills it in).
/// `reply_id` is `None` for a plain Send/Receive Waiter and `Some` for
/// one that originated from `call()`; whichever Receive claims this
/// Waiter is what turns a `Some` here into an actual Reply capability
/// (see syscall.rs's `dispatch_endpoint_receive`).
struct Waiter {
    task_index: usize,
    message: Option<Message>,
    reply_id: Option<u64>,
}

/// One sync IPC rendezvous point. `senders`/`receivers` are FIFO
/// queues of parked Waiters; at most one of the two is ever non-empty
/// at a time in practice (a Send arriving while a receiver is already
/// parked is handled by the fast path below and never queues at all),
/// but nothing here enforces that as an invariant, just as a natural
/// consequence of send/receive's own logic.
pub struct EndpointObject {
    senders: Vec<Waiter>,
    receivers: Vec<Waiter>,
}

impl EndpointObject {
    pub fn new() -> EndpointObject {
        EndpointObject {
            senders: Vec::new(),
            receivers: Vec::new(),
        }
    }
}

/// Global registry of live EndpointObjects, indexed by
/// KernelObjectId, same pattern as untyped.rs's own UNTYPED_OBJECTS/
/// CSPACE_OBJECTS/THREAD_OBJECTS registries (see abi's KernelObjectId
/// doc comment on why every object type gets its own registry/
/// namespace instead of one shared heap).
static ENDPOINT_OBJECTS: SpinLock<Vec<Option<EndpointObject>>> = SpinLock::new(Vec::new());

/// Adds `object` to the registry and returns the id to use as its
/// Capability's KernelObjectId. Called from untyped.rs's `retype`,
/// same convention as `register_cspace`/`register_thread` there.
pub fn register_endpoint(object: EndpointObject) -> u64 {
    let mut objects = ENDPOINT_OBJECTS.lock();
    objects.push(Some(object));
    (objects.len() - 1) as u64
}

/// Reasons a Send/Receive can fail. Own small code space, same
/// pattern as every other syscall-adjacent error enum in this
/// codebase; see syscall.rs for how `code()` gets offset (+40 band,
/// the next free one after MapError's +30) and encoded into a single
/// return register.
#[derive(Debug, PartialEq, Eq)]
pub enum EndpointError {
    /// The endpoint_id didn't resolve to a live ENDPOINT_OBJECTS
    /// entry. Same "look it up, `None` means garbage" shape as every
    /// other registry in this codebase (see untyped.rs's own
    /// InvalidUntyped/InvalidThread).
    InvalidEndpoint,
}

impl EndpointError {
    /// Stable small integer per variant, same convention as every
    /// other error enum's own `code()`.
    pub fn code(&self) -> u8 {
        match self {
            EndpointError::InvalidEndpoint => 1,
        }
    }
}

/// Sends `message` to the endpoint at `endpoint_id`. Non-blocking fast
/// path if a receiver is already parked: mutates that receiver's own
/// Waiter.message in place, wakes it, returns immediately. Otherwise
/// parks the caller as a new sender Waiter and blocks until some
/// future Receive claims it; always returns `Ok(())` once resumed,
/// since by construction nothing resumes this task until its message
/// has actually been delivered (see `receive`'s own fast path below).
pub fn send(endpoint_id: u64, message: Message) -> Result<(), EndpointError> {
    // Captured before ever taking ENDPOINT_OBJECTS's own lock, so this
    // never needs to hold both that lock and SCHEDULER's at once (see
    // scheduler::current_index's own doc comment); the two locks stay
    // strictly sequential in this file, never nested.
    let task_index = scheduler::current_index();

    let mut objects = ENDPOINT_OBJECTS.lock();
    let endpoint = objects
        .get_mut(endpoint_id as usize)
        .and_then(|entry| entry.as_mut())
        .ok_or(EndpointError::InvalidEndpoint)?;

    if let Some(receiver) = endpoint.receivers.first_mut() {
        // Deliver directly into the parked receiver's own Waiter,
        // left in place in its queue; the receiver removes its own
        // entry once resumed (see receive()'s own resume-path
        // comment on why it re-finds itself rather than trusting
        // anything captured before it blocked).
        receiver.message = Some(message);
        let receiver_task = receiver.task_index;
        drop(objects);
        scheduler::wake(receiver_task);
        return Ok(());
    }

    endpoint.senders.push(Waiter {
        task_index,
        message: Some(message),
        reply_id: None,
    });
    drop(objects);
    scheduler::block_current_and_switch();
    // Resumed here once some future Receive has taken this Waiter out
    // of the senders queue (see receive()'s own fast path below).
    // Nothing left to check: a parked sender's message is only ever
    // read by exactly one Receive call, and that call is what wakes
    // this task in the first place, so there's no failure mode once
    // execution actually gets back to this line.
    Ok(())
}

/// Receives one message from the endpoint at `endpoint_id`. Returns
/// the message alongside the reply_id of whichever Call sent it, if
/// it came from `call()` rather than plain `send()` (`None` in that
/// latter case, since there's nothing to reply to). Non-blocking fast
/// path if a sender is already parked: removes it from the senders
/// queue outright and returns its message, waking the sender.
/// Otherwise parks the caller as a new receiver Waiter and blocks
/// until some future Send or Call delivers into it.
pub fn receive(endpoint_id: u64) -> Result<(Message, Option<u64>), EndpointError> {
    let task_index = scheduler::current_index();

    {
        let mut objects = ENDPOINT_OBJECTS.lock();
        let endpoint = objects
            .get_mut(endpoint_id as usize)
            .and_then(|entry| entry.as_mut())
            .ok_or(EndpointError::InvalidEndpoint)?;

        if !endpoint.senders.is_empty() {
            let sender = endpoint.senders.remove(0);
            let message = sender
                .message
                .expect("rose: endpoint: parked sender had no message");
            let reply_id = sender.reply_id;
            let sender_task = sender.task_index;
            drop(objects);
            scheduler::wake(sender_task);
            return Ok((message, reply_id));
        }

        endpoint.receivers.push(Waiter {
            task_index,
            message: None,
            reply_id: None,
        });
        // Lock dropped here (end of block), before blocking: holding
        // it across block_current_and_switch would leave it locked
        // for however long this task stays parked, wedging every
        // other Send/Receive against the same endpoint (and, on a
        // single core, everything else that ever needs this lock)
        // for good, since nothing else could ever reach the unlock.
    }

    scheduler::block_current_and_switch();

    // Resumed here once some future Send or Call has found this
    // task's own Waiter in the receivers queue (by task_index, see
    // send()'s fast path above) and filled in its message (and,
    // for Call, its reply_id). Re-lock and pull the now-filled entry
    // back out by identity rather than trusting anything computed
    // before this task blocked: the Vec's own indices can have
    // shifted underneath while this was parked (any number of
    // unrelated remove(0) calls against this same queue could have
    // run in the meantime), so `task_index` -- this task's own
    // stable identity -- is the only thing safe to search by.
    let mut objects = ENDPOINT_OBJECTS.lock();
    let endpoint = objects
        .get_mut(endpoint_id as usize)
        .and_then(|entry| entry.as_mut())
        .expect("rose: endpoint: object vanished while this task was parked");
    let position = endpoint
        .receivers
        .iter()
        .position(|waiter| waiter.task_index == task_index)
        .expect("rose: endpoint: own receiver waiter vanished while parked");
    let waiter = endpoint.receivers.remove(position);
    let message = waiter
        .message
        .expect("rose: endpoint: own receiver waiter woke with no message");
    Ok((message, waiter.reply_id))
}

/// Sends `message` to the endpoint at `endpoint_id` and blocks until
/// the eventual matching Reply delivers a response, returning it.
/// Layered directly on top of `send`'s own fast-path/slow-path split
/// for the send phase (steps 3-4 below), plus a second, always-taken
/// block for the reply phase (step 5) that has no equivalent in plain
/// `send`.
///
/// Order of operations, and why:
///   1. Capture this task's own identity first, same reasoning as
///      `send`'s own doc comment (never hold this lock and
///      SCHEDULER's at once).
///   2. Validate the endpoint exists *before* minting a Reply object.
///      An InvalidEndpoint error has to leave no trace: minting the
///      reply.rs entry first and only then discovering the endpoint
///      doesn't exist would orphan that entry for good, since nothing
///      would ever reply to it or take_reply it back out.
///   3/4. Deliver the send phase exactly like `send` does: fast path
///      if a receiver is already parked (deliver in place, wake it,
///      do NOT block for this phase), slow path otherwise (park as a
///      sender Waiter, block once, resume once some Receive claims
///      it). Either way, by the time step 5 below runs, the message
///      has either already been handed to a receiver or is sitting in
///      the senders queue ready to be claimed.
///   5. Block a second time, unconditionally, waiting specifically
///      for `reply::reply` to wake this exact task. This is the one
///      new block plain `send` never takes; whether step 3/4 took the
///      fast path (zero blocks so far) or the slow path (one block so
///      far), this call always blocks exactly once more here, for
///      exactly one total block in the fast case and two total blocks
///      in the slow case.
pub fn call(endpoint_id: u64, message: Message) -> Result<Message, EndpointError> {
    let task_index = scheduler::current_index();

    let mut objects = ENDPOINT_OBJECTS.lock();
    let endpoint = objects
        .get_mut(endpoint_id as usize)
        .and_then(|entry| entry.as_mut())
        .ok_or(EndpointError::InvalidEndpoint)?;

    // Endpoint confirmed live; only now is it safe to mint the Reply
    // object nothing has claimed yet (see step 2 above).
    let reply_id = reply::register_reply(ReplyObject {
        task_index,
        message: None,
    });

    if let Some(receiver) = endpoint.receivers.first_mut() {
        receiver.message = Some(message);
        receiver.reply_id = Some(reply_id);
        let receiver_task = receiver.task_index;
        drop(objects);
        scheduler::wake(receiver_task);
        // Fast path taken: no block yet for the send phase, matching
        // send()'s own fast path exactly. Falls through to the
        // always-taken reply-phase block below.
    } else {
        endpoint.senders.push(Waiter {
            task_index,
            message: Some(message),
            reply_id: Some(reply_id),
        });
        drop(objects);
        scheduler::block_current_and_switch();
        // Resumed here once some future Receive has taken this
        // Waiter out of the senders queue, exactly like send()'s own
        // slow path. Falls through to the always-taken reply-phase
        // block below -- this is the second block for this call, not
        // a repeat of the first.
    }

    // Reply phase: block unconditionally, regardless of which branch
    // above ran, waiting for reply::reply(reply_id, ...) to wake this
    // task specifically (see reply.rs).
    scheduler::block_current_and_switch();

    let response = reply::take_reply(reply_id)
        .expect("rose: endpoint: call resumed with no reply message");
    Ok(response)
}
