#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
IMAGE=${HV_DISK_IMAGE:-$ROOT/output/mdriver.iso}
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
MDRIVER_VECTORS=${MDRIVER_VECTORS:-}
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
cp --sparse=always "$IMAGE" "$WORK/mdriver.iso"
truncate -s 1M "$WORK/device.img"
DEVICE_VECTOR_OPTION=
if [[ -n $MDRIVER_VECTORS ]]; then
    DEVICE_VECTOR_OPTION=",vectors=$MDRIVER_VECTORS"
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
    -drive "if=none,id=disk,format=raw,file=$WORK/mdriver.iso" \
    -device virtio-blk-pci,drive=disk,addr=0x2,bootindex=1 \
    -drive "if=none,id=device,format=raw,file=$WORK/device.img" \
    -device "virtio-blk-pci,drive=device,addr=0x3,disable-legacy=on,iommu_platform=on$DEVICE_VECTOR_OPTION" \
    -device amd-iommu,dma-remap=on \
    -display none \
    -monitor none \
    -serial "file:$WORK/serial.log" \
    -net none \
    -no-reboot \
    -no-shutdown &
QEMU_PID=$!

for ((attempt = 0; attempt < TIMEOUT_SECONDS * 10; attempt++)); do
    if grep -Fq 'mDriver OK' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq 'mDriver: block data I/O completed' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq 'mDriver: mBoot control Event Channel ready' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq 'mDriver control Event Channel verified' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq 'mDriver: mBoot device control protocol ready' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq 'mDriver device control protocol ready' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq 'mDriver block data path ready' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq 'mDriver: asynchronous block queue ready' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq 'mDriver: asynchronous block batch completed' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq 'mDriver asynchronous block queue ready' "$WORK/serial.log" 2>/dev/null \
        && grep -Eq 'mDriver: mBoot PCI inventory ready: [0-9]+ devices, 1 claimed' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq 'PCI requester 0018 mapped for DMA and claimed-disabled by Hardware Domain 2' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq 'mDriver: mBoot PCI frontend ready: 1 devices, 2 resources, 1 active' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq '[mBoot] Hardware Domain 2 ready' "$WORK/serial.log" 2>/dev/null; then
        route_verified=0
        if [[ $MDRIVER_VECTORS == 1 ]]; then
            shared_vector=$(sed -n 's/.*uses shared MSI-X IRQ [0-9][0-9]* vector \(0x[0-9a-f][0-9a-f]*\).*/\1/p' "$WORK/serial.log" | head -n 1)
            if [[ -n $shared_vector ]] \
                && grep -Fq "PCI requester 0018 active: shared MSI-X IRQ 0x50 -> Domain 2 vector $shared_vector" "$WORK/serial.log"; then
                route_verified=1
            fi
        else
            config_vector=$(sed -n 's/.*uses config IRQ [0-9][0-9]* vector \(0x[0-9a-f][0-9a-f]*\), queue IRQ.*/\1/p' "$WORK/serial.log" | head -n 1)
            queue_vector=$(sed -n 's/.*queue IRQ [0-9][0-9]* vector \(0x[0-9a-f][0-9a-f]*\).*/\1/p' "$WORK/serial.log" | head -n 1)
            if [[ -n $config_vector && -n $queue_vector ]] \
                && grep -Fq "PCI requester 0018 active: config IRQ 0x50 -> Domain 2 vector $config_vector, queue IRQ 0x51 -> vector $queue_vector" "$WORK/serial.log"; then
                route_verified=1
            fi
        fi
        if [[ $route_verified == 1 ]]; then
            grep -E '\[mBoot\]|mDriver: mBoot (PCI|control)|mDriver (block IRQ )?OK|Linux version|PCI requester 0018|virtio_blk| vda' "$WORK/serial.log"
            echo 'test-mdriver: PASS'
            exit 0
        fi
    fi
    kill -0 "$QEMU_PID" 2>/dev/null || break
    sleep 0.1
done

sed -n '1,240p' "$WORK/serial.log" >&2
echo 'test-mdriver: assigned block I/O did not complete through the mBoot interrupt route' >&2
exit 1
