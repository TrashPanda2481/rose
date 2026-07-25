# Changelog: Rose (project level)

Format: date, what changed, why. Newest first.

## 2026-07-25

- Kernel boot verified in Oracle VM VirtualBox, matching QEMU serial output. Promoted `main` to `stable` (see `BRANCHING.md` merge log).

## 2026-07-19

- Added `docs/VISION.md`: end-state goal (real-time usage understanding, adapts to the end user, native successor to Compass) and the bespoke-vs-standard heuristic. Added deferred `adaptive` core stub (README/CHANGELOG/BUGS/TROUBLESHOOTING, no design/code yet).
- Kernel boots. Serial hello-world confirmed on BIOS and UEFI in QEMU. First hardware-interface milestone reached.
- Repo pushed to GitHub, private, `main` + `stable` branches. `stable` still at skeleton-only until this boot is verified in VirtualBox too (see docs/TESTING.md promotion checklist).
- Named the project Rose.
- Drafted kernel core invariants v0.1: capability model, IPC format, address space model, boot handoff, scheduler policy. See `docs/cores/kernel/README.md`.
- Set documentation structure: per-core README/CHANGELOG/BUGS/TROUBLESHOOTING, plus this project-level file.
