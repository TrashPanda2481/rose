# Branching

Two branches. That's it, no more until there's a real reason.

## main
Active work. Can be broken between commits. This is where cores get built, invariants get revised, code gets written and fixed.

## stable
Only receives merges from main, never worked on directly. Full promotion checklist lives in `TESTING.md`; short version: builds clean, boots in QEMU with smoke test passing, boots in VirtualBox too.

If that's not true, it doesn't go to stable. No exceptions for "it's basically done."

## Merge log

Track every main → stable merge here. Newest first.

```
Date        Commit      What it proves works
----        ------      ---------------------
2026-08-09  a683694     Security audit fixes: Frame-Map ring-3 privilege escalation, W^X bypass, huge-page walk assert, scheduler lock ordering (deadlock risk), Configure commit-before-verify, Untyped rights check. Verified end to end on QEMU (BIOS+UEFI) and Oracle VM VirtualBox.
2026-08-08  a7750e9     PCI enumeration v0.1, Frame-Map syscall (fixes Configure page-fault bug), and per-task kernel entry stacks (fixes post-syscall page-fault bug), verified end to end on QEMU (BIOS+UEFI) and Oracle VM VirtualBox.
2026-08-01  44903de     Retype extended to AddressSpace and Thread (six-step ring-3 self-test), verified end to end on QEMU (BIOS) and Oracle VM VirtualBox.
2026-08-01  93a2fc5     Capabilities/CSpace v0.1, CSpace syscalls, Untyped/Retype v0.1, and the task 0 address-space sync fix, verified end to end on QEMU (BIOS+UEFI) and Oracle VM VirtualBox.
2026-07-25  cd93663     PIC remap + PIT timer (IRQ0 on vector 32). Timer self-test timeout bug fixed. VirtualBox boot confirms hardware IRQ0 delivery works outside this dev sandbox's QEMU/TCG.
2026-07-25  4189edf     GDT/IDT, kernel-owned page tables, kernel heap. Verified on QEMU (BIOS+UEFI) and Oracle VM VirtualBox, serial logs match exactly.
2026-07-25  29207ce     Memory map parsing + physical frame allocator, verified on QEMU (BIOS+UEFI) and Oracle VM VirtualBox.
2026-07-25  9235f89     Kernel boots and reaches the smoke-test serial output on both QEMU (BIOS+UEFI) and Oracle VM VirtualBox.
```

## Why two branches and not more

Feature branches per core can exist temporarily during work but always merge back to main first, never straight to stable. Stable is a snapshot of "known good," not a place to develop.
