# Troubleshooting — kernel core

Problems hit during dev, and how they were solved. Not bugs in the design — friction with toolchain, QEMU, hardware, build.

Format:

```
## Problem
Context (what you were doing when it happened):
Symptom:
Root cause:
Resolution:
```

## Custom JSON target needs an unstable cargo flag
Context: setting up the freestanding build target for the kernel.
Symptom: `.json` target specs require -Zjson-target-spec to be added to the cargo invocation, even for plain `cargo fetch`.
Root cause: recent nightly cargo gated raw `.json` target specs behind an unstable flag not exposed via `.cargo/config.toml`.
Resolution: dropped the custom json target, switched to the built-in `x86_64-unknown-none` target (`rustup target add x86_64-unknown-none`) and moved freestanding flags (no SSE, static relocation, kernel code model, linker script) into `[target.x86_64-unknown-none] rustflags` in `.cargo/config.toml`.

## limine crate import paths
Context: first main.rs using the `limine` crate for boot requests.
Symptom: `unresolved import limine::request::RequestsEndMarker` / `RequestsStartMarker`.
Root cause: current limine crate (0.6.5) re-exports these at the crate root, not under `request::`.
Resolution: `use limine::RequestsEndMarker;` / `use limine::RequestsStartMarker;` instead of going through the `request` module.

## soft-float target-feature conflict
Context: disabling SSE/FP for the freestanding target via rustflags.
Symptom: `target feature soft-float cannot be enabled with -Ctarget-feature: use a soft-float target instead`.
Root cause: `+soft-float` is a target-level property, not a feature toggle, on this target.
Resolution: dropped `+soft-float` from rustflags. Just `-sse` (and dropped `-mmx` too, unknown/unstable feature name) is enough for now; revisit if FP state ever needs to cross a context switch.

## Limine bootloader binary too old for the crate's base revision
Context: first QEMU boot attempt.
Symptom: kernel panics immediately with `assertion failed: BASE_REVISION.is_supported()`. Panic handler itself worked correctly and printed over serial, which is how this got caught.
Root cause: cloned the `v9.x-binary` branch of the limine-bootloader repo, which predates the boot protocol base revision the `limine` 0.6.5 crate requests.
Resolution: cloned `v11.x-binary` instead (latest binary branch), rebuilt `limine` deploy tool, rebuilt the ISO. Boot succeeded on both BIOS and UEFI after that.

## OVMF `-bios` flag fails on 4M firmware variant
Context: testing the UEFI boot path in QEMU.
Symptom: `qemu: could not load PC BIOS '/usr/share/OVMF/OVMF_CODE_4M.fd'`.
Root cause: the `-bios` flag expects a single combined firmware image; the installed OVMF package ships split 4M CODE/VARS variants meant for `-drive if=pflash`.
Resolution: use two `-drive if=pflash,format=raw,unit=0/1,...` lines (CODE read-only, VARS writable, VARS copied to a scratch file first since QEMU writes to it). See `tools/run-uefi.sh`.
