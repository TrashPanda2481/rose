# Rose

From-scratch OS. Not Unix-derived, not NT-derived. Capability-based microkernel, Rust, QEMU-first.

## Status

Boots. Kernel reaches its entry point and prints over serial on both BIOS and UEFI in QEMU. Everything past boot + serial (capabilities, IPC, scheduler, drivers) is still spec, not code.

## Structure

Project is split into cores. Each core owns its own README, CHANGELOG, BUGS, and TROUBLESHOOTING file. This file only tracks project-wide state.

| Core | Purpose | Status |
|---|---|---|
| kernel | capabilities, IPC, address spaces, scheduler, boot handoff | boots (serial hello-world); capabilities/IPC/scheduler not implemented yet |

## Design bets

- Capabilities instead of Unix permissions. No ambient root.
- IPC is the only kernel primitive. No large syscall table.
- Userspace is a component graph, not a process tree.
- No POSIX layer until the native model is proven. POSIX, if it ever exists, is a compat server bolted on later.

## Non-goals (for now)

Drivers beyond virtio, filesystems, networking, GUI, SMP, dynamic component loading. These get their own core + docs when they start.

## Where things live

- `docs/cores/<name>/` — per-core README, CHANGELOG, BUGS, TROUBLESHOOTING
- `kernel/` — kernel source (not yet created)
- `abi/` — shared types between kernel and userspace (not yet created)

## Log format

See `CHANGELOG.md` in this folder for project-level milestones. Per-core changelogs track implementation-level changes.

## Branching

Two branches: `main` (active work, can be broken) and `stable` (only known-good boots land here). See `docs/BRANCHING.md`.

## Testing

QEMU for automated smoke tests, Oracle VM VirtualBox as a second-opinion check before stable promotion. See `docs/TESTING.md`.

## Repo visibility

Private. Not published anywhere public.
