#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
IMAGE=${HV_DISK_IMAGE:?HV_DISK_IMAGE is required}
CONFIG=${HV_CONFIG:?HV_CONFIG is required}
QEMU=${QEMU:-qemu-system-x86_64}
ACCEL=${HV_ACCEL:-kvm}
CPU=${HV_CPU:-host}
TIMEOUT_SECONDS=${HV_TIMEOUT_SECONDS:-30}
MEMORY_MIB=${HV_MEMORY_MIB:-1024}
OVMF_CODE="$ROOT/firmware/OVMF_CODE_4M.fd"
OVMF_VARS="$ROOT/firmware/OVMF_VARS_4M.fd"
DOMAIN_COUNT=$($ROOT/scripts/config-value.pl "$CONFIG" domain_count)
SYSTEM_IMAGE=$($ROOT/scripts/config-value.pl "$CONFIG" system_image)
HARDWARE_BOOTSTRAP_ID=$($ROOT/scripts/config-value.pl "$CONFIG" hardware_bootstrap_id)
GPU_TEST=${HV_GPU_TEST:-0}
DESKTOP_MARKER=${HV_DESKTOP_MARKER:-}
IOMMU_TEST=${HV_IOMMU_TEST:-intel}

QEMU_GPU_ARGS=()
QEMU_MACHINE=q35
if [[ $GPU_TEST == 1 ]]; then
    if [[ $IOMMU_TEST == amd ]]; then
        QEMU_GPU_ARGS+=(
            -device amd-iommu,dma-remap=on
            -vga none
            -device virtio-gpu-pci,disable-legacy=on,iommu_platform=on
        )
    else
        # QEMU cannot expose interrupt remapping through the in-kernel irqchip.
        # Split mode keeps interrupt routing visible to the emulated VT-d unit.
        QEMU_MACHINE=q35,kernel-irqchip=split
        QEMU_GPU_ARGS+=(
            -device intel-iommu,intremap=on
            -vga none
            -device virtio-vga
        )
    fi
fi

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
cp --sparse=always "$IMAGE" "$WORK/mochiOS.img"
SERIAL_LOG=${HV_SERIAL_LOG:-"$WORK/serial.log"}
mkdir -p "$(dirname "$SERIAL_LOG")"
: > "$SERIAL_LOG"

"$QEMU" \
    -accel "$ACCEL" \
    -cpu "$CPU" \
    -machine "$QEMU_MACHINE" \
    -boot menu=off,strict=on \
    -smp 1 \
    -m "$MEMORY_MIB" \
    -drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
    -drive "if=pflash,format=raw,file=$WORK/OVMF_VARS.fd" \
    -drive "if=none,id=disk,format=raw,file=$WORK/mochiOS.img" \
    -device virtio-blk-pci,drive=disk,bootindex=1 \
    -display none \
    -monitor none \
    -serial "file:$SERIAL_LOG" \
    -net none \
    "${QEMU_GPU_ARGS[@]}" \
    -no-reboot \
    -no-shutdown &
QEMU_PID=$!

if [[ $SYSTEM_IMAGE == mochios-system ]]; then
    expected='[Domain 1] [INFO]  Kernel initialization complete. Entering idle loop...'
elif [[ $SYSTEM_IMAGE == mochios ]]; then
    expected='[mBoot] mochiOS System Domain 1 ready'
else
    expected="[mBoot] bootstrap complete; $DOMAIN_COUNT Domain"
fi
for ((attempt = 0; attempt < TIMEOUT_SECONDS * 10; attempt++)); do
    marker_ready=1
    if [[ -n $DESKTOP_MARKER ]] \
        && ! grep -Fq "$DESKTOP_MARKER" "$SERIAL_LOG" 2>/dev/null; then
        marker_ready=0
    fi
    if grep -Fq "$expected" "$SERIAL_LOG" 2>/dev/null \
        && { [[ $HARDWARE_BOOTSTRAP_ID == 0 ]] \
            || grep -Fq "[mBoot] Hardware Domain $HARDWARE_BOOTSTRAP_ID ready" "$SERIAL_LOG" 2>/dev/null; } \
        && [[ $marker_ready == 1 ]]; then
        break
    fi
    kill -0 "$QEMU_PID" 2>/dev/null || break
    sleep 0.1
done
grep -Fq "$expected" "$SERIAL_LOG" || {
    sed -n '1,200p' "$SERIAL_LOG" >&2
    echo "hypervisor image did not reach its expected Domain state" >&2
    exit 1
}
if [[ -n $DESKTOP_MARKER ]]; then
    grep -Fq "$DESKTOP_MARKER" "$SERIAL_LOG" || {
        sed -n '1,260p' "$SERIAL_LOG" >&2
        echo "desktop marker was not reached: $DESKTOP_MARKER" >&2
        exit 1
    }
fi
if [[ $HARDWARE_BOOTSTRAP_ID != 0 ]]; then
    grep -Fq "[mBoot] Hardware Domain $HARDWARE_BOOTSTRAP_ID ready" "$SERIAL_LOG" || {
        sed -n '1,200p' "$SERIAL_LOG" >&2
        echo 'Hardware Domain did not query devices and become ready' >&2
        exit 1
    }
fi
if [[ $GPU_TEST == 1 ]]; then
    grep -Eq '\[mBoot\].*(display|Display).*(Hardware Domain|Domain 2|handed off)|mboot-pci.*display' "$SERIAL_LOG" || {
        sed -n '1,240p' "$SERIAL_LOG" >&2
        echo 'mDriver did not claim an automatically discovered display controller' >&2
        exit 1
    }
fi
grep -E '\[mBoot\]|\[Domain' "$SERIAL_LOG"
echo 'test-disk-image: PASS'
