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
