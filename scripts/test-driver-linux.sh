#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
IMAGE=${HV_DISK_IMAGE:-$ROOT/output/driver-linux.iso}
QEMU=${QEMU:-qemu-system-x86_64}
ACCEL=${HV_ACCEL:-auto}
if [[ $ACCEL == auto ]]; then
    if [[ -r /dev/kvm && -w /dev/kvm ]]; then
        ACCEL=kvm
    else
        ACCEL=tcg
    fi
fi
if [[ $ACCEL == kvm ]]; then
    CPU=${HV_CPU:-host}
    TIMEOUT_SECONDS=${HV_TIMEOUT_SECONDS:-45}
else
    CPU=${HV_CPU:-max}
    TIMEOUT_SECONDS=${HV_TIMEOUT_SECONDS:-240}
fi
OVMF_CODE="$ROOT/firmware/OVMF_CODE_4M.fd"
OVMF_VARS="$ROOT/firmware/OVMF_VARS_4M.fd"

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
cp --sparse=always "$IMAGE" "$WORK/driver-linux.iso"

"$QEMU" \
    -accel "$ACCEL" \
    -cpu "$CPU" \
    -machine q35 \
    -boot menu=off,strict=on \
    -smp 1 \
    -m 512 \
    -drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
    -drive "if=pflash,format=raw,file=$WORK/OVMF_VARS.fd" \
    -drive "if=none,id=disk,format=raw,file=$WORK/driver-linux.iso" \
    -device virtio-blk-pci,drive=disk,bootindex=1 \
    -display none \
    -monitor none \
    -serial "file:$WORK/serial.log" \
    -net none \
    -no-reboot \
    -no-shutdown &
QEMU_PID=$!

for ((attempt = 0; attempt < TIMEOUT_SECONDS * 10; attempt++)); do
    if grep -Fq 'Driver Linux OK' "$WORK/serial.log" 2>/dev/null; then
        grep -E '\[mBoot\]|Driver Linux OK|Linux version' "$WORK/serial.log"
        echo 'test-driver-linux: PASS'
        exit 0
    fi
    kill -0 "$QEMU_PID" 2>/dev/null || break
    sleep 0.1
done

sed -n '1,240p' "$WORK/serial.log" >&2
echo 'test-driver-linux: Driver Linux did not reach init' >&2
exit 1
