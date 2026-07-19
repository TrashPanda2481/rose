# Branching

Two branches. That's it, no more until there's a real reason.

## main
Active work. Can be broken between commits. This is where cores get built, invariants get revised, code gets written and fixed.

## stable
Only receives merges from main, never worked on directly. A merge to stable is only allowed when:

1. It builds clean.
2. It boots in QEMU.
3. The smoke test (serial log check, see `tools/`) passes.

If those three aren't true, it doesn't go to stable. No exceptions for "it's basically done."

## Merge log

Track every main → stable merge here. Newest first.

```
Date        Commit      What it proves works
----        ------      ---------------------
(none yet)
```

## Why two branches and not more

Feature branches per core can exist temporarily during work but always merge back to main first, never straight to stable. Stable is a snapshot of "known good," not a place to develop.
