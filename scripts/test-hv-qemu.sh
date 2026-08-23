#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
QEMU=${QEMU:-qemu-system-x86_64}
TIMEOUT_SECONDS=${HV_TIMEOUT_SECONDS:-45}
ACCEL=${HV_ACCEL:-kvm}
CPU=${HV_CPU:-host}
EXPECT_BACKEND=${HV_EXPECT_BACKEND:-}
EFI="$ROOT/output/hv-target/x86_64-unknown-uefi/release/mboot-hv.efi"
RING_BOOTSTRAP_DOMAIN_ELF=${RING_BOOTSTRAP_DOMAIN_ELF:-"$ROOT/../core/target/x86_64-unknown-none/release/ring-bootstrap"}
HV_LAUNCH_MANIFEST=${HV_LAUNCH_MANIFEST:-"$ROOT/output/hv/launch.manifest"}
OVMF_CODE="$ROOT/board/mboot/rootfs-overlay/usr/share/mboot/OVMF_CODE_4M.fd"
OVMF_VARS="$ROOT/board/mboot/rootfs-overlay/usr/share/mboot/OVMF_VARS_4M.fd"

for command in "$QEMU" mkfs.vfat mcopy mmd truncate; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "test-hv-qemu: missing command: $command" >&2
        exit 1
    }
done
test -s "$EFI" || {
    echo "test-hv-qemu: missing UEFI binary: $EFI" >&2
    exit 1
}
test -s "$RING_BOOTSTRAP_DOMAIN_ELF" || {
    echo "test-hv-qemu: missing Shared Ring bootstrap image: $RING_BOOTSTRAP_DOMAIN_ELF" >&2
    exit 1
}
test -s "$HV_LAUNCH_MANIFEST" || {
    echo "test-hv-qemu: missing Launch Manifest: $HV_LAUNCH_MANIFEST" >&2
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
ESP="$WORK/esp.img"
VARS="$WORK/OVMF_VARS.fd"
SERIAL="$WORK/serial.log"

truncate -s 64M "$ESP"
mkfs.vfat -n MBOOTTEST "$ESP" >/dev/null
mmd -i "$ESP" ::/EFI
mmd -i "$ESP" ::/EFI/BOOT
mmd -i "$ESP" ::/EFI/MBOOT
mcopy -i "$ESP" "$EFI" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$ESP" "$RING_BOOTSTRAP_DOMAIN_ELF" ::/EFI/MBOOT/RINGBOOT.ELF
mcopy -i "$ESP" "$HV_LAUNCH_MANIFEST" ::/EFI/MBOOT/LAUNCH.MF
cp "$OVMF_VARS" "$VARS"

"$QEMU" \
    -accel "$ACCEL" \
    -cpu "$CPU" \
    -machine q35 \
    -boot menu=off,strict=on \
    -smp 1 \
    -m 512 \
    -drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
    -drive "if=pflash,format=raw,file=$VARS" \
    -drive "if=none,id=esp,format=raw,file=$ESP" \
    -device virtio-blk-pci,drive=esp,bootindex=1 \
    -display none \
    -monitor none \
    -serial "file:$SERIAL" \
    -net none \
    -no-reboot \
    -no-shutdown &
QEMU_PID=$!

BOOTSTRAPPED=0
for ((attempt = 0; attempt < TIMEOUT_SECONDS * 10; attempt++)); do
    if grep -Fq '[mBoot-HV] bootstrap complete; 2 Domains entered and stopped cleanly' "$SERIAL" 2>/dev/null; then
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
grep -Fq 'Domain 1 stopped: reason=0 yields=0' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-hv-qemu: System Domain did not stop cleanly' >&2
    exit 1
}
grep -Fq 'Domain 2 stopped: reason=0 yields=0' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-hv-qemu: Hardware Domain did not stop cleanly' >&2
    exit 1
}
grep -Fq '[Domain 1] Shared Ring bootstrap entered' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-hv-qemu: System Domain ConsoleWrite was not handled' >&2
    exit 1
}
grep -Fq '[Domain 2] Shared Ring bootstrap entered' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-hv-qemu: Hardware Domain ConsoleWrite was not handled' >&2
    exit 1
}
grep -Fq '[Domain 2] Shared Ring request handled' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-hv-qemu: Hardware Domain did not consume the Shared Ring request' >&2
    exit 1
}
grep -Fq '[Domain 1] Shared Ring response verified' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-hv-qemu: System Domain did not verify the Shared Ring response' >&2
    exit 1
}
grep -Fq 'Grant 257 mapped: Domain 2 GPA 0x1f0000' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-hv-qemu: Grant page was not mapped into the Hardware Domain' >&2
    exit 1
}
grep -Fq 'Grant 257 unmapped from Domain 2' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-hv-qemu: Grant page was not unmapped from the Hardware Domain' >&2
    exit 1
}
grep -Fq 'Event Channel 1:1 -> 2:1' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-hv-qemu: forward Event Channel delivery was not observed' >&2
    exit 1
}
grep -Fq 'Event Channel 2:1 -> 1:1' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-hv-qemu: reverse Event Channel delivery was not observed' >&2
    exit 1
}

grep -E '\[mBoot-HV\]|\[Domain' "$SERIAL"
echo 'test-hv-qemu: PASS'
