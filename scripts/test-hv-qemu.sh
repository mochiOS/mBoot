#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
QEMU=${QEMU:-qemu-system-x86_64}
TIMEOUT_SECONDS=${HV_TIMEOUT_SECONDS:-45}
ACCEL=${HV_ACCEL:-kvm}
CPU=${HV_CPU:-host}
EXPECT_BACKEND=${HV_EXPECT_BACKEND:-}
EFI="$ROOT/output/hv-target/x86_64-unknown-uefi/release/mboot-hv.efi"
OVMF_CODE="$ROOT/board/mboot/rootfs-overlay/usr/share/mboot/OVMF_CODE_4M.fd"
OVMF_VARS="$ROOT/board/mboot/rootfs-overlay/usr/share/mboot/OVMF_VARS_4M.fd"

for command in "$QEMU"; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "test-hv-qemu: missing command: $command" >&2
        exit 1
    }
done
test -s "$EFI" || {
    echo "test-hv-qemu: missing UEFI binary: $EFI" >&2
    exit 1
}

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
ESP="$WORK/esp"
VARS="$WORK/OVMF_VARS.fd"
SERIAL="$WORK/serial.log"

mkdir -p "$ESP/EFI/BOOT"
cp "$EFI" "$ESP/EFI/BOOT/BOOTX64.EFI"
cp "$OVMF_VARS" "$VARS"

"$QEMU" \
    -accel "$ACCEL" \
    -cpu "$CPU" \
    -machine q35 \
    -smp 1 \
    -m 512 \
    -drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
    -drive "if=pflash,format=raw,file=$VARS" \
    -drive "file=fat:rw:$ESP,format=raw" \
    -display none \
    -monitor none \
    -serial "file:$SERIAL" \
    -net none \
    -no-reboot \
    -no-shutdown &
QEMU_PID=$!

BOOTSTRAPPED=0
for ((attempt = 0; attempt < TIMEOUT_SECONDS * 10; attempt++)); do
    if grep -Fq '[mBoot-HV] bootstrap complete' "$SERIAL" 2>/dev/null; then
        BOOTSTRAPPED=1
        break
    fi
    if ! kill -0 "$QEMU_PID" 2>/dev/null; then
        break
    fi
    sleep 0.1
done

if [[ $BOOTSTRAPPED -ne 1 ]]; then
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-hv-qemu: hypervisor did not complete bootstrap' >&2
    exit 1
fi
if [[ -n $EXPECT_BACKEND ]]; then
    grep -Fq "backend=$EXPECT_BACKEND" "$SERIAL" || {
        sed -n '1,200p' "$SERIAL" >&2
        echo "test-hv-qemu: expected backend $EXPECT_BACKEND was not used" >&2
        exit 1
    }
fi
grep -Fq 'guest entry and VM exit verified' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-hv-qemu: guest did not exit through HLT interception' >&2
    exit 1
}

grep -F '[mBoot-HV]' "$SERIAL"
echo 'test-hv-qemu: PASS'
