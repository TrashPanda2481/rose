# Kernel core

Status: boots. Entry point + serial output working on BIOS and UEFI in QEMU, and verified in Oracle VM VirtualBox. Limine's memory map is parsed and logged (base/length/type per region, usable total). Physical frame allocator is in (`kernel/src/mem.rs`), free-list based, addressed via HHDM, verified with a self-test on boot. Our own GDT/IDT/TSS are loaded (`kernel/src/gdt.rs`, `kernel/src/idt.rs`), all 32 CPU exception vectors handled, double fault backed by an IST stack, verified with a breakpoint self-test on boot plus manual #GP/#PF fault injection during development. Kernel now owns its own page tables (`kernel/src/paging.rs`): kernel image mapped section-by-section with W^X (text R+X, rodata R-only, data/bss R+W+NX), HHDM remapped for usable and bootloader-reclaimable memory, verified with a map/write/read/unmap self-test on boot. A kernel heap is in (`kernel/src/heap.rs`), 1MiB fixed region, free-list first-fit allocator backing `extern crate alloc` (Box/Vec/etc.), verified with a self-test that allocates a Box and a growing Vec through the real `alloc` API and confirms bytes return to the free list on drop. A PIC-remapped, PIT-driven timer is in (`kernel/src/pic.rs`, `kernel/src/timer.rs`), IRQ0 mapped to vector 32 at 100Hz. Hardware IRQ0 delivery does not reach the CPU in this dev sandbox's QEMU/TCG (see TROUBLESHOOTING.md), so the boot self-test is timeout-bounded there and falls back to a software-triggered interrupt. Fully verified in Oracle VM VirtualBox (real hardware virtualization): self-test passes clean, `50 ticks elapsed via hardware irq0, 510ms since boot`, confirming end-to-end hardware interrupt delivery works and the timeout fix holds. GDT/IDT/page tables/heap/timer boot chain all verified in BIOS+UEFI QEMU and in Oracle VM VirtualBox; timer is the only one that behaves differently between the two (software-int fallback in this sandbox's QEMU/TCG, real hardware ticks in VirtualBox), which is expected and documented. A v0.1 scheduler is in (`kernel/src/scheduler.rs`): single round-robin queue, fixed timeslice, hand-written context switch (naked asm), preemption from both the timer IRQ and a voluntary `yield_now`. Verified with a cooperative two-task self-test (`task_a=10 task_b=10 switches=31`) that's deterministic regardless of whether hardware timer interrupts fire; real hardware ticks interleave through the same round-robin rule opportunistically wherever they land. `scheduler::init()` runs before interrupts are ever enabled specifically because a VirtualBox boot on real hardware exposed a boot-order bug (first real tick landing before the scheduler existed panicked on a modulo by zero; this sandbox's QEMU/TCG never surfaced it since it never delivers a real IRQ0); see `BUGS.md`. Verified on BIOS+UEFI QEMU and the fix confirmed in Oracle VM VirtualBox, zero build warnings. Promoted to `stable`. Ring 3 / user mode is in (`kernel/src/usermode.rs`): new user GDT selectors, a DPL=3 syscall gate at IDT vector 0x80, and a `PAGE_USER` propagation fix in paging.rs so U/S actually holds across every level of the page-table walk, not just the leaf. A hand-written asm program makes two round-trip `int 0x80` syscalls from ring 3 before the kernel halts for good; no process model, no return path back into the interrupted kernel flow, and no second user task yet, all deliberate v0.1 scope cuts (see module docs in usermode.rs). Verified on BIOS+UEFI QEMU, zero build warnings, and confirmed in Oracle VM VirtualBox (identical two-syscall sequence and halt line). Promoted to `stable`. Address spaces are in (`kernel/src/paging.rs`): an `AddressSpace` handle wraps a PML4 root; `kernel_address_space()` returns the kernel's own, `new_address_space()` allocates a fresh one sharing the kernel/HHDM upper half (PML4 entries 256..512, present but supervisor-only) with a zeroed private lower half. `map_page`/`unmap_page`/`switch`/`is_mapped` are methods on it now instead of operating on one implicit global table set. The usermode self-test now runs its program in a real second AddressSpace instead of the kernel's own; the kernel writes the program bytes in through the target frame's permanent HHDM alias rather than through the not-yet-active user virtual address, which also makes the earlier WRITABLE-then-remap workaround for that copy unnecessary going forward (kept in BUGS.md as history, not deleted; see its own CHANGELOG entry). A boot self-test proves two AddressSpaces are actually isolated (`is_mapped` shows a page present in one and absent in the other, checked without ever dereferencing the absent side) and that switching CR3 into a fresh AddressSpace, writing, reading back, and switching back to the kernel works. No teardown/free for an AddressSpace once built, same accepted v0.1 scope cut as the heap's no-coalescing and the scheduler's no-task-exit. Verified on BIOS+UEFI QEMU, zero build warnings, and confirmed in Oracle VM VirtualBox (isolation confirmed, write/readback match, usermode self-test identical to QEMU, real hardware timer ticks as expected). Promoted to `stable`. The scheduler is now address-space-aware (`kernel/src/scheduler.rs`): each `Task` carries the `AddressSpace` it runs in, `spawn` defaults to the kernel's own, and a new `spawn_in` picks any; both `tick()` and `yield_now()` reload CR3 for the incoming task right before the existing register-swap `context_switch`, unconditionally for now, no same-AS skip yet. A new self-test spawns one task per AddressSpace, each mapping the same virtual address to a different physical frame with a different pattern, and confirms both read back only their own pattern after several real preemptions/yields interleaved with the existing scheduler self-test's tasks, proving CR3 actually follows the task under real scheduling rather than a single manual switch. Verified on BIOS+UEFI QEMU, zero build warnings, and confirmed in Oracle VM VirtualBox: identical isolation result with real hardware timer ticks interleaved in, same as the QEMU run. Promoted to `stable`. Capabilities/IPC below are still spec only, not implemented.

## Boot, concretely

- Target: built-in `x86_64-unknown-none` (see TROUBLESHOOTING.md for why not a custom json target).
- Bootloader: Limine, `v11.x-binary` release, vendored under `boot/limine-bootloader/`.
- Entry: `kernel_main`, reached via Limine protocol requests + base revision check.
- First hardware interface: COM1 UART (port I/O, no MMIO/framebuffer needed). Same driver backs the panic handler.
- Build/run: `tools/build-iso.sh`, `tools/run-bios.sh`, `tools/run-uefi.sh`, `tools/smoke-test.sh`.
- Smoke test line: `rose: hello, hardware`.

Scope for v0.1: capabilities, CSpace, IPC (register-only), address spaces, boot handoff, scheduler. Nothing else. Drivers, storage, networking, GUI, SMP come later as their own cores.

## Capability

Unforgeable kernel-mediated reference to an object, plus rights.

```
Capability {
    object_ref:  KernelObjectId
    object_type: Untyped | Endpoint | Notification | Thread |
                 AddressSpace | PageTable | Frame | IrqHandler | Reply
    rights:      bitfield (Read, Write, Grant, Map, Send, Receive)
    badge:       u64 (optional sender-set tag)
}
```

No raw pointers or IDs in userspace. Only capabilities. Kernel is the only thing that dereferences one.

## CSpace

Each component has one capability table, addressed by local integers (CPtr). A component can't name another component's CPtr. If it's not in your CSpace, it doesn't exist for you.

## Deriving capabilities

- Copy: new slot, same object, equal or lower rights.
- Mint: derived cap, new badge and/or lower rights.
- Move: transferred out of sender's CSpace, sender loses it.
- Revoke: destroys the cap and everything derived from it. Kernel tracks a derivation tree per object.

Open question: does revocation always cascade fully (seL4 style), or can a cap be sealed to survive a parent's revocation (EROS style, for persistent refs later)? Default: full cascade for v0.1. Revisit once there's an object store.

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

No "process," "file," or "socket" here. Those get built on top later, if at all.

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

## Open questions (blocking nothing yet, but need answers before code locks in)

1. Revocation: full cascade vs. sealed caps.
2. Message limits: MAX_MSG_WORDS / MAX_MSG_CAPS values.
3. Pager: per-component vs. shared root pager for v0.1.

## Naming/versioning

This doc is v0.1. Breaking changes to the model bump the version and get a line in `CHANGELOG.md`. Object types, syscall numbers, and message labels live in one shared `abi` crate, never duplicated by hand on both sides of the kernel/userspace boundary.
