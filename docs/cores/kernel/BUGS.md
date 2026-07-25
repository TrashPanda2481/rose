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

## [FIXED] spawned tasks page-faulted at rip=0 on their first switch-in
Date found: 2026-07-25
Date fixed: 2026-07-25
Symptom: first boot of the scheduler self-test paniced with `EXCEPTION vector=14 (page fault) error_code=0x10 rip=0x0000000000000000 faulting address (cr2)=0x0`, right after the first `yield_now()` call switched away from task 0.
Cause: `spawn()` fabricates a fake saved-context frame on the new task's stack so `context_switch`'s pop sequence resumes into `task_trampoline` instead of a real prior call. The frame values were written with `frame.iter().rev()`, which put `task_trampoline` (meant to be the return address, popped last) at the lowest address (popped first, into r15) and the r15 zero-word at the highest address (meant to be the return address). `context_switch` ended up popping garbage into the callee-saved registers and then `ret`-ing to address 0.
Fix: write the frame top-down in the order it's declared (no reversal), so `task_trampoline` lands at the highest address (last popped, used by `ret`) and each register value lands where its matching `pop` expects it.
Commit/ref: kernel: scheduler (round-robin, context switching)

## [FIXED] scheduler self-test hung after first task finished
Date found: 2026-07-25
Date fixed: 2026-07-25
Symptom: after fixing the frame-layout bug above, boot got past the first switch but then hung indefinitely after `rose: timer self-test`, no further serial output, no panic.
Cause: `task_a`/`task_b` parked in `loop { spin_loop() }` after finishing their 10 iterations, without ever calling `yield_now()` again. Round-robin selection always advances to `(current + 1) % len` regardless of whether the task actually being switched to will ever yield back; once either task parked, control passed to it once and then never rotated again, freezing every other task in the ring, including task 0's own `while TASKS_DONE < 2` wait loop.
Fix: changed both parking loops to `loop { scheduler::yield_now(); }` so a finished task keeps participating in the rotation instead of dead-ending it. Round-robin still has no concept of "done"/skippable tasks; a real scheduler needs one before this class of bug stops being possible by construction, tracked as a known gap for v0.2.
Commit/ref: kernel: scheduler (round-robin, context switching)

## [FIXED] scheduler panic on real hardware: modulo by zero in tick()
Date found: 2026-07-25
Date fixed: 2026-07-25
Symptom: QEMU (BIOS/UEFI, this sandbox) booted clean through the scheduler self-test every time. A VirtualBox boot on real hardware panicked earlier, right after "rose: timer: first irq0 tick received": `panicked at src/scheduler.rs:142:26: attempt to calculate the remainder with a divisor of zero`.
Cause: `idt.rs` calls `scheduler::tick()` unconditionally on every timer IRQ, but `kernel_main` didn't call `scheduler::init()` (which registers task 0) until `scheduler_selftest()`, well after `pic::init()`/`timer::init()` already executed `sti`. In this sandbox's QEMU/TCG, IRQ0 never actually reaches the CPU (see TROUBLESHOOTING.md), so the empty-queue window was never exercised. On VirtualBox, real hardware delivers IRQ0 immediately once IF goes high, so ticks landed on an empty `tasks` Vec; the 5th one reached `tick()`'s round-robin step and computed `(0 + 1) % 0`.
Fix: moved the `scheduler::init()` call to immediately before `pic::init()`, so task 0 is always registered before interrupts are ever enabled; removed the now-duplicate `init()` call from `scheduler_selftest()`. Also added a defensive `if tasks.is_empty() { return }` guard at the top of `tick()` itself, since this path is reachable from a real hardware interrupt whose timing this kernel doesn't control, not just from code that polls something on its own schedule.
Commit/ref: kernel: scheduler (round-robin, context switching), boot-order fix
