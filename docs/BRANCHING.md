# Branching

Two branches. That's it, no more until there's a real reason.

## main
Active work. Can be broken between commits. This is where cores get built, invariants get revised, code gets written and fixed.

## stable
Only receives merges from main, never worked on directly. Full promotion checklist lives in `TESTING.md` — short version: builds clean, boots in QEMU with smoke test passing, boots in VirtualBox too.

If that's not true, it doesn't go to stable. No exceptions for "it's basically done."

## Merge log

Track every main → stable merge here. Newest first.

```
Date        Commit      What it proves works
----        ------      ---------------------
(none yet)
```

## Why two branches and not more

Feature branches per core can exist temporarily during work but always merge back to main first, never straight to stable. Stable is a snapshot of "known good," not a place to develop.
