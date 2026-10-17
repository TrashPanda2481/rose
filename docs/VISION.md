# Vision

Not a spec. A statement of end state, written down so it does not drift or get lost under kernel plumbing. The detailed design of the intelligent layer lives in `docs/cores/adaptive/README.md`; this file is the why.

## End state

Rose is not just a capability microkernel. That is the foundation, not the goal. The goal is a system that:

- Is built bespoke where it matters and conforms to standards where it does not. See the heuristic below.
- Carries zero commercial cruft: no telemetry, no ads, no forced accounts, no bloat kept alive for a business model Rose does not have.
- Understands, in real time, how it is actually being used, and adapts to the person using it instead of forcing the person to adapt to it.

That last point is what makes Rose more than a clean microkernel, and most of this document is about it.

## The kernel is reflex, the intelligence is a faculty

The kernel, Rose itself, is reflex. It is the arbiter of authority: it enforces capabilities, enforces isolation, and owns the allocation of CPU and memory. It settles what is permitted to happen and leaves what should happen to the layers above it. A component proposes, the kernel authorizes, the person grants, and those three roles stay separate. A reasoner living in the kernel would swell the trusted core and break the footprint guarantee below, so the kernel stays reflexive and the thinking lives elsewhere.

The intelligence is a faculty that runs across the whole system. It maps inputs into a coherent picture, passes useful patterns down into faster layers, surfaces what is needed in the moment up to where a decision gets made, and stores what may be needed later. It is pervasive, and most of it runs below conscious attention, the way most of a brain's work never reaches awareness. Its reach is wide and its authority is narrow: it holds only what it was granted, inside a hard footprint bound it cannot raise.

## How the layers sit

The shape of a nervous system fits this well:

- Sensing (Stem): an afferent layer that watches hardware, software, program usage, and activity. It keeps baselines, reports what it sees, and judges nothing. It is the sensory wiring of the system, feeding every layer that holds a capability to listen.
- Reflex (Rose, the kernel): the autonomous layer. Fast, non-optional, and consulted by nothing at its floor. It keeps the hardware safe and the components isolated. An untrainable floor can never be modified; the tier above it can be influenced within bounds.
- Modulation (the resource policy): a semi-autonomous layer that influences the reflex layer within bounds. It biases allocation inside ranges the kernel owns and can override, running on its own inside limits granted once.
- Intent (Compass and the person): the manual, deliberate layer, where a decision is negotiated and consent is given. Compass is the interface to the person and the visible surface of the intelligence.

Two rules hold this together:

- Intent flows down, veto flows up. Influence and intent propagate from the manual layer toward the reflex layer; safety authority propagates the other way. The untrainable floor overrides any intent, any policy, and any learned behavior, the same way a person cannot will their heart to stop.
- The watcher stays inside a bounded footprint the kernel enforces. The whole point is catching things that waste the machine; a catcher that became the thing worth catching would be a joke. The bound is structural: the kernel enforces it, and nothing above can raise it.

## How it learns

The only training signal is what the person tells it when it asks: yes, no, or do it differently. Nothing is phoned out or aggregated with strangers. It gets better at what to suggest because the person taught it.

Learning is a behavior descending the layer stack as trust accrues. A response starts as something Compass asks about every time (manual). Approved enough, and told to remember, it drops to running on its own within bounds (modulation). In the rare, tightly bounded cases where it has earned it, it consolidates into a trained reflex. This is the path a person takes turning a deliberate skill into muscle memory, gated the same way: slowly, under supervision, and never into the untrainable floor.

## Substrate and tenant

Two parts carry different obligations.

The substrate is always present, even in a build with no intelligence at all: the way the kernel exposes safe observation, the single path by which a human yes becomes authority, the footprint bound, and the gate through which a reflex can be trained. This machinery is part of what Rose is.

The tenant is the intelligence itself, Stem and Compass. A build of Rose without them is still a complete, working operating system. The intelligence is a layer you can leave out.

## Heuristic: bespoke vs. standard

Build original where the component defines Rose's identity or exposes architectural surface area. Conform to the standard where the component is dictated by hardware or protocol and has no such surface.

Examples already decided:
- Capability model, IPC, component graph, and the intelligence substrate above them: bespoke. This is what makes Rose not-Unix and not-NT.
- Boot chain (Limine): standard. BIOS/UEFI bring-up has no architectural surface; it is compliance, not design. See `docs/cores/kernel/README.md` and `TROUBLESHOOTING.md`.

Apply the same test to everything later: drivers, storage, networking, GUI. Default to standard-conformant unless there is a specific reason Rose needs to do it differently.

## Sequencing

The intelligence needs something to observe and something to act through. It cannot exist before:
1. Capabilities and IPC (done).
2. A scheduler and real components to run (kernel mechanisms done; the real component model is Phase 1 in `docs/TRANSITION.md`, not started).
3. Enough of a component graph that "usage" means something.

The kernel has reached the scheduler and IPC round-trip the adaptive core was waiting on, so the design can now be written down for real. The spec lives in `docs/cores/adaptive/README.md`. Building it still waits on the component model.
