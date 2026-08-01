# Transition

Not a spec. This is the path from "Rose boots under Debian-based Meridian" to "Rose is Meridian's kernel and nothing Linux-derived is load-bearing." Written down so the shape of the plan doesn't drift phase to phase.

## Decision: hybrid shim, not full compat, not cold native

Three ways to read "replace the Debian kernel":

- Full Linux syscall compatibility (fork/execve, signals, mmap, VFS, cgroups/namespaces, abstract Unix sockets). Rejected: this is reimplementing most of Linux's kernel interface, and it fights the actual goal since matching Linux semantics that closely means becoming Linux-shaped.
- Cold native replacement, nothing from Debian's userland runs unmodified from day one. Rejected as the starting point: correct end state, but nothing is usable until the whole native stack exists, no incremental proof along the way.
- Hybrid: a minimal, deliberately narrow compatibility shim to bootstrap the transition, while everything meant to be permanent is written native from the start. The shim exists to shrink, not to grow toward parity. Chosen.

## Phase 0: kernel primitives

Status: mostly done. Frame allocator, GDT/IDT/TSS, paging, heap, timer, scheduler, address spaces, capabilities/CSpace, CSpace syscalls. See `docs/cores/kernel/README.md` and `docs/cores/kernel/CHANGELOG.md` for exact verified state per feature.

## Phase 1: real component model

Not started. Nothing past this point works until components can be created at runtime and can talk to each other; today everything is hardcoded in `kernel_main`'s self-test chain.

- Untyped/Retype: turn raw physical memory into typed kernel objects at runtime. Without it nothing but the kernel can create a CSpace, AddressSpace, or Thread.
- Endpoint/IPC: register-only synchronous message passing, per the spec already in `docs/cores/kernel/README.md`.
- Root task: replace the self-test chain with the real boot handoff already specced (kernel creates one privileged root task, hands it Untyped caps over all memory plus console/BootInfo caps, never creates another component itself again).
- Loader: root task parses and maps a binary (Rose's own format, not necessarily ELF) and starts its thread. Needs an initial boot archive bundled at build time; no filesystem yet.

## Phase 2: minimal native runtime

- Rose-native runtime crate (extends `abi`, or a new `rose-rt`): syscall wrappers, no libc, no POSIX assumptions.
- Console/serial driver moves out of the kernel into a userspace component holding IrqHandler/Frame caps. First end-to-end proof driver isolation works.
- Storage driver (virtio-blk under QEMU/VirtualBox) plus a minimal native filesystem server, since native components need to load from disk eventually, not just the boot archive.

This phase starts picking up items the kernel README currently lists as non-goals (drivers, storage). That list in `docs/README.md` gets updated as each one moves from "not started" to "in progress."

## Phase 3: the shim

One userspace component: a personality server translating a narrow slice of Linux syscalls into calls against the native filesystem/loader/IPC primitives from Phase 1-2.

Scope, deliberately narrow: enough for a shell, coreutils, a build toolchain. Not in scope unless something concrete in the transition needs it: cgroups, namespaces, abstract Unix sockets, the deeper systemd/D-Bus surface.

This is the only place Linux-shaped code exists anywhere in Rose. If it starts growing past what's needed to bootstrap the next phase, that's a signal of drifting back toward full compatibility, not progress.

## Phase 4: replace Meridian's stack outward

Order, lowest boot-dependency first: init/service supervisor, then shell, then display, then applications.

Rule: every time a native replacement lands for something the shim was covering, delete that part of the shim. Don't leave it in place "just in case." Shim line count should trend down every phase, never up.

## Phase 5: shim at zero

Fully native Meridian userland running on Rose. No Linux-compatibility code left anywhere. Interoperability with Linux/Windows/Mac (reading common filesystem formats, speaking common network protocols, maybe a foreign-binary translation layer) can exist afterward as an optional add-on, not as something the OS itself depends on.

## Tracking

Each phase's actual progress lives in the normal places: per-core CHANGELOG for implementation, `docs/CHANGELOG.md` for project-level milestones, `docs/BRANCHING.md` merge log for what's verified where. This file only tracks the plan shape, not current state; don't let per-feature status drift into this doc.
