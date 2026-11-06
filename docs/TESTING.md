# Testing

Two virtualizers. Different jobs.

## QEMU: primary, automated

Every build gets tested here. Headless, scriptable, fast. This is the smoke test referenced in `BRANCHING.md`: boot the image, capture serial output, check for the expected line. No GUI, no manual steps, runs on every commit worth checking.

## Oracle VM VirtualBox: secondary, manual, gate for stable

Different firmware/BIOS implementation than QEMU's OVMF. A clean boot here means the kernel isn't just happening to work around a QEMU-specific quirk. Not run on every commit; run before a merge to `stable`, as a second opinion.

Setup: attach the same `rose.iso` built for QEMU to the optical drive. The serial port is redirected to a host file for log capture, same as QEMU, so the check is comparable; point it at `Log/rose-serial.log`. The `Log/` dir is tracked via `.gitkeep`; its `*.log` contents are gitignored.

### VM settings

| Pane | Setting | Value |
|---|---|---|
| General | Type / Version | Other / Other-Unknown (64-bit) |
| System > Motherboard | Base Memory | 256 MB minimum (QEMU uses `-m 256M`) |
| System > Motherboard | Boot Order | Optical first |
| System > Motherboard | Chipset | PIIX3 |
| System > Motherboard | Firmware | BIOS (EFI off) |
| System > Processor | Processors | 1 |
| System > Processor | Enable PAE/NX | ON (required, see below) |
| System > Processor | Nested VT-x/AMD-V | OFF |
| System > Acceleration | Paravirtualization | None |
| System > Acceleration | Nested Paging | ON |
| Display > Screen | Video Memory | 16 MB |
| Display > Screen | Graphics Controller | VBoxVGA |
| Display > Screen | 3D Acceleration | OFF |
| Storage | Optical drive | `rose.iso` (repo root) |
| Serial Ports > Port 1 | Port | COM1 (I/O 0x3F8, IRQ 4) |
| Serial Ports > Port 1 | Mode / Path | Raw File, `Log/rose-serial.log` |

PAE/NX is not optional. The kernel enables `EFER.NXE` and sets the NX bit (bit 63) on every data, heap, and HHDM page to enforce W^X from the first kernel-owned page table (`kernel/src/paging.rs`). With PAE/NX disabled the NX bit becomes a reserved bit and paging faults on boot.

BIOS over EFI is deliberate. The ISO is hybrid, but VirtualBox's EFI is unreliable booting from CD. VirtualBox's legacy BIOS is still a different implementation from QEMU's SeaBIOS, which is the whole point of the second opinion. Test the UEFI path with `tools/run-uefi.sh` (OVMF) instead.

A clean boot shows both smoke-test lines in the serial log:

```
grep -E "rose: hello, hardware|rose: root task: handoff ok" Log/rose-serial.log
```

## Real hardware

Not yet. Add a reference board/laptop here once QEMU + VirtualBox are both reliably green and there's a driver set narrow enough to bother.

## Stable promotion checklist (supersedes the short version in BRANCHING.md)

1. Builds clean.
2. Boots in QEMU, smoke test passes.
3. Boots in VirtualBox, same smoke-test line shows up in the serial log.
4. Merge logged in `BRANCHING.md`.
