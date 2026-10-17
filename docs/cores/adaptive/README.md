# Adaptive core

Status: spec, not built. The kernel has reached the scheduler + IPC round-trip this core was waiting on (see `docs/cores/kernel/README.md`), so this is the first real design, replacing the earlier placeholder. No code yet; building waits on the Phase 1 component model in `docs/TRANSITION.md`.

## What this is

The intelligent layer described in `docs/VISION.md`: the native successor to Compass. Not an app on top of a generic OS, but an OS-level faculty that observes the system, learns only from the person, and acts only through authority the person granted. This document is the design; VISION is the why.

## The shape

Intelligence here is a faculty, not a single component and not a privileged layer. It performs four operations across the system:

- map: build a coherent picture from raw signal,
- consolidate: push proven patterns down into faster layers,
- surface: raise what is needed in the moment up to where a decision is made,
- store: retain what may be needed later.

It is pervasive in function and bounded in privilege. Two named pieces carry it:

- Stem: observe-only sensing. Keeps baselines, notices deviation, reports it, judges nothing. It needs no model of what anything means; "different than usual" is its whole job, which is exactly why it is safe to let it watch broadly. Stem is afferent: a shared sensor bus that feeds every layer holding a scoped observe capability, not a control stage between the kernel and the rest.
- Compass: the reasoner and the interface. It decides whether a flagged change matters, forms a guess at cause, and if action is warranted, proposes it to the person in plain terms: what it noticed, what it thinks is going on, what it wants to do, what to expect, then it waits for a yes. Saying yes is the only way it gets authority to act, and it does not keep that authority afterward unless told to remember the decision.

## The layers it plugs into

Full picture in VISION. The seams each layer exposes:

- Reflex (kernel): an untrainable floor plus a modulated-autonomic tier. Exposes a bounded reflex-registration gate (below).
- Modulation (resource policy): influences allocation within bounds; the fast loop acts on its own inside limits granted once, and hard-safety responses must be autonomous because asking is too slow.
- Manual (Compass + person): consent-gated, deliberate.
- Sensing (Stem): feeds all of the above.

Invariant, repeated here because the floor below depends on it: intent flows down, veto flows up. The untrainable floor overrides any intent, policy, or learned behavior.

## The untrainable floor

This is the decision the rest of the design hangs on, and the first thing to ratify before any code. The floor is the set of kernel behaviors that are pure reflex: consulted by nothing, modifiable by no one, never consolidatable into from above. It is simultaneously the system's safety guarantee and the hard ceiling on what the intelligence may ever install. Everything not on this list is, in principle, influenceable or trainable within bounds.

On the floor (inviolable, never trainable):

1. Capability and rights enforcement. Every invocation is checked against the capabilities actually held. No learned shortcut and no fast path ever skips the check.
2. Address-space isolation and W^X. No component touches memory it holds no capability for. Not negotiable, not biasable.
3. CPU exception and fault handling. A fault is contained by the kernel immediately (fault the offending component, not the system). Policy never gets to defer or suppress a fault.
4. The authority-granting path. Only the consent broker, blessed by the root task, mints capabilities. The intelligence can never grant itself authority, and nothing can train it to.
5. Revocation. A revoke (a cut in the derivation tree) always completes. The intelligence cannot block, slow, or train away revocation of its own authority.
6. The footprint bound on the intelligence itself. The cap on what Stem, Compass, and the learner may consume is enforced by the kernel and cannot be raised by the thing it bounds.
7. Hardware-safety hard cutoffs (design intent, not yet implemented). When Rose grows real hardware-safety sensing, its emergency responses (for example an overtemp halt) sit on the floor: they fire autonomously and cannot be trained away or suppressed by policy.

Above the floor (influenceable, consolidatable under consent): allocation strategy (priority bias, which non-essential work to suspend, prefetch and cache hints, scheduling bias within safe ranges), salience and surfacing choices, which suggestions to make and when, and profile or preference adaptation.

Items 1 through 6 map onto mechanisms the kernel already has (cap/rights checks, paging W^X, the IDT fault path, the mint path, the CSpace derivation tree, and a bounded Untyped). Item 7 is forward-looking and named so it is not forgotten.

## How it learns

Learning is a behavior descending the layer stack as trust accrues. The person's three consent modes are the waypoints on one consolidation gradient:

- handle once: stays manual, Compass asks every time,
- remember and do it automatically: drops to modulation, runs on its own within bounds,
- (earned, rare, tightly bounded): consolidates into a trained reflex through the registration gate.

Grounding ("how it knows what is what"), in order of how much to trust the source:
1. the capability and IPC graph as semantic substrate: a component's purpose is legible from the capabilities it holds and the endpoints it talks to, which is hard to fake and is a Rose-native advantage a Unix or NT system cannot offer,
2. a component's self-declared manifest, treated as a claim checked against the graph, never trusted raw (the kernel's own verify-before-commit principle applied to identity),
3. labels the person gives, where every consent dialogue is one labeled example.

The learner must be small, local, and interpretable: rules and thresholds, a tiny local model, and a bounded case memory, never a frontier model. The footprint ceiling is enforced structurally through bounded capabilities, so "the watcher became the thing worth watching" is impossible by construction rather than policed. A fresh install knows nothing about the person and must be humble early, asking more and assuming less, earning autonomy over time. That is the anti-telemetry stance turned into a property: the intelligence is the person's own, non-transferable, never pre-loaded from aggregated strangers.

## How it acts

Authority enters through a single chokepoint, the consent broker: a human yes is the only thing that mints Compass a capability, and the three modes map to an ephemeral capability (handle once), a stored automation rule that re-mints on the same trigger (remember), or a suppression entry (never ask). Autonomy is therefore a delegated, revocable capability, not a trait: granted one situation at a time, taken back with one cut of the derivation tree. Badges (not yet built, IPC increment 3) are how minted capabilities would carry provenance.

## The reflex-training gate

The kernel never learns by changing its own code. It exposes a bounded, verified, capability-gated registration facility for fast-path rules (condition then pre-authorized action). The verifier enforces the untrainable floor as the hard limit: a registered reflex can act quickly and without consultation, but it can never violate items 1 through 7. The shape of the prior art is eBPF (userspace installs small verified programs that run in the kernel fast path under a verifier's limits); Rose's version is the same idea, capability-scoped and bounded.

## Scope: v1 is system health only

Compass has two jobs, and they are not equally tractable. System health (is the machine ok) grounds cleanly in signals, baselines, and the capability graph. "Understand what the person is working on" (intent and workflow) means modeling the person from impoverished, indirect signals, which is a genuinely open problem. v1 is system-health only; intent inference is a later, opt-in frontier, not a v1 promise.

## What it needs from the rest of Rose first

- The Phase 1 component model (`docs/TRANSITION.md`): nothing to observe or act through until components exist at runtime.
- An observe capability or right in the kernel's `Rights` set, decided when the root task first hands out capabilities, since the root task is the only authority that could ever grant system-wide observation without system-wide power.
- The consent broker as the single mint chokepoint.
- A bounded Untyped for the intelligence's own footprint.
- Later, the reflex-registration gate.

## Open decisions

- Ratify the untrainable floor above.
- Grounding mix: primarily structural (capability graph) versus primarily declared or taught (manifests plus labels).
- Learner style: interpretable (rules plus case memory) versus adaptive (a small opaque model). The vision leans interpretable.
- Whether routine resource allocation acts autonomously within granted bounds (current lean: yes) or asks each time.
- Where allocation policy lives: its own resource-manager component, or inside Compass.
