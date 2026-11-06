#!/usr/bin/env bash
# The smoke test referenced in docs/BRANCHING.md and docs/TESTING.md.
# Builds, boots headless under QEMU (BIOS path), checks for the expected
# serial line. Exit 0 = pass, exit 1 = fail.
set -uo pipefail
cd "$(dirname "$0")/.."

EXPECTED_LINE="rose: hello, hardware"
# Phase 1: also require the root-task handoff success line. EXPECTED_LINE
# stays the primary proof of life (it prints before paging); ROOT_LINE
# proves the boot handoff ran end to end.
ROOT_LINE="rose: root task: handoff ok"
LOGFILE=".scratch/smoke.log"
mkdir -p .scratch

./tools/build-iso.sh || { echo "FAIL: build error"; exit 1; }

rm -f "$LOGFILE"
timeout 60 qemu-system-x86_64 \
    -cdrom rose.iso \
    -serial file:"$LOGFILE" \
    -display none \
    -no-reboot \
    -no-shutdown \
    -m 256M &
QEMU_PID=$!
# Poll the serial log and stop as soon as both success lines appear,
# waiting up to MAX_WAIT seconds otherwise. The timer self-test
# busy-waits on an RDTSC budget with no real IRQ0 under QEMU/TCG, so the
# root-task line can take ~20s to appear here; it is near-instant on real
# hardware / VirtualBox. Polling exits early rather than always paying
# the worst case.
MAX_WAIT=50
for i in $(seq 1 "$MAX_WAIT"); do
    if grep -qF "$EXPECTED_LINE" "$LOGFILE" 2>/dev/null \
        && grep -qF "$ROOT_LINE" "$LOGFILE" 2>/dev/null; then
        break
    fi
    printf "\rwaiting for boot... %ds " "$i"
    sleep 1
done
printf "\rwaiting for boot... done        \n"
kill -9 "$QEMU_PID" 2>/dev/null
wait 2>/dev/null

if grep -qF "$EXPECTED_LINE" "$LOGFILE" 2>/dev/null && grep -qF "$ROOT_LINE" "$LOGFILE" 2>/dev/null; then
    echo "PASS: found '$EXPECTED_LINE' and '$ROOT_LINE'"
    cat "$LOGFILE"
    exit 0
else
    echo "FAIL: expected both '$EXPECTED_LINE' and '$ROOT_LINE'"
    echo "--- serial log ---"
    cat "$LOGFILE" 2>/dev/null
    exit 1
fi
