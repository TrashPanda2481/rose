# Vision

Not a spec. A statement of end state, written down so it does not drift or get lost under kernel plumbing. The detailed design of the intelligent layer lives in `docs/cores/adaptive/README.md`; this file is the why.

## End state

Rose is not just a capability microkernel. That is the foundation, not the goal. The goal is a system that:

- Is built bespoke where it matters and conforms to standards where it does not. See the heuristic below.
- Carries zero commercial cruft: no telemetry, no ads, no forced accounts, no bloat kept alive for a business model Rose does not have.
- Understands, in real time, how it is actually being used, and adapts to the person using it instead of forcing the person to adapt to it.

That last point is what makes Rose more than a clean microkernel, and it is the part most easily misunderstood, so most of this document is about getting it right.

## The intelligence is not a layer, and it is not in the kernel

The instinct is to picture "the smart part" as a box sitting on top, or worse, baked into the kernel so the system can "decide" things. Both are wrong, and the distinction is the whole design.

The kernel (Rose itself) is reflex, not intelligence. It is the arbiter of authority: it enforces capabilities, enforces isolation, and owns the allocation of CPU and memory. It does not reason about what should happen; it enforces what is permitted to happen. A component proposes, the kernel authorizes, the person grants. Those three roles never collapse into one. Putting a reasoner in the kernel would swell the trusted core, break the footprint guarantee below, and quietly turn Rose back into the kind of system it exists to replace.

The intelligence is a faculty, not a place. It does four things: it maps inputs into a coherent picture, it passes useful patterns down into faster layers, it surfaces what is needed in the moment up to where a decision gets made, and it stores what may be needed later. It is deep in the system in the sense that it is pervasive and mostly runs below conscious attention, the way most of a brain's work never reaches awareness. It is not deep in the sense of privilege. It holds only the authority it was granted, inside a hard footprint bound it cannot raise.

## How the layers sit

Borrowing the shape of a nervous system, because it turns out to fit:

- Sensing (Stem): an afferent layer that watches hardware, software, program usage, and activity. It judges nothing and reports everything. It is not a control layer wedged between the kernel and the rest; it is the sensory wiring that feeds every layer holding a capability to listen.
- Reflex (Rose, the kernel): the autonomous layer. Non-optional, fast, consulted by nothing at its floor. It keeps the hardware safe and the components isolated. Part of it is an untrainable floor that can never be modified; the rest can be influenced within bounds.
- Modulation (the resource policy): a semi-autonomous layer that can influence the reflex layer but never seize it. It biases allocation within ranges the kernel still owns and can override. It runs on its own, inside bounds granted once.
- Intent (Compass and the person): the manual, deliberate layer. This is where a decision is negotiated and consent is given. Compass is the digestible interface to the person, the part of the intelligence that surfaces into view. It is the tip, not the whole.

Two rules hold this together:

- Intent flows down, veto flows up. Influence and intent propagate from the manual layer toward the reflex layer; safety authority propagates the other way. The untrainable floor overrides any intent, any policy, and any learned behavior. Nothing above it can talk it out of protecting the system, the same way a person cannot will their heart to stop.
- The watcher cannot become what it watches. Whatever runs the sensing and the intelligence lives inside a bounded footprint the kernel enforces. The whole point is catching things that waste the machine; it would be a joke if the catcher became the thing worth catching. So the bound is structural, not a polite budget.

## How it learns

The only training signal is what the person tells it when it asks: yes, no, or do it differently. Nothing external, nothing phoned out, nothing aggregated with strangers. If it gets better at what to suggest, that is because the person taught it, not because a server somewhere pooled everyone's usage.

Learning, concretely, is a behavior descending the layer stack as trust accrues. A response starts as something Compass asks about every time (manual). Approved enough, and told to remember, it drops to running on its own within bounds (modulation). In the rare, tightly bounded cases where it has earned it, it consolidates into a trained reflex. This is the same path a person takes turning a deliberate skill into muscle memory, and it is gated the same way: slowly, under supervision, and never into the untrainable floor.

## It is optional, and yet it is part of what Rose is

Both are true, and they stop contradicting each other once the substrate is separated from the tenant.

The substrate is first-class and always present, even in a build with no intelligence at all: the way the kernel exposes safe observation, the single path by which a human yes becomes authority, the footprint bound, and the gate through which a reflex can ever be trained. That machinery is part of what Rose is.

The tenant is the intelligence itself, Stem and Compass. That is optional. A build of Rose with neither is still a complete, working operating system. The intelligence is a layer you can leave out, not a tax you have to pay to run the thing at all.

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
