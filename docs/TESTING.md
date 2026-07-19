# Testing

Two virtualizers. Different jobs.

## QEMU: primary, automated

Every build gets tested here. Headless, scriptable, fast. This is the smoke test referenced in `BRANCHING.md`: boot the image, capture serial output, check for the expected line. No GUI, no manual steps, runs on every commit worth checking.

## Oracle VM VirtualBox: secondary, manual, gate for stable

Different firmware/BIOS implementation than QEMU's OVMF. A clean boot here means the kernel isn't just happening to work around a QEMU-specific quirk. Not run on every commit; run before a merge to `stable`, as a second opinion.

Setup: raw disk image or ISO, same one built for QEMU. Serial port can be redirected to a host file for log capture, same as QEMU, so the check is comparable.

## Real hardware

Not yet. Add a reference board/laptop here once QEMU + VirtualBox are both reliably green and there's a driver set narrow enough to bother.

## Stable promotion checklist (supersedes the short version in BRANCHING.md)

1. Builds clean.
2. Boots in QEMU, smoke test passes.
3. Boots in VirtualBox, same smoke-test line shows up in the serial log.
4. Merge logged in `BRANCHING.md`.
