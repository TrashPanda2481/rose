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
2026-07-25  29207ce     Memory map parsing + physical frame allocator, verified on QEMU (BIOS+UEFI) and Oracle VM VirtualBox.
2026-07-25  9235f89     Kernel boots and reaches the smoke-test serial output on both QEMU (BIOS+UEFI) and Oracle VM VirtualBox.
```

## Why two branches and not more

Feature branches per core can exist temporarily during work but always merge back to main first, never straight to stable. Stable is a snapshot of "known good," not a place to develop.
