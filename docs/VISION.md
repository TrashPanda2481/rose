# Vision

Not a spec. A statement of end state, written down so it doesn't drift or get lost under kernel plumbing.

## End state

Rose isn't just a capability microkernel. That's the foundation, not the goal. The goal is a system that:

- Is built bespoke where it matters, and conforms to standards where it doesn't. See heuristic below.
- Has zero commercial cruft — no telemetry, no ads, no forced accounts, no bloat carried for a business model Rose doesn't have.
- Understands, in real time, how it's actually being used, and adapts to the end user instead of forcing the end user to adapt to it.

That last point is a first-class OS capability, not an app bolted on top. It's the Compass idea, but native — instead of a userspace guidance tool sitting on top of a generic OS, the understanding/adaptation layer is part of what Rose is.

## Heuristic: bespoke vs. standard

Build original where the component defines Rose's identity or exposes architectural surface area. Conform to the standard where the component is dictated by hardware or protocol and has no such surface.

Examples already decided:
- Capability model, IPC, component graph: bespoke. This is what makes Rose not-Unix and not-NT.
- Boot chain (Limine): standard. BIOS/UEFI bring-up has no architectural surface — it's compliance, not design. See `docs/cores/kernel/README.md` and `TROUBLESHOOTING.md`.

Apply the same test to everything that comes later: drivers, storage, networking, GUI. Default to standard-conformant unless there's a specific reason Rose needs to do it differently.

## Sequencing

The adaptation layer needs something to observe and something to act through. It can't exist before:
1. Capabilities and IPC (ground-zero steps 3-4)
2. A scheduler and real components to run
3. Enough of a component graph that "usage" means something

Not scoped further than that yet. See `docs/cores/adaptive/README.md` for the stub.
