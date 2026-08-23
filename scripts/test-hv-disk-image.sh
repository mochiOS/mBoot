#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
IMAGE=${HV_DISK_IMAGE:?HV_DISK_IMAGE is required}
CONFIG=${HV_CONFIG:?HV_CONFIG is required}
QEMU=${QEMU:-qemu-system-x86_64}
ACCEL=${HV_ACCEL:-kvm}
CPU=${HV_CPU:-host}
TIMEOUT_SECONDS=${HV_TIMEOUT_SECONDS:-30}
OVMF_CODE="$ROOT/board/mboot/rootfs-overlay/usr/share/mboot/OVMF_CODE_4M.fd"
OVMF_VARS="$ROOT/board/mboot/rootfs-overlay/usr/share/mboot/OVMF_VARS_4M.fd"
DOMAIN_COUNT=$($ROOT/scripts/hv-config-value.pl "$CONFIG" domain_count)

test -s "$IMAGE" || { echo "missing image: $IMAGE" >&2; exit 1; }
for file in "$OVMF_CODE" "$OVMF_VARS"; do
    test -s "$file" || { echo "missing firmware: $file" >&2; exit 1; }
done

WORK=$(mktemp -d)
QEMU_PID=
cleanup() {
    if [[ -n $QEMU_PID ]] && kill -0 "$QEMU_PID" 2>/dev/null; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
    rm -rf -- "$WORK"
}
trap cleanup EXIT
cp "$OVMF_VARS" "$WORK/OVMF_VARS.fd"
cp --sparse=always "$IMAGE" "$WORK/mochiOS.iso"

"$QEMU" \
    -accel "$ACCEL" \
    -cpu "$CPU" \
    -machine q35 \
    -boot menu=off,strict=on \
    -smp 1 \
    -m 512 \
    -drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
    -drive "if=pflash,format=raw,file=$WORK/OVMF_VARS.fd" \
    -drive "if=none,id=disk,format=raw,file=$WORK/mochiOS.iso" \
    -device virtio-blk-pci,drive=disk,bootindex=1 \
    -display none \
    -monitor none \
    -serial "file:$WORK/serial.log" \
    -net none \
    -no-reboot \
    -no-shutdown &
QEMU_PID=$!

expected="[mBoot-HV] bootstrap complete; $DOMAIN_COUNT Domains entered and stopped cleanly"
for ((attempt = 0; attempt < TIMEOUT_SECONDS * 10; attempt++)); do
    grep -Fq "$expected" "$WORK/serial.log" 2>/dev/null && break
    kill -0 "$QEMU_PID" 2>/dev/null || break
    sleep 0.1
done
grep -Fq "$expected" "$WORK/serial.log" || {
    sed -n '1,200p' "$WORK/serial.log" >&2
    echo "hypervisor image did not complete bootstrap" >&2
    exit 1
}
grep -E '\[mBoot-HV\]|\[mnu Domain' "$WORK/serial.log"
echo 'test-hv-disk-image: PASS'
