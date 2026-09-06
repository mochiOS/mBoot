#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
QEMU=${QEMU:-qemu-system-x86_64}
ACCEL=${HV_ACCEL:-kvm}
CPU=${HV_CPU:-host}
TIMEOUT_SECONDS=${HV_TIMEOUT_SECONDS:-30}
DISK=${HV_DISK_IMAGE:-"$ROOT/output/watchdog/mochiOS.img"}
OVMF_CODE="$ROOT/firmware/OVMF_CODE_4M.fd"
OVMF_VARS="$ROOT/firmware/OVMF_VARS_4M.fd"
ARTIFACTS=${HV_ARTIFACTS:-"$ROOT/output/watchdog-proof"}
SERIAL="$ARTIFACTS/serial.log"
VARS="$ARTIFACTS/OVMF_VARS.fd"

for command in "$QEMU"; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "test-watchdog: missing command: $command" >&2
        exit 1
    }
done
for path in "$DISK" "$OVMF_CODE" "$OVMF_VARS"; do
    test -s "$path" || {
        echo "test-watchdog: missing input: $path" >&2
        exit 1
    }
done

mkdir -p "$ARTIFACTS"
cp "$OVMF_VARS" "$VARS"
: > "$SERIAL"

"$QEMU" \
    -accel "$ACCEL" \
    -cpu "$CPU" \
    -machine q35 \
    -boot menu=off,strict=on \
    -smp 1 \
    -m 512 \
    -drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
    -drive "if=pflash,format=raw,file=$VARS" \
    -drive "if=none,id=osdisk,format=raw,file=$DISK" \
    -device virtio-blk-pci,drive=osdisk,bootindex=1 \
    -device i6300esb \
    -action watchdog=reset \
    -display none \
    -monitor none \
    -serial "file:$SERIAL" \
    -net none \
    -no-shutdown &
QEMU_PID=$!

cleanup() {
    if kill -0 "$QEMU_PID" 2>/dev/null; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

passed=0
for ((attempt = 0; attempt < TIMEOUT_SECONDS * 10; attempt++)); do
    boots=$(grep -Fc '[mBoot] starting independent hypervisor' "$SERIAL" 2>/dev/null || true)
    if [[ $boots -ge 2 ]] \
        && grep -Fq 'hardware watchdog active: Intel 6300ESB timeout=4s' "$SERIAL" \
        && grep -Fq 'hardware watchdog heartbeat accepted from System Domain 1' "$SERIAL"; then
        passed=1
        break
    fi
    kill -0 "$QEMU_PID" 2>/dev/null || break
    sleep 0.1
done

if [[ $passed -ne 1 ]]; then
    sed -n '1,240p' "$SERIAL" >&2
    echo 'test-watchdog: watchdog did not reset and boot the machine again' >&2
    exit 1
fi

cat > "$ARTIFACTS/result.json" <<EOF
{
  "test": "mboot-hardware-watchdog-reset",
  "watchdog": "Intel 6300ESB",
  "timeout_seconds": 4,
  "boots_observed": $boots,
  "result": "PASS"
}
EOF

echo "test-watchdog: hardware reset PASS ($boots boots observed)"
echo "test-watchdog: serial log: $SERIAL"
echo "test-watchdog: result: $ARTIFACTS/result.json"
