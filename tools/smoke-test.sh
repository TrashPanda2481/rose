#!/usr/bin/env bash
# The smoke test referenced in docs/BRANCHING.md and docs/TESTING.md.
# Builds, boots headless under QEMU (BIOS path), checks for the expected
# serial line. Exit 0 = pass, exit 1 = fail.
set -uo pipefail
cd "$(dirname "$0")/.."

EXPECTED_LINE="rose: hello, hardware"
LOGFILE=".scratch/smoke.log"
mkdir -p .scratch

./tools/build-iso.sh || { echo "FAIL: build error"; exit 1; }

rm -f "$LOGFILE"
timeout 15 qemu-system-x86_64 \
    -cdrom rose.iso \
    -serial file:"$LOGFILE" \
    -display none \
    -no-reboot \
    -no-shutdown \
    -m 256M &
QEMU_PID=$!
for remaining in 8 7 6 5 4 3 2 1; do
    printf "\rwaiting for boot... %ds remaining " "$remaining"
    sleep 1
done
printf "\rwaiting for boot... done              \n"
kill -9 "$QEMU_PID" 2>/dev/null
wait 2>/dev/null

if grep -qF "$EXPECTED_LINE" "$LOGFILE" 2>/dev/null; then
    echo "PASS: found '$EXPECTED_LINE'"
    cat "$LOGFILE"
    exit 0
else
    echo "FAIL: did not find '$EXPECTED_LINE'"
    echo "--- serial log ---"
    cat "$LOGFILE" 2>/dev/null
    exit 1
fi
