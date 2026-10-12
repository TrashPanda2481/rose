# Root task / boot handoff: increment 1

Status: planned, not started. Branch `wt/root-task`.

## Goal

Replace the hand-assembled ring-3 self-test chain in `kernel_main` with a real
boot handoff: the kernel brings up its own primitives, then creates exactly one
privileged root task, grants it authority over all remaining memory plus a
BootInfo page, starts it in ring 3, and never creates another component itself.
This is Phase 1 of `docs/TRANSITION.md` and the handoff already specced in
`docs/cores/kernel/README.md` ("Boot handoff").

## Why now

Every v0.1 mechanism (Untyped/Retype, CSpace plus syscalls, Frame-Map,
Configure, Endpoint Send/Receive/Call/Reply) is implemented, promoted to
`stable`, and proven by the self-test chain. What does not exist yet is a single
authority root from which every other capability descends. Until it does,
"components created at runtime" is impossible, and every later phase (loader,
driver isolation, the shim) is blocked behind it.

## Scope (in)

1. BootInfo: a plain-data structure (new, in the `abi` crate) carrying what a
   root task needs and cannot derive for itself: a usable-memory summary,
   framebuffer geometry, and the hhdm offset. Not a capability; data only, per
   the README handoff spec. Stable field layout, since the root program reads it
   by offset.
2. Root resources the kernel builds at handoff:
   - a fresh AddressSpace,
   - a CSpace,
   - a Thread,
   - an Untyped capability over a large pool of remaining usable frames (see the
     contiguous-region scope cut),
   - the BootInfo page, mapped read-only into the root AddressSpace.
3. Handoff sequence in `kernel_main` (replacing the self-test tail at lines
   176-188): build the above, grant the Untyped and boot caps into the root
   CSpace, Configure the Thread against the root AddressSpace and CSpace with the
   root program's entry and stack, start the thread.
4. Root program (embedded for this increment; the external loader is increment
   2): proves the handoff worked by, as its first acts, reading BootInfo,
   retyping one Frame out of its own Untyped, Frame-Mapping it into its own
   AddressSpace, writing and reading back a pattern, then emitting one success
   line. This is the root-task equivalent of today's `usermode_selftest`, but
   reached through the real handoff rather than `kernel_main` wiring each grant
   to task 0 by hand.
5. Kernel idle: after starting the root thread, `kernel_main` enters its `hlt`
   idle loop and creates nothing further.

## Scope cuts (deferred, each with a reason)

- External binary loading and a boot archive: that is increment 2 (Loader) in
  `docs/TRANSITION.md`. The root program stays embedded in the kernel image (its
  own linker section, the same mechanism as today's ring-3 programs) here.
- Contiguous-region Untyped: the README lists this as boot handoff's job "once
  boot handoff is real." This increment grants a large bounded pool (frames
  pulled up front, as `UntypedObject` does today), not one contiguous physical
  region. Real contiguous-region Untyped is a follow-up (1b), so this increment
  does not also have to rework the allocator.
- A real console/serial device capability and a device syscall: device caps
  (IrqHandler or Frame backed) are Phase 2. For this increment the root program
  reports liveness through the existing `int 0x80` logging path (the kernel
  prints on its behalf). The handoff reserves the idea of a console cap but does
  not implement a usable one.
- Reclamation: revoke still does not return frames; unchanged here.
- The root creating a child component and doing IPC with it: a later increment,
  once the single root handoff is proven. Keep this increment to one root task
  so the failure surface stays small.

## Transition detail (what changes in `kernel_main`)

- Keep unchanged: lines 73-174 (serial, memory-map parse, `mem::init`,
  `gdt`/`idt`, `paging` plus framebuffer console, `heap`, `scheduler::init`,
  `pic`, `timer`). This is kernel bring-up and is orthogonal to the handoff.
- Keep (decision 1 below): the in-kernel boot self-tests (`frame_allocator`,
  `idt`, `paging`, `address_space`, `heap`, `timer`, `scheduler`,
  `scheduled_address_space`, `cspace`, `cspace_deep_revoke`, `pci`). They are the
  regression gate and run before the handoff. Proposed: keep them on every boot,
  unchanged.
- Replace: the `usermode_selftest` call (line 188) and the self-test's
  hand-assembled grant sequence in `usermode.rs` become the new handoff plus root
  program. Proposed: move the old `usermode_selftest` behind a `cfg` feature
  (retained as a regression artifact), since it exercises CSpace/Untyped error
  paths the root program will not.
- New: a root-task module (`kernel/src/root.rs`) that owns the handoff: builds
  the resources, performs the grants, Configures and starts the thread.
  `kernel_main` calls it where `usermode_selftest` is called today, then idles.
- `abi`: add `BootInfo` with a fixed layout.

## Done-when (observable unless marked inferred)

1. `cargo +nightly build --release` is clean, zero warnings. (observed)
2. QEMU BIOS smoke test still prints `rose: hello, hardware` and reaches the new
   root-task success line. (observed, subject to the known QEMU/TCG no-IRQ0
   truncation for anything that needs a real tick)
3. The in-kernel self-tests still pass on boot, unchanged. (observed)
4. The root task runs in ring 3, in its own AddressSpace and CSpace, having
   received its caps through the handoff, not through a `kernel_main` hardcoded
   grant to task 0. (observed, via the success line's content)
5. The root task retypes a Frame from its granted Untyped, maps it, writes and
   reads back a pattern. (observed)
6. `kernel_main` creates no component after the root task; the kernel idles.
   (inferred from code, observed as the absence of any further
   component-creation log line)
7. VirtualBox: the full sequence completes under real hardware preemption, with
   the success line present in the serial log. (observed, the stable gate)

## VM check line to look for

Propose one greppable success line the root program emits as its last step:

    rose: root task: handoff ok (bootinfo read, untyped retype+map+rw confirmed)

The smoke test's `EXPECTED_LINE` stays `rose: hello, hardware` (it must; that
prints before paging). The root-task line is the human and VirtualBox check for
this increment. If the smoke test should also assert it, that is a one-line
addition to `tools/smoke-test.sh` (a second grep); see decision 4.

## Decisions to confirm before coding

1. Keep the in-kernel self-tests running on every boot (proposed yes), or move
   them behind a `cfg` feature so a normal boot goes straight to the handoff?
2. Old ring-3 `usermode_selftest`: feature-gate and keep (proposed), or delete it
   now that the root task supersedes it?
3. Untyped granted to root: large bounded pool now (proposed), with
   contiguous-region Untyped scheduled as increment 1b, or build
   contiguous-region Untyped as part of this increment?
4. Success-line wording, and whether `tools/smoke-test.sh` should grep for it
   too.
5. BootInfo contents: the full raw memory map copied into the page, or a compact
   summary (usable total, framebuffer geometry, hhdm offset)? Proposed: compact
   summary here, raw map when the loader needs it.

## Risk and rollback

- All work is on branch `wt/root-task`; nothing reaches `main` or `stable` until
  it is merged. Rollback is "do not merge," or `tools/ws.sh rollback`.
- The handoff touches the one place authority comes from nothing (README: "If it
  doesn't trace back, something's wrong"), so the grants are security-sensitive.
  Each grant's rights must be minimal and intentional, and reviewed against the
  security-audit findings already closed in `BUGS.md` (the Frame-Map kernel
  address-space gate, the Untyped WRITE-right check, verify-before-commit).
- Gate: build gate on this Windows checkout; full QEMU smoke plus VirtualBox on
  the VM box per `docs/TESTING.md` before any promotion to `stable`.
