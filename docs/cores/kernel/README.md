# Kernel core

Status: boots. Entry point + serial output working on BIOS and UEFI in QEMU, and verified in Oracle VM VirtualBox. Limine's memory map is parsed and logged (base/length/type per region, usable total). Physical frame allocator is in (`kernel/src/mem.rs`), free-list based, addressed via HHDM, verified with a self-test on boot. Our own GDT/IDT/TSS are loaded (`kernel/src/gdt.rs`, `kernel/src/idt.rs`), all 32 CPU exception vectors handled, double fault backed by an IST stack, verified with a breakpoint self-test on boot plus manual #GP/#PF fault injection during development. Kernel now owns its own page tables (`kernel/src/paging.rs`): kernel image mapped section-by-section with W^X (text R+X, rodata R-only, data/bss R+W+NX), HHDM remapped for usable and bootloader-reclaimable memory, verified with a map/write/read/unmap self-test on boot. A kernel heap is in (`kernel/src/heap.rs`), 1MiB fixed region, free-list first-fit allocator backing `extern crate alloc` (Box/Vec/etc.), verified with a self-test that allocates a Box and a growing Vec through the real `alloc` API and confirms bytes return to the free list on drop. A PIC-remapped, PIT-driven timer is in (`kernel/src/pic.rs`, `kernel/src/timer.rs`), IRQ0 mapped to vector 32 at 100Hz. Hardware IRQ0 delivery does not reach the CPU in this dev sandbox's QEMU/TCG (see TROUBLESHOOTING.md), so the boot self-test is timeout-bounded there and falls back to a software-triggered interrupt. Fully verified in Oracle VM VirtualBox (real hardware virtualization): self-test passes clean, `50 ticks elapsed via hardware irq0, 510ms since boot`, confirming end-to-end hardware interrupt delivery works and the timeout fix holds. GDT/IDT/page tables/heap/timer boot chain all verified in BIOS+UEFI QEMU and in Oracle VM VirtualBox; timer is the only one that behaves differently between the two (software-int fallback in this sandbox's QEMU/TCG, real hardware ticks in VirtualBox), which is expected and documented. A v0.1 scheduler is in (`kernel/src/scheduler.rs`): single round-robin queue, fixed timeslice, hand-written context switch (naked asm), preemption from both the timer IRQ and a voluntary `yield_now`. Verified with a cooperative two-task self-test (`task_a=10 task_b=10 switches=31`) that's deterministic regardless of whether hardware timer interrupts fire; real hardware ticks interleave through the same round-robin rule opportunistically wherever they land. `scheduler::init()` runs before interrupts are ever enabled specifically because a VirtualBox boot on real hardware exposed a boot-order bug (first real tick landing before the scheduler existed panicked on a modulo by zero; this sandbox's QEMU/TCG never surfaced it since it never delivers a real IRQ0); see `BUGS.md`. Verified on BIOS+UEFI QEMU and the fix confirmed in Oracle VM VirtualBox, zero build warnings. Promoted to `stable`. Ring 3 / user mode is in (`kernel/src/usermode.rs`): new user GDT selectors, a DPL=3 syscall gate at IDT vector 0x80, and a `PAGE_USER` propagation fix in paging.rs so U/S actually holds across every level of the page-table walk, not just the leaf. A hand-written asm program makes two round-trip `int 0x80` syscalls from ring 3 before the kernel halts for good; no process model, no return path back into the interrupted kernel flow, and no second user task yet, all deliberate v0.1 scope cuts (see module docs in usermode.rs). Verified on BIOS+UEFI QEMU, zero build warnings, and confirmed in Oracle VM VirtualBox (identical two-syscall sequence and halt line). Promoted to `stable`. Address spaces are in (`kernel/src/paging.rs`): an `AddressSpace` handle wraps a PML4 root; `kernel_address_space()` returns the kernel's own, `new_address_space()` allocates a fresh one sharing the kernel/HHDM upper half (PML4 entries 256..512, present but supervisor-only) with a zeroed private lower half. `map_page`/`unmap_page`/`switch`/`is_mapped` are methods on it now instead of operating on one implicit global table set. The usermode self-test now runs its program in a real second AddressSpace instead of the kernel's own; the kernel writes the program bytes in through the target frame's permanent HHDM alias rather than through the not-yet-active user virtual address, which also makes the earlier WRITABLE-then-remap workaround for that copy unnecessary going forward (kept in BUGS.md as history, not deleted; see its own CHANGELOG entry). A boot self-test proves two AddressSpaces are actually isolated (`is_mapped` shows a page present in one and absent in the other, checked without ever dereferencing the absent side) and that switching CR3 into a fresh AddressSpace, writing, reading back, and switching back to the kernel works. No teardown/free for an AddressSpace once built, same accepted v0.1 scope cut as the heap's no-coalescing and the scheduler's no-task-exit. Verified on BIOS+UEFI QEMU, zero build warnings, and confirmed in Oracle VM VirtualBox (isolation confirmed, write/readback match, usermode self-test identical to QEMU, real hardware timer ticks as expected). Promoted to `stable`. The scheduler is now address-space-aware (`kernel/src/scheduler.rs`): each `Task` carries the `AddressSpace` it runs in, `spawn` defaults to the kernel's own, and a new `spawn_in` picks any; both `tick()` and `yield_now()` reload CR3 for the incoming task right before the existing register-swap `context_switch`, unconditionally for now, no same-AS skip yet. A new self-test spawns one task per AddressSpace, each mapping the same virtual address to a different physical frame with a different pattern, and confirms both read back only their own pattern after several real preemptions/yields interleaved with the existing scheduler self-test's tasks, proving CR3 actually follows the task under real scheduling rather than a single manual switch. Verified on BIOS+UEFI QEMU, zero build warnings, and confirmed in Oracle VM VirtualBox: identical isolation result with real hardware timer ticks interleaved in, same as the QEMU run. Promoted to `stable`. Capabilities/CSpace are in (`abi` crate, `kernel/src/cspace.rs`): a real `abi` crate holds `ObjectType`/`Rights`/`Capability`/`CPtr`/`KernelObjectId` so kernel and future userspace never hand-duplicate them, and `CSpace` is a fixed 64-slot table supporting grant/copy/mint/move/revoke with a derivation tree for cascading revocation. Verified on BIOS+UEFI QEMU, zero build warnings. Confirmed in Oracle VM VirtualBox (see the task 0 address-space sync fix below, which is what made a full VirtualBox run possible end to end). Promoted to `stable`. CSpace syscalls are in (`kernel/src/syscall.rs`): every `Task` now owns a CSpace, and `int 0x80` reaches `SYS_CSPACE_COPY`/`MINT`/`MOVE`/`REVOKE` against the calling task's own CSpace, args in rdi/rsi/rdx/r10, result in rax (0/positive success, negative magnitude a `CSpaceError` code). The usermode self-test's ring-3 program now runs a fixed six-step sequence exercising all four ops, including both expected error paths (dest occupied, source revoked), against a root capability the boot-handoff stand-in grants ahead of time. Verified on BIOS+UEFI QEMU, zero build warnings. Confirmed in Oracle VM VirtualBox. Promoted to `stable`. Not wired to Untyped/Retype or Endpoint/IPC yet; see this core's own CHANGELOG entry for exact scope. IPC below is still spec only, not implemented. Untyped/Retype is in (`kernel/src/untyped.rs`, `kernel/src/syscall.rs`): an `UntypedObject` is a pool of individually-allocated physical frames (not yet a contiguous region, see this core's own CHANGELOG entry for the full scope cut list), and `retype` pops one frame out of that pool and turns it into a Frame, CSpace, AddressSpace, or Thread object; those four are retypeable in v0.1. `int 0x80` reaches `SYS_UNTYPED_RETYPE` (0x1004) the same way the CSpace syscalls do, and a new `CSpace::install_child` installs the freshly retyped object's capability as a child of the Untyped cap it came from, reusing the existing derivation tree so revoking the Untyped cascades into it for free. The usermode self-test's ring-3 program now runs six more syscalls after the six CSpace ones, retyping a Frame, a CSpace, an AddressSpace, and a Thread successfully in that order, then hitting Exhausted (pool of four frames now spent) and UnsupportedType (PageTable is a deliberate punt, not retypeable) on purpose, against a root Untyped cap the boot-handoff stand-in grants ahead of time. Verified on BIOS+UEFI QEMU, zero build warnings. First VirtualBox run caught a real scheduler/AddressSpace gap (see BUGS.md, fixed) rather than a bug in Retype itself; confirmed clean end to end in Oracle VM VirtualBox once that fix landed. Promoted to `stable`. Task 0's own `Task.address_space` field now stays in sync whenever `usermode_selftest` manually switches CR3 (`scheduler::set_current_address_space`), so a real timer preemption mid ring-3-program no longer reverts CR3 to the kernel's own AddressSpace out from under it. AddressSpace and Thread retype extends the same match arm (see this core's own CHANGELOG entry): an AddressSpace's `KernelObjectId` is its own PML4 physical address, same stable-identity reuse Frame already does; a Thread's is a registry index into a new module-local table (`kernel/src/untyped.rs`'s `THREAD_OBJECTS`), a bare placeholder object with no scheduler wiring yet, that's the Configure/Resume/Suspend/SetPriority syscall feature's job. Verified on BIOS QEMU, zero build warnings, and confirmed in Oracle VM VirtualBox (identical six-step sequence and outcomes, real hardware timer ticks). Promoted to `stable`.

## Boot, concretely

- Target: built-in `x86_64-unknown-none` (see TROUBLESHOOTING.md for why not a custom json target).
- Bootloader: Limine, `v11.x-binary` release, vendored under `boot/limine-bootloader/`.
- Entry: `kernel_main`, reached via Limine protocol requests + base revision check.
- First hardware interface: COM1 UART (port I/O, no MMIO/framebuffer needed). Same driver backs the panic handler.
- Build/run: `tools/build-iso.sh`, `tools/run-bios.sh`, `tools/run-uefi.sh`, `tools/smoke-test.sh`.
- Smoke test line: `rose: hello, hardware`.

Scope for v0.1: capabilities, CSpace, IPC (register-only), address spaces, boot handoff, scheduler. Nothing else. Drivers, storage, networking, GUI, SMP come later as their own cores.

## Capability

Unforgeable kernel-mediated reference to an object, plus rights. Implemented in the `abi` crate (`abi::Capability`), used by `kernel/src/cspace.rs`.

```
Capability {
    object_ref:  KernelObjectId
    object_type: Untyped | Endpoint | Notification | Thread |
                 AddressSpace | PageTable | Frame | IrqHandler | Reply | CSpace
    rights:      bitfield (Read, Write, Grant, Map, Send, Receive)
    badge:       u64 (optional sender-set tag)
}
```

No raw pointers or IDs in userspace. Only capabilities. Kernel is the only thing that dereferences one. `KernelObjectId` in v0.1 reuses whatever stable identity the referenced object already has (an AddressSpace's own PML4 physical address, via `AddressSpace::raw()`) rather than a generic object heap; that heap is Untyped/Retype's job, not yet built.

Update: Untyped/Retype landed (see "Untyped/Retype" section below). It still isn't a generic object heap for every type above, just Frame, CSpace, AddressSpace, and Thread in v0.1; a Frame's `KernelObjectId` is its physical address, an AddressSpace's is its own PML4 physical address (`AddressSpace::raw()`), both reusing a stable identity the object already has; a CSpace's or Thread's is a registry index into its own module-local table (`kernel/src/untyped.rs`'s `CSPACE_OBJECTS`/`THREAD_OBJECTS`), since a freshly retyped CSpace or Thread has no other stable address to reuse.

## Untyped/Retype

Untyped is a pool of individually-allocated physical frames, not yet shaped into anything. Retype pops one frame out of that pool and turns it into a Frame, CSpace, AddressSpace, or Thread object; those four are retypeable in v0.1. PageTable, Endpoint, Notification, Reply, and IrqHandler are still future features, added to `retype`'s match arm when their turn comes. PageTable specifically is a deliberate punt, not just unimplemented: `paging.rs`'s `AddressSpace::map_page` already manages intermediate page-table levels internally, invisible to the capability model, and there's no current need to reason about a table level as its own object; revisit if that changes.

Implemented (`kernel/src/untyped.rs`, `kernel/src/syscall.rs`): `UntypedObject::new(count)` pulls `count` frames from the global frame allocator up front; `retype(untyped_id, object_type)` takes the next frame off that pool's watermark and, depending on `object_type`, hands back a `KernelObjectId`: the frame's own physical address (Frame), a new `CSpace` boxed into a module-local registry, its index returned (CSpace), a freshly built `paging::AddressSpace`'s own PML4 physical address via `new_address_space()` (AddressSpace), or a placeholder `ThreadObject` pushed into its own module-local registry, its index returned (Thread). Reached via `SYS_UNTYPED_RETYPE` (0x1004): args are the Untyped cptr, the raw `ObjectType` byte (see `abi`'s `ObjectType::to_u8`/`from_u8`), and the destination cptr for the newly minted cap. Dispatch checks everything it can before calling `retype` (both cptrs resolve, the source cap really is Untyped, `object_type` is a real and retypeable variant, the destination slot is empty) since a frame `retype` hands out has no way back into the pool if the install after it is rejected; `CSpace::install_child` (new) is what performs that install, recording the new cap as a child of the Untyped cap in the same derivation tree `copy`/`mint` already use, so revoking the Untyped cascades into whatever was retyped from it. Default rights on the new cap: Frame gets READ|WRITE|MAP, CSpace gets GRANT, AddressSpace gets MAP (for a future Frame.Map syscall to check on the target), Thread gets GRANT (provisional, same loose convention as CSpace, pending the Configure/Resume/Suspend/SetPriority syscalls actually defining per-op checks).

Scope cuts, all deliberate, not oversights (see `kernel/src/untyped.rs`'s own module doc):
- Pool is frames pulled one at a time from the allocator, not one contiguous physical region. Real contiguous-region Untyped is boot handoff's job later, once boot handoff is real.
- Frame, CSpace, AddressSpace, and Thread are retypeable. PageTable is a deliberate punt (see above); Endpoint, Notification, Reply, and IrqHandler are simply not built yet.
- A retyped CSpace or Thread is bookkept as one frame charged out of the pool, but the actual struct lives on the kernel heap via `Box` (CSpace) or a module-local registry (Thread), not placed in that physical frame. A retyped AddressSpace charges that same one bookkeeping frame from the pool on top of the real, separate PML4 frame `new_address_space` allocates directly from the global frame allocator. Documented asymmetry, extended consistently rather than special-cased.
- Revoking a retyped cap kills the cap and its derivation subtree, same as every other cap, but doesn't return frames to the allocator or roll back the watermark. No reclamation yet, same open-question category as full-cascade-vs-sealed-caps above.
- Frame caps minted here don't do anything yet; nothing calls Map/Unmap on them. This proves a Frame cap can be minted from real memory and tracked, not that it's usable in paging yet.
- A retyped Thread is a bare placeholder: nothing configures it, gives it a stack/entry point, or makes it schedulable, and it isn't wired into `scheduler.rs`'s task list. That wiring is the Configure/Resume/Suspend/SetPriority syscall feature's job, not this one's.
- A retyped AddressSpace is real and immediately usable by `paging.rs`, but empty; nothing maps anything into it here. That's a future Frame.Map syscall's job.

## CSpace

Each component has one capability table, addressed by local integers (CPtr). A component can't name another component's CPtr. If it's not in your CSpace, it doesn't exist for you.

Implemented (`kernel/src/cspace.rs`): fixed 64-slot table, CPtr 0 reserved as the null cap. Every `Task` now owns one (`kernel/src/scheduler.rs`), empty at creation. A syscall reaches it (`kernel/src/syscall.rs`, `int 0x80` vector 0x80): `SYS_CSPACE_COPY`/`MINT`/`MOVE`/`REVOKE`, args in rdi/rsi/rdx/r10, result in rax on return (0/positive success, negative magnitude a `CSpaceError` code). Dispatch always targets the calling task's own CSpace, via `scheduler::with_current_cspace`. Confirmed in Oracle VM VirtualBox. Promoted to `stable`.

## Deriving capabilities

- Copy: new slot, same object, equal or lower rights.
- Mint: derived cap, new badge and/or lower rights.
- Move: transferred out of sender's CSpace, sender loses it.
- Revoke: destroys the cap and everything derived from it. Kernel tracks a derivation tree per object.

Open question: does revocation always cascade fully (seL4 style), or can a cap be sealed to survive a parent's revocation (EROS style, for persistent refs later)? Default: full cascade for v0.1. Revisit once there's an object store.

Implemented (`kernel/src/cspace.rs`): `copy`/`mint` both record the new slot as a child of the source in the derivation tree (a plain copy differs from a mint only in badge/rights, not in whether revoking the source should take it down too), so `revoke` walks the tree and clears every descendant, matching the full-cascade default above. `move_slot` only re-homes a cap within the same CSpace for v0.1; the cross-CSpace half of Move needs a delivery mechanism, which is what IPC is for, and IPC isn't implemented yet.

## Object types, v0.1

| Type | Represents | Ops |
|---|---|---|
| Untyped | raw physical memory, no shape yet | Retype |
| Frame | one physical page | Map, Unmap |
| PageTable | page-table level | Install into AddressSpace |
| AddressSpace | a component's VM root | Install PageTable/Frame |
| Thread | schedulable unit | Configure, Resume, Suspend, SetPriority |
| Endpoint | sync IPC rendezvous | Send, Receive, Call |
| Notification | async signal, binary | Signal, Wait, Poll |
| Reply | one-shot reply cap from Call | Reply |
| IrqHandler | binds an IRQ line to a Notification | Ack, Bind |
| CSpace | a component's capability table | Insert, Delete |

No "process," "file," or "socket" here. Those get built on top later, if at all.

`CSpace` added to this table this feature: "Boot handoff" already grants the root task a cap to its own CSpace, so a CSpace has to be a nameable object type; the table was just missing the row until now.

## IPC

Register-only for v0.1. No shared buffer, no memory grants yet. Fixed-size message:

```
Message {
    label:     u32   (opcode, receiver interprets)
    length:    u8     (0..MAX_MSG_WORDS)
    cap_count: u8     (0..MAX_MSG_CAPS)
    data:      [u64; MAX_MSG_WORDS]
    caps:      [CPtr; MAX_MSG_CAPS]
}
```

Open question: MAX_MSG_WORDS / MAX_MSG_CAPS size. Default 4 words / 2 caps. Small on purpose: forces bulk transfer through a real grant mechanism later instead of a bloated struct now.

Three verbs:
- Send: blocks if no receiver waiting / no queue space.
- Receive: blocks until a Send arrives. `badge` tells you which minted cap the sender used.
- Call: Send + implicit Receive on a kernel-generated one-shot Reply cap. Main pattern for request/response between components.

Interrupts and async events use Notification + Signal + Wait. No separate interrupt-handler concept. Same primitive as everything else.

## Address spaces

One AddressSpace per component, built from PageTable/Frame objects retyped out of Untyped. No implicit kernel-visible-to-all region: kernel memory is structurally unmappable by components, not just off-limits by convention.

Current v0.1 mechanism (`kernel/src/paging.rs`) doesn't fully satisfy that last sentence yet: every `AddressSpace` shares the kernel's PML4 entries 256..512 (HHDM plus the kernel image) so the kernel can always run regardless of which one is loaded in CR3. Those entries are present but marked supervisor-only, so ring 3 genuinely can't read or write them; that's real isolation for the ring3/usermode case. It's conventional (pre-KPTI-style) kernel design, though, not the stricter structurally-unmappable goal above, since the mapping still exists in every component's own page tables. Known v0.1 limitation, not a bug; revisit for v0.2 hardening (KPTI-style or seL4-style layout) if Meltdown-class concerns become load-bearing here.

Update: the scheduler's `Task` now carries an `AddressSpace` and both `tick()`/`yield_now()` reload CR3 for the incoming task before the register-swap `context_switch` runs (`kernel/src/scheduler.rs`). This closes the gap above; the scheduler itself can now run components in separate AddressSpaces. What's still missing is everything else a real component needs: no CSpace/capabilities yet (below), so there's nothing yet stopping a task in its own AddressSpace from calling arbitrary kernel functions directly, since it's still plain Rust code linked into the same kernel image, not an isolated userspace binary reached only through syscalls. That's the next layer, not this one.

Page faults: kernel doesn't resolve them. Converts fault into an IPC to a pager cap the component configured ahead of time. Pager decides map / kill / other. Mechanism stays in kernel, policy moves to userspace.

Open question: one pager per component, or one shared root pager for v0.1? Default: shared root pager for now. Split once the component model itself is real; don't solve paging policy and component isolation at the same time.

No global address, no global path. Sharing between components only happens if one explicitly maps or lends a capability. Never a default.

## Boot handoff

1. Bootloader (Limine) hands kernel a memory map, framebuffer, entry point.
2. Kernel sets up its own state: frame allocator, GDT/IDT, page tables, timer.
3. Kernel creates the root task, first and only privileged component, and gives it:
   - Untyped caps covering all remaining physical memory
   - Caps to its own AddressSpace, Thread, CSpace
   - Cap to the boot console/serial device
   - A BootInfo page (plain data, not a cap): memory map, framebuffer geometry
4. Kernel starts the root task's thread. Kernel never creates another component after this; root task does it by retyping Untyped memory.

Actual v0.1 order in step 2 is GDT/IDT before page tables, not the reverse: neither GDT/IDT nor their self-tests touch anything beyond static kernel data that's already mapped by Limine's own tables, so there's no reason to gate them on the kernel owning its own hierarchy first. Page tables come right after because everything past this point (a heap, per-component AddressSpace objects, user mode) needs the kernel to own its mappings outright rather than borrowing the bootloader's indefinitely.

This is the only place authority comes from nothing. Every other capability in the system traces back to this handoff. If it doesn't trace back, something's wrong.

## Scheduler, v0.1

- Fixed-priority round-robin. No deadlines, no fairness heuristics, no dynamic priority.
- Priority: small int range (0-15), set at Thread config time, changeable by whoever holds the Thread cap.
- One fixed timeslice for everyone at a given priority.
- Preempt on timer tick or on block (Receive/Call/Wait with nothing to do).
- No starvation handling. A saturated high-priority thread can starve everyone below it. Known gap, not fixing yet: IPC correctness comes first.

Update: the spec above describes the eventual per-priority model; v0.1 code (`kernel/src/scheduler.rs`) is flatter than that, one round-robin queue, no priority field on `Task` at all yet. `scheduler::spawn_user` (Configure syscall, `kernel/src/thread.rs`) is the first way to get a task into that queue starting in ring 3 rather than ring 0; it takes an already-built `AddressSpace` and `CSpace` rather than building its own, both supplied by whoever calls Configure. Priority is deliberately not a Configure argument, since there's still no field to set it on; a future `SetPriority` syscall is what actually needs to exist before priority becomes real, not this one.

Known gap, not solved by Configure: `gdt::set_kernel_stack` (TSS.RSP0) is a single global value, set once before the first ring3->ring0 trap and never updated per-task. Fine as long as only one ring-3 thread is ever mid-trap at a time, true today since there's no real concurrency (single core, cooperative-plus-timer-tick scheduling, no syscall reentrancy). Stops being fine the moment two ring-3 threads could both be trapping concurrently; a real fix needs a per-task kernel stack and a TSS.RSP0 reload on every switch-in, not just at boot. Not needed yet, since Configure only proves a *second* task can exist and run, not that two can trap at once.

## Design principle: verify before commit

A standing rule for kernel code, not a single feature: nothing acts on unconfirmed information. Every place the kernel is about to commit state based on something it hasn't checked yet gets a read-only verification step first, and only proceeds once that check passes. Unconfirmed data is treated as untrusted by default, not optimistically assumed correct.

The pattern already exists in the Configure syscall (`kernel/src/thread.rs`): `peek_thread_unconfigured` reads a thread's state without changing anything, and only `claim_thread` is allowed to actually commit it, and only after the peek confirms the thread is in the expected state. PCI enumeration (`kernel/src/pci.rs`) follows the same shape at the hardware level: a slot's vendor id is read first, and every other register on that slot is only read once the vendor id confirms something is actually there. Nothing about a bus, a device, or a thread is assumed true because it seemed likely; it's read, checked, and only then acted on.

This is the rule any future syscall or hardware-facing code should be held against: find the point where the code is about to trust something it hasn't verified, and put the read-only check before the commit, not after.

## Open questions (blocking nothing yet, but need answers before code locks in)

1. Revocation: full cascade vs. sealed caps.
2. Message limits: MAX_MSG_WORDS / MAX_MSG_CAPS values.
3. Pager: per-component vs. shared root pager for v0.1.

## Naming/versioning

This doc is v0.1. Breaking changes to the model bump the version and get a line in `CHANGELOG.md`. Object types, syscall numbers, and message labels live in one shared `abi` crate, never duplicated by hand on both sides of the kernel/userspace boundary.
