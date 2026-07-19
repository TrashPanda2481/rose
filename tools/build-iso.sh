#!/usr/bin/env bash
# Builds the kernel and stages a bootable ISO (BIOS + UEFI) at repo root: rose.iso
set -euo pipefail
cd "$(dirname "$0")/.."

source "$HOME/.cargo/env" 2>/dev/null || true

echo "building kernel..."
(cd kernel && cargo build --release)

echo "staging iso root..."
rm -rf iso_root
mkdir -p iso_root/boot/limine iso_root/EFI/BOOT

cp kernel/target/x86_64-unknown-none/release/rose-kernel iso_root/boot/rose-kernel

cat > iso_root/boot/limine/limine.conf << 'EOF'
timeout: 0

/Rose
    protocol: limine
    kernel_path: boot():/boot/rose-kernel
EOF

cp boot/limine-bootloader/limine-bios.sys iso_root/boot/limine/
cp boot/limine-bootloader/limine-bios-cd.bin iso_root/boot/limine/
cp boot/limine-bootloader/limine-uefi-cd.bin iso_root/boot/limine/
cp boot/limine-bootloader/BOOTX64.EFI iso_root/EFI/BOOT/

echo "building iso..."
xorriso -as mkisofs -R -r -J -b boot/limine/limine-bios-cd.bin \
    -no-emul-boot -boot-load-size 4 -boot-info-table \
    --efi-boot boot/limine/limine-uefi-cd.bin \
    -efi-boot-part --efi-boot-image --protective-msdos-label \
    iso_root -o rose.iso

./boot/limine-bootloader/limine bios-install rose.iso

echo "rose.iso built."
