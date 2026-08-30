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
2026-08-30  b660673     Framebuffer console mirrors serial output onto the screen (10 self-test functions + 9 usermode syscall-hook init sites switched from Serial to console::Dual). Confirmed in Oracle VM VirtualBox in two steps: a screenshot showing the console rendering correctly on real hardware through the full self-test sequence, then a serial log containing the exact smoke-test line (`rose: hello, hardware`) plus every other checked line matching the current build verbatim. Re-verified `cargo clean && cargo build` (zero warnings) and a fresh QEMU smoke-test pass on this exact commit before merging. Also closes the previously-open Oracle VM VirtualBox confirmation gap on the 2026-08-15 Endpoint IPC increment 2 entry (Call/Reply full round trip and cross-family aggregate now confirmed under real hardware preemption).
2026-08-09  a34219e     Endpoint IPC v0.1 increment 1: Endpoint object, blocking Send/Receive (SYS_ENDPOINT_SEND/SYS_ENDPOINT_RECEIVE). Receive-side delivery confirmed on QEMU BIOS+UEFI; full round-trip (Send blocks, wakes, resumes, aggregate report) confirmed in Oracle VM VirtualBox via real hardware timer preemption, the QEMU/TCG truncation there is the pre-existing IRQ0 sandbox limitation, not a defect.
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
