#!/usr/bin/env bash
# Runs rose.iso under QEMU with a legacy BIOS boot path. Serial to stdio.
set -euo pipefail
cd "$(dirname "$0")/.."

qemu-system-x86_64 \
    -cdrom rose.iso \
    -serial stdio \
    -m 256M \
    "$@"
