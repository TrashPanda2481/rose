# Changelog: Rose (project level)

Format: date, what changed, why. Newest first.

## 2026-07-25

- Kernel boot verified in Oracle VM VirtualBox, matching QEMU serial output. Promoted `main` to `stable` (see `BRANCHING.md` merge log).
- Memory bring-up: kernel parses Limine's memory map and logs every region over serial. Physical frame allocator built on top (free-list, HHDM-addressed, self-test on boot). Verified on QEMU BIOS+UEFI and Oracle VM VirtualBox. Promoted `main` to `stable` again (see `BRANCHING.md` merge log).
- GDT/IDT and kernel-owned page tables added (all 32 exception vectors, double-fault IST stack, W^X kernel image mapping, HHDM remap, map/unmap self-tests). Verified on QEMU BIOS+UEFI only so far; VirtualBox verification deliberately deferred, both still on `main` only, not yet in `stable`.
- Kernel heap added (free-list first-fit, 1MiB fixed region, backs `extern crate alloc`). Self-test exercises Box and a growing Vec, confirms memory returns on drop. Verified on QEMU BIOS+UEFI only; still on `main` only, not yet in `stable`.
- PIC + PIT timer added (IRQ0 remapped to vector 32, 100Hz). Hit a real bug: hardware IRQ0 never reaches the CPU in this sandbox's QEMU/TCG despite verified-correct PIC/PIT programming; IDT dispatch/EOI/tick-counting confirmed correct via software-triggered interrupt instead. Left open in `BUGS.md` pending a non-sandboxed repro; boot self-test now timeout-bounded instead of hanging. Verified on QEMU BIOS+UEFI only; still on `main` only, not yet in `stable`.

## 2026-07-19

- Added `docs/VISION.md`: end-state goal (real-time usage understanding, adapts to the end user, native successor to Compass) and the bespoke-vs-standard heuristic. Added deferred `adaptive` core stub (README/CHANGELOG/BUGS/TROUBLESHOOTING, no design/code yet).
- Kernel boots. Serial hello-world confirmed on BIOS and UEFI in QEMU. First hardware-interface milestone reached.
- Repo pushed to GitHub, private, `main` + `stable` branches. `stable` still at skeleton-only until this boot is verified in VirtualBox too (see docs/TESTING.md promotion checklist).
- Named the project Rose.
- Drafted kernel core invariants v0.1: capability model, IPC format, address space model, boot handoff, scheduler policy. See `docs/cores/kernel/README.md`.
- Set documentation structure: per-core README/CHANGELOG/BUGS/TROUBLESHOOTING, plus this project-level file.
