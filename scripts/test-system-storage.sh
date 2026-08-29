#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
IMAGE=${HV_DISK_IMAGE:?HV_DISK_IMAGE is required}
SOURCE_ROOT_DISK=${MOCHIOS_ROOT_DISK:?MOCHIOS_ROOT_DISK is required}
QEMU=${QEMU:-qemu-system-x86_64}
KEEP_ARTIFACTS=${KEEP_SYSTEM_STORAGE_ARTIFACTS:-0}
DESKTOP_MARKER=${SYSTEM_STORAGE_DESKTOP_MARKER:-}
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
    TIMEOUT_SECONDS=${HV_TIMEOUT_SECONDS:-60}
else
    CPU=${HV_CPU:-max}
    TIMEOUT_SECONDS=${HV_TIMEOUT_SECONDS:-240}
fi

OVMF_CODE="$ROOT/firmware/OVMF_CODE_4M.fd"
OVMF_VARS="$ROOT/firmware/OVMF_VARS_4M.fd"
STORAGE_DISK_GUID=6d426f6f-7400-4b00-8a00-00000000d101
STORAGE_TYPE_GUID=6d6f6368-694f-5300-8000-6d5061727401
STORAGE_PARTITION_GUID=6d426f6f-7400-4b00-8a00-00000000d102
TARGET_FIRST_SECTOR=2048

for file in "$IMAGE" "$SOURCE_ROOT_DISK" "$OVMF_CODE" "$OVMF_VARS"; do
    test -s "$file" || { echo "missing file: $file" >&2; exit 1; }
done

source_first=$(sgdisk -i 2 "$SOURCE_ROOT_DISK" | sed -n 's/^First sector: \([0-9][0-9]*\).*/\1/p')
source_last=$(sgdisk -i 2 "$SOURCE_ROOT_DISK" | sed -n 's/^Last sector: \([0-9][0-9]*\).*/\1/p')
test -n "$source_first" && test -n "$source_last" || {
    echo 'could not locate rootfs partition 2' >&2
    exit 1
}
partition_sectors=$((source_last - source_first + 1))
target_last=$((TARGET_FIRST_SECTOR + partition_sectors - 1))
disk_sectors=$((target_last + 2049))

WORK=$(mktemp -d)
QEMU_PID=
cleanup() {
    if [[ -n $QEMU_PID ]] && kill -0 "$QEMU_PID" 2>/dev/null; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
    if [[ $KEEP_ARTIFACTS == 1 ]]; then
        echo "test-system-storage: artifacts kept at $WORK" >&2
    else
        rm -rf -- "$WORK"
    fi
}
trap cleanup EXIT

cp "$OVMF_VARS" "$WORK/OVMF_VARS.fd"
cp --sparse=always "$IMAGE" "$WORK/mochiOS.img"
truncate -s "$((disk_sectors * 512))" "$WORK/system-root.img"
sgdisk --clear --disk-guid="$STORAGE_DISK_GUID" \
    --new="1:$TARGET_FIRST_SECTOR:$target_last" \
    --typecode="1:$STORAGE_TYPE_GUID" \
    --partition-guid="1:$STORAGE_PARTITION_GUID" \
    "$WORK/system-root.img" >/dev/null
dd if="$SOURCE_ROOT_DISK" of="$WORK/system-root.img" bs=512 \
    skip="$source_first" seek="$TARGET_FIRST_SECTOR" count="$partition_sectors" \
    conv=notrunc status=none

source_before=$(sha256sum "$SOURCE_ROOT_DISK")
prefix_before=$(dd if="$WORK/system-root.img" bs=512 count="$TARGET_FIRST_SECTOR" status=none | sha256sum)
suffix_before=$(dd if="$WORK/system-root.img" bs=512 skip="$((target_last + 1))" status=none | sha256sum)

"$QEMU" \
    -accel "$ACCEL" \
    -cpu "$CPU" \
    -machine q35 \
    -global q35-pcihost.pci-hole64-size=32G \
    -global q35-pcihost.x-pci-hole64-fix=off \
    -boot menu=off,strict=on \
    -smp 1 \
    -m 1024 \
    -drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
    -drive "if=pflash,format=raw,file=$WORK/OVMF_VARS.fd" \
    -drive "if=none,id=boot,format=raw,file=$WORK/mochiOS.img" \
    -device virtio-blk-pci,drive=boot,addr=0x2,bootindex=1 \
    -drive "if=none,id=system,format=raw,file=$WORK/system-root.img" \
    -device virtio-blk-pci,drive=system,addr=0x3,disable-legacy=on,iommu_platform=on \
    -device virtio-gpu-pci,addr=0x4,disable-legacy=on,iommu_platform=on,xres=320,yres=200 \
    -device amd-iommu,dma-remap=on \
    -vga none \
    -display none \
    -monitor none \
    -serial "file:$WORK/serial.log" \
    -net none \
    -no-reboot \
    -no-shutdown &
QEMU_PID=$!

for ((attempt = 0; attempt < TIMEOUT_SECONDS * 10; attempt++)); do
    marker_ready=1
    if [[ -n $DESKTOP_MARKER ]] \
        && ! grep -Fq "$DESKTOP_MARKER" "$WORK/serial.log" 2>/dev/null; then
        marker_ready=0
    fi
    if grep -Fq '[Domain 1] [INFO]  mDriver storage mounted (read-write)' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq 'mDriver: block data I/O completed' "$WORK/serial.log" \
        && grep -Fq 'cext: loaded bundle ext2' "$WORK/serial.log" \
        && grep -Fq '[mBoot] Hardware Domain 2 ready' "$WORK/serial.log" \
        && [[ $marker_ready == 1 ]]; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
        QEMU_PID=
        source_after=$(sha256sum "$SOURCE_ROOT_DISK")
        prefix_after=$(dd if="$WORK/system-root.img" bs=512 count="$TARGET_FIRST_SECTOR" status=none | sha256sum)
        suffix_after=$(dd if="$WORK/system-root.img" bs=512 skip="$((target_last + 1))" status=none | sha256sum)
        [[ $source_before == "$source_after" ]] || {
            echo 'source root disk was modified' >&2
            exit 1
        }
        [[ $prefix_before == "$prefix_after" && $suffix_before == "$suffix_after" ]] || {
            echo 'data outside the enrolled partition changed' >&2
            exit 1
        }
        grep -E 'Hardware Domain 2 ready|block data I/O completed|mDriver storage|loaded bundle ext2|Binder.app' "$WORK/serial.log"
        echo 'test-system-storage: PASS'
        exit 0
    fi
    kill -0 "$QEMU_PID" 2>/dev/null || break
    sleep 0.1
done

sed -n '1,280p' "$WORK/serial.log" >&2
echo 'System Domain did not mount and read the mDriver storage device' >&2
exit 1
