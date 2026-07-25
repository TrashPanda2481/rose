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

## [FIXED] timer self-test timeout used a frequency floor instead of a ceiling, aborted early on fast hosts
Date found: 2026-07-25
Date fixed: 2026-07-25
Symptom: a VirtualBox boot showed the one-time "first irq0 tick received" log firing and the tick counter reaching 21, proving real hardware IRQ0 delivery works there; despite that, `timer_selftest` still printed "no hardware irq0 within ~2s" and fell back to the software interrupt path, contradicting the ticks that were visibly happening.
Cause: `timer_selftest`'s RDTSC-based timeout computed `cycle_budget = MIN_ASSUMED_HZ * WAIT_SECONDS` using a 500MHz floor, intending to bound the wait to "about 2 seconds." That's backwards: `real_elapsed_seconds = cycle_budget / actual_hz`, so a floor only bounds the wait correctly when the real CPU is slower than the floor. On a real multi-GHz host (as in VirtualBox, running on real host hardware instead of QEMU/TCG emulation), the budget of 1 billion cycles elapsed in a couple hundred milliseconds, not 2 seconds, so the test gave up almost immediately, before the 100Hz timer could reach 50 ticks, and misreported a working timer as absent.
Fix: swapped the constant to `MAX_ASSUMED_HZ = 8_000_000_000` (an assumed ceiling, not floor) so `cycle_budget / actual_hz >= WAIT_SECONDS` holds for any realistic host clock speed. This is a self-test bug only; the underlying PIC/PIT/IDT timer implementation was already correct, as proven by the tick counter advancing on VirtualBox before the fix. Re-verified after the fix with a second VirtualBox boot: self-test now passes clean, `50 ticks elapsed via hardware irq0, 510ms since boot`.
Commit/ref: kernel: timer (PIC + PIT, IRQ0-gated self-test with software-int fallback)
