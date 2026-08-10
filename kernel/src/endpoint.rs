// Endpoint IPC, v0.1 increment 1.
//
// Register-only, blocking Send/Receive only; no Call yet, no shared
// buffer, no cap transfer. See docs/cores/kernel/README.md, "IPC":
// this implements a subset of that spec (label plus two data words,
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
//     zero-queue-space case specifically.
//   - No Call (Send + implicit Receive on a Reply cap). Two separate
//     verbs only; Call is layered on top of these once Reply objects
//     exist.
//   - No badge on the receiving side yet. The README says "badge
//     tells you which minted cap the sender used"; this increment's
//     Message has no badge field, since nothing here mints Endpoint
//     caps with distinct badges yet either. Future increment's job.
//   - FIFO within each queue (senders/receivers), first arrival served
//     first; no priority ordering (matches scheduler.rs's own v0.1
//     flat round-robin, no priority to order by yet).

use crate::mem::SpinLock;
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
struct Waiter {
    task_index: usize,
    message: Option<Message>,
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

/// Receives one message from the endpoint at `endpoint_id`.
/// Non-blocking fast path if a sender is already parked: removes it
/// from the senders queue outright and returns its message, waking
/// the sender. Otherwise parks the caller as a new receiver Waiter
/// and blocks until some future Send delivers into it.
pub fn receive(endpoint_id: u64) -> Result<Message, EndpointError> {
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
            let sender_task = sender.task_index;
            drop(objects);
            scheduler::wake(sender_task);
            return Ok(message);
        }

        endpoint.receivers.push(Waiter {
            task_index,
            message: None,
        });
        // Lock dropped here (end of block), before blocking: holding
        // it across block_current_and_switch would leave it locked
        // for however long this task stays parked, wedging every
        // other Send/Receive against the same endpoint (and, on a
        // single core, everything else that ever needs this lock)
        // for good, since nothing else could ever reach the unlock.
    }

    scheduler::block_current_and_switch();

    // Resumed here once some future Send has found this task's own
    // Waiter in the receivers queue (by task_index, see send()'s
    // fast path above) and filled in its message. Re-lock and pull
    // the now-filled entry back out by identity rather than trusting
    // anything computed before this task blocked: the Vec's own
    // indices can have shifted underneath while this was parked (any
    // number of unrelated remove(0) calls against this same queue
    // could have run in the meantime), so `task_index` -- this task's
    // own stable identity -- is the only thing safe to search by.
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
    Ok(message)
}
