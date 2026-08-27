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
STORAGE_CORRUPT=${MDRIVER_STORAGE_CORRUPT:-none}
STORAGE_INSPECT=${MDRIVER_STORAGE_INSPECT:-0}
OVMF_CODE="$ROOT/firmware/OVMF_CODE_4M.fd"
OVMF_VARS="$ROOT/firmware/OVMF_VARS_4M.fd"
STORAGE_DISK_GUID=6d426f6f-7400-4b00-8a00-00000000d001
STORAGE_TYPE_GUID=6d6f6368-694f-5300-8000-6d5061727401
STORAGE_PARTITION_GUID=6d426f6f-7400-4b00-8a00-00000000d002
STORAGE_FIRST_SECTOR=2048
STORAGE_LAST_SECTOR=12287

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
truncate -s 8M "$WORK/device.img"
sgdisk --clear --disk-guid="$STORAGE_DISK_GUID" \
    --new="1:$STORAGE_FIRST_SECTOR:$STORAGE_LAST_SECTOR" \
    --typecode="1:$STORAGE_TYPE_GUID" \
    --partition-guid="1:$STORAGE_PARTITION_GUID" \
    "$WORK/device.img" >/dev/null
if [[ $STORAGE_CORRUPT == primary ]]; then
    printf '\xff' | dd of="$WORK/device.img" bs=1 seek=600 conv=notrunc status=none
elif [[ $STORAGE_CORRUPT != none ]]; then
    echo "test-mdriver: unknown MDRIVER_STORAGE_CORRUPT mode: $STORAGE_CORRUPT" >&2
    exit 1
fi
whole_before=$(sha256sum "$WORK/device.img")
prefix_before=$(dd if="$WORK/device.img" bs=512 count="$STORAGE_FIRST_SECTOR" status=none | sha256sum)
suffix_before=$(dd if="$WORK/device.img" bs=512 skip="$((STORAGE_LAST_SECTOR + 1))" status=none | sha256sum)
partition_before=$(dd if="$WORK/device.img" bs=512 skip="$STORAGE_FIRST_SECTOR" \
    count="$((STORAGE_LAST_SECTOR - STORAGE_FIRST_SECTOR + 1))" status=none | sha256sum)
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
    if [[ $STORAGE_CORRUPT != none ]] \
        && grep -Fq 'mDriver: storage policy rejected primary GPT' "$WORK/serial.log" 2>/dev/null; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
        QEMU_PID=
        whole_after=$(sha256sum "$WORK/device.img")
        [[ $whole_before == "$whole_after" ]] || {
            echo 'test-mdriver: rejected storage was modified' >&2
            exit 1
        }
        ! grep -Fq 'mDriver: block data I/O completed' "$WORK/serial.log" || {
            echo 'test-mdriver: I/O completed after GPT rejection' >&2
            exit 1
        }
        echo 'test-mdriver: malformed GPT rejected without writes: PASS'
        exit 0
    fi
    if [[ $STORAGE_INSPECT == 1 ]] \
        && grep -Fq '[Domain 1] mDriver read-only storage inspection ready' "$WORK/serial.log" 2>/dev/null \
        && grep -Fq '[Domain 1] STORAGE DISK  6D426F6F-7400-4B00-8A00-00000000D001' "$WORK/serial.log" \
        && grep -Fq '[Domain 1] STORAGE TYPE 00 6D6F6368-694F-5300-8000-6D5061727401' "$WORK/serial.log" \
        && grep -Fq '[Domain 1] STORAGE PART 00 6D426F6F-7400-4B00-8A00-00000000D002' "$WORK/serial.log" \
        && grep -Fq '[Domain 1] STORAGE P00 RANGE 0000000000000800 0000000000002800' "$WORK/serial.log" \
        && grep -Fq 'mDriver asynchronous block queue ready' "$WORK/serial.log"; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
        QEMU_PID=
        whole_after=$(sha256sum "$WORK/device.img")
        [[ $whole_before == "$whole_after" ]] || {
            echo 'test-mdriver: read-only inspection modified storage' >&2
            exit 1
        }
        echo 'test-mdriver: read-only GPT inspection without writes: PASS'
        exit 0
    fi
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
            kill "$QEMU_PID" 2>/dev/null || true
            wait "$QEMU_PID" 2>/dev/null || true
            QEMU_PID=
            prefix_after=$(dd if="$WORK/device.img" bs=512 count="$STORAGE_FIRST_SECTOR" status=none | sha256sum)
            suffix_after=$(dd if="$WORK/device.img" bs=512 skip="$((STORAGE_LAST_SECTOR + 1))" status=none | sha256sum)
            partition_after=$(dd if="$WORK/device.img" bs=512 skip="$STORAGE_FIRST_SECTOR" \
                count="$((STORAGE_LAST_SECTOR - STORAGE_FIRST_SECTOR + 1))" status=none | sha256sum)
            [[ $prefix_before == "$prefix_after" && $suffix_before == "$suffix_after" ]] || {
                echo 'test-mdriver: data outside the enrolled partition changed' >&2
                exit 1
            }
            [[ $partition_before != "$partition_after" ]] || {
                echo 'test-mdriver: enrolled partition did not receive the test write' >&2
                exit 1
            }
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
