# Kernel core

Status: boots. Entry point + serial output working on BIOS and UEFI in QEMU, and verified in Oracle VM VirtualBox. Limine's memory map is parsed and logged (base/length/type per region, usable total). Physical frame allocator is in (`kernel/src/mem.rs`), free-list based, addressed via HHDM, verified with a self-test on boot. Our own GDT/IDT/TSS are loaded (`kernel/src/gdt.rs`, `kernel/src/idt.rs`), all 32 CPU exception vectors handled, double fault backed by an IST stack, verified with a breakpoint self-test on boot plus manual #GP/#PF fault injection during development. Kernel now owns its own page tables (`kernel/src/paging.rs`): kernel image mapped section-by-section with W^X (text R+X, rodata R-only, data/bss R+W+NX), HHDM remapped for usable and bootloader-reclaimable memory, verified with a map/write/read/unmap self-test on boot. A kernel heap is in (`kernel/src/heap.rs`), 1MiB fixed region, free-list first-fit allocator backing `extern crate alloc` (Box/Vec/etc.), verified with a self-test that allocates a Box and a growing Vec through the real `alloc` API and confirms bytes return to the free list on drop. A PIC-remapped, PIT-driven timer is in (`kernel/src/pic.rs`, `kernel/src/timer.rs`), IRQ0 mapped to vector 32 at 100Hz. Hardware IRQ0 delivery does not reach the CPU in this dev sandbox's QEMU/TCG (see TROUBLESHOOTING.md), so the boot self-test is timeout-bounded there and falls back to a software-triggered interrupt. Fully verified in Oracle VM VirtualBox (real hardware virtualization): self-test passes clean, `50 ticks elapsed via hardware irq0, 510ms since boot`, confirming end-to-end hardware interrupt delivery works and the timeout fix holds. GDT/IDT/page tables/heap/timer boot chain all verified in BIOS+UEFI QEMU and in Oracle VM VirtualBox; timer is the only one that behaves differently between the two (software-int fallback in this sandbox's QEMU/TCG, real hardware ticks in VirtualBox), which is expected and documented. Capabilities/IPC/scheduler below are still spec only, not implemented.

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
