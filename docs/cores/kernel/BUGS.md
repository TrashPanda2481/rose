# Bugs: kernel core

One entry per bug. Keep closed ones, don't delete; they're history.

Format:

```
## [OPEN|FIXED] short title
Date found:
Date fixed:
Symptom:
Cause:
Fix:
Commit/ref:
```

## [FIXED] double fault immediately after CR3 switch to kernel-owned page tables
Date found: 2026-07-25
Date fixed: 2026-07-25
Symptom: booting past "rose: page tables loaded, kernel-owned" never happened; serial log showed vector=8 (double fault) with rip pointing a few instructions after the `mov cr3` in paging::switch_to, then halt.
Cause: paging::build only mapped the HHDM range for MEMMAP_USABLE regions. Limine's default 64KiB boot stack (no StackSizeRequest sent) is allocated out of MEMMAP_BOOTLOADER_RECLAIMABLE memory, and that stack is still in use the instant CR3 changes. First stack access after the switch (a spilled local a few instructions later) had no mapping in the new tables, page-faulted, and the page fault itself couldn't be delivered cleanly (stack still bad), producing a double fault.
Fix: also map MEMMAP_BOOTLOADER_RECLAIMABLE through HHDM alongside MEMMAP_USABLE in paging::build. Does not change what the frame allocator hands out, only what's mapped.
Commit/ref: kernel: page tables

## [OPEN] IRQ0 never dispatched to CPU under this sandbox's QEMU/TCG despite correct PIC/PIT programming
Date found: 2026-07-25
Date fixed: (open)
Symptom: after `pic::init()` and `timer::init()` (8259 remap + 8253/8254 PIT programmed for 100Hz + `sti`), the timer self-test never observes a tick; the one-time "first irq0 tick received" log never fires. No hang or crash, just silence.
Cause: not fully isolated. Byte-verified via objdump that the PIC remap (ICW1-4, IRQ0-7 to vectors 32-39) and PIT programming (command 0x36, divisor 11931) are correct; runtime probes confirm RFLAGS.IF=1 and PIC master IMR=0xfe (only IRQ0 unmasked). PIC master IRR reads back as 0x01 (IRQ0 pending) and stays pinned there indefinitely; the CPU never acknowledges it. A software `int 0x20` executed from the same code path dispatches correctly and increments the tick counter immediately, proving the IDT vector 32 stub, `rose_exception_handler` dispatch, `timer::on_tick`, and `pic::send_eoi` are all wired correctly end to end. QEMU's own `-d int` interrupt trace shows zero interrupt-servicing events during the entire timer-active window. Reproduces identically under BIOS (SeaBIOS) and UEFI (OVMF) boot paths, and with `-accel tcg` explicit, so it isn't firmware-specific or accelerator-specific; most likely a quirk of this sandbox's QEMU 10.2.1/TCG PIC-to-CPU interrupt injection path, not a kernel bug, but not proven.
Fix: none yet. Worked around at the self-test level: `timer_selftest` now bounds its wait with an RDTSC cycle budget (~2s at a conservative 500MHz floor) instead of waiting unboundedly, and falls back to a software `int 0x20` to at least confirm dispatch if no hardware tick arrives in time. Boot no longer hangs either way. Real fix requires reproducing outside this sandbox (bare metal or a host with KVM) to tell apart "QEMU/TCG sandbox quirk" from "kernel-side bug that happens to also pass a software-triggered vector."
Commit/ref: kernel: timer (PIC + PIT, IRQ0-gated self-test with software-int fallback)
