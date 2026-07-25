# Changelog: kernel core

Date, change, why. Newest first.

## 2026-07-25

- Boot verified in Oracle VM VirtualBox, second virtualizer required by the stable promotion checklist. Serial log matches QEMU exactly: `rose: boot ok`, `rose: base revision supported`, `rose: hello, hardware`.
- Two VirtualBox-specific issues hit and logged in `TROUBLESHOOTING.md`: VM created as a 32-bit guest type masked the long-mode CPUID bit (Limine panicked with "This CPU does not support 64-bit mode"), and raw-file serial capture failed with `VERR_INVALID_NAME` when pointed at a non-local drive.
- Promoted to `stable`.
- Memory bring-up started: kernel now requests and parses Limine's memory map (`MemmapRequest`), logs every region's base/length/type over serial, and totals usable memory. Verified on both BIOS and UEFI. First step toward the physical frame allocator; no allocator yet, this is just reading and printing what the bootloader reports.

## 2026-07-19

- v0.1 spec drafted: capability model, CSpace, object types, IPC format, address space model, boot handoff, scheduler policy.
- First code: kernel entry point, COM1 serial driver, panic handler. Boots via Limine (protocol requests + base revision check) on both BIOS and UEFI in QEMU.
- Milestone: "Boot to Hello, Rose in QEMU" (ground-zero step 2) reached. Serial output confirmed: `rose: boot ok`, `rose: base revision supported`, `rose: hello, hardware`.
- Smoke test script (`tools/smoke-test.sh`) added and passing: builds, boots headless, greps serial log for expected line.
- Five toolchain/boot issues hit and logged in `TROUBLESHOOTING.md`: custom json target needing an unstable cargo flag (switched to built-in `x86_64-unknown-none` target), limine crate import path, soft-float target-feature conflict, bootloader binary too old for the crate's protocol revision, OVMF `-bios` vs `-drive if=pflash` for UEFI testing.
