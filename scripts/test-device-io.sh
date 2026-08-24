#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
MNU_DIR=${MNU_DIR:-"$ROOT/../core"}
QEMU=${QEMU:-qemu-system-x86_64}
ACCEL=${HV_ACCEL:-tcg}
CPU=${HV_CPU:-EPYC}
TIMEOUT_SECONDS=${HV_TIMEOUT_SECONDS:-60}
TRACE_EVENTS=${HV_TRACE_EVENTS:-}
CONFIG="$ROOT/config/qemu-device-io.toml"
OVMF_CODE="$ROOT/firmware/OVMF_CODE_4M.fd"
OVMF_VARS="$ROOT/firmware/OVMF_VARS_4M.fd"

for command in "$QEMU" dd grep make mktemp printf truncate; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "test-device-io: missing command: $command" >&2
        exit 1
    }
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

BOOT_IMAGE="$WORK/mochiOS.iso"
TEST_DISK="$WORK/virtio-test.img"
SERIAL="$WORK/serial.log"
TRACE="$WORK/qemu.trace"
make -C "$ROOT" image \
    CONFIG="$CONFIG" \
    IMAGE="$BOOT_IMAGE" \
    MNU_DIR="$MNU_DIR" \
    CARGO="${MBOOT_HOST_CARGO:-$(command -v cargo)}" \
    RUSTC="${MBOOT_HOST_RUSTC:-$(command -v rustc)}"

truncate -s 1M "$TEST_DISK"
printf 'mochiOS virtio-blk DMA test\n' \
    | dd of="$TEST_DISK" bs=1 conv=notrunc status=none
cp "$OVMF_VARS" "$WORK/OVMF_VARS.fd"

QEMU_TRACE_ARGS=()
if [[ -n $TRACE_EVENTS ]]; then
    QEMU_TRACE_ARGS=(-trace "enable=$TRACE_EVENTS,file=$TRACE")
fi

"$QEMU" \
    -accel "$ACCEL" \
    -cpu "$CPU" \
    -machine q35 \
    -boot menu=off,strict=on \
    -smp 1 \
    -m 512 \
    -drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
    -drive "if=pflash,format=raw,file=$WORK/OVMF_VARS.fd" \
    -drive "if=none,id=boot,format=raw,file=$BOOT_IMAGE" \
    -device virtio-blk-pci,drive=boot,addr=0x2,bootindex=1 \
    -drive "if=none,id=test,format=raw,file=$TEST_DISK" \
    -device virtio-blk-pci,drive=test,addr=0x3,disable-legacy=on,iommu_platform=on \
    -device amd-iommu,dma-remap=on \
    -display none \
    -monitor none \
    -serial "file:$SERIAL" \
    -net none \
    -no-reboot \
    -no-shutdown \
    "${QEMU_TRACE_ARGS[@]}" &
QEMU_PID=$!

verified=0
for ((attempt = 0; attempt < TIMEOUT_SECONDS * 10; attempt++)); do
    if grep -Fq '[Domain 2] virtio-blk DMA and MSI-X IRQ verified' "$SERIAL" 2>/dev/null \
        && grep -Fq 'PCI requester 0018 returned to IOMMU deny-all by Hardware Domain 2' "$SERIAL" 2>/dev/null \
        && grep -Fq 'Hardware Domain 2 ready' "$SERIAL" 2>/dev/null; then
        verified=1
        break
    fi
    kill -0 "$QEMU_PID" 2>/dev/null || break
    sleep 0.1
done

if [[ $verified -ne 1 ]]; then
    sed -n '1,240p' "$SERIAL" >&2
    if [[ -s $TRACE ]]; then
        sed -n '1,240p' "$TRACE" >&2
    fi
    echo 'test-device-io: virtio-blk DMA/IRQ verification failed' >&2
    exit 1
fi

grep -E 'IOMMU|PCI requester 0018|virtio-blk|Hardware Domain 2 ready' "$SERIAL"
echo 'test-device-io: PASS'
