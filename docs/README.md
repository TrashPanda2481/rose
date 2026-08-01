# Rose

From-scratch OS. Not Unix-derived, not NT-derived. Capability-based microkernel, Rust, QEMU-first.

## Status

Kernel boots and drops to ring 3. GDT/IDT, kernel-owned page tables, a kernel heap, a PIC/PIT timer, a round-robin scheduler, and per-task address-space isolation are all in. Capabilities are in too: CSpace (copy/mint/move/revoke with cascading revocation), CSpace syscalls reachable from ring 3 via `int 0x80`, and Untyped/Retype (Frame and CSpace object types so far). A VirtualBox run on real hardware caught a real scheduler/AddressSpace bug (task 0's own bookkeeping never learned about a manual CR3 switch, so a real timer preemption reverted it mid ring-3-program); fixed, and the full stack now passes clean end to end on both QEMU (BIOS+UEFI) and Oracle VM VirtualBox. As of 2026-08-01, `stable` carries all of the above. IPC, boot handoff, and a real process model are still spec, not code.

## Structure

Project is split into cores. Each core owns its own README, CHANGELOG, BUGS, and TROUBLESHOOTING file. This file only tracks project-wide state.

| Core | Purpose | Status |
|---|---|---|
| kernel | capabilities, IPC, address spaces, scheduler, boot handoff | boots, drops to ring 3; scheduler, address spaces, CSpace, and Untyped/Retype in and promoted to stable; IPC and boot handoff still spec |
| adaptive | real-time usage/intent understanding, native successor to Compass | deferred, stub only; see docs/cores/adaptive/README.md |

See `docs/VISION.md` for the end-state goal and the bespoke-vs-standard heuristic behind these decisions. See `docs/TRANSITION.md` for the phased path off Debian's kernel and onto Rose.

## Design bets

- Capabilities instead of Unix permissions. No ambient root.
- IPC is the only kernel primitive. No large syscall table.
- Userspace is a component graph, not a process tree.
- No POSIX layer until the native model is proven. POSIX, if it ever exists, is a compat server bolted on later.

## Non-goals (for now)

Drivers beyond virtio, filesystems, networking, GUI, SMP, dynamic component loading. These get their own core + docs when they start.

## Where things live

- `docs/cores/<name>/`: per-core README, CHANGELOG, BUGS, TROUBLESHOOTING
- `kernel/`: kernel source (not yet created)
- `abi/`: shared types between kernel and userspace (not yet created)

## Log format

See `CHANGELOG.md` in this folder for project-level milestones. Per-core changelogs track implementation-level changes.

## Branching

Two branches: `main` (active work, can be broken) and `stable` (only known-good boots land here). See `docs/BRANCHING.md`.

## Testing

QEMU for automated smoke tests, Oracle VM VirtualBox as a second-opinion check before stable promotion. See `docs/TESTING.md`.

## Repo visibility

Private. Not published anywhere public.
