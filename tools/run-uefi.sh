#!/usr/bin/env bash
# Runs rose.iso under QEMU with OVMF (UEFI). Serial to stdio.
# OVMF vars get copied to a scratch file since QEMU writes to it at runtime.
set -euo pipefail
cd "$(dirname "$0")/.."

OVMF_CODE_SRC="${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}"
OVMF_VARS_SRC="${OVMF_VARS:-/usr/share/OVMF/OVMF_VARS_4M.fd}"

mkdir -p .scratch
cp "$OVMF_VARS_SRC" .scratch/OVMF_VARS.fd

qemu-system-x86_64 \
    -cdrom rose.iso \
    -drive if=pflash,format=raw,unit=0,file="$OVMF_CODE_SRC",readonly=on \
    -drive if=pflash,format=raw,unit=1,file=.scratch/OVMF_VARS.fd \
    -serial stdio \
    -m 256M \
    "$@"
