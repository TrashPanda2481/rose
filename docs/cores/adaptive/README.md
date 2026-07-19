# Adaptive core

Status: deferred. No spec, no code. Placeholder so the goal isn't forgotten.

## What this is

The real-time usage/intent layer described in `docs/VISION.md`. The native successor to Compass (the guidance app from the Meridian-OS project) — same idea, but as an OS-level capability instead of a userspace tool sitting on top of a generic system.

## Why there's nothing here yet

This core observes and acts on other components. It can't be designed sensibly before those components exist:

- Needs the capability model and IPC working (ground-zero steps 3-4)
- Needs a scheduler and real running components
- Needs enough of a component graph that "usage" has something to refer to

## Next step

Once the kernel core reaches a working scheduler + IPC round-trip, come back here and start a real spec. Until then, this file is just a marker.
