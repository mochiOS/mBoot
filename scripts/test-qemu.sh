#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
QEMU=${QEMU:-qemu-system-x86_64}
TIMEOUT_SECONDS=${HV_TIMEOUT_SECONDS:-45}
ACCEL=${HV_ACCEL:-kvm}
CPU=${HV_CPU:-host}
EXPECT_BACKEND=${HV_EXPECT_BACKEND:-}
EXPECT_IOMMU=${HV_EXPECT_IOMMU:-}
IOMMU_DEVICE=${HV_IOMMU_DEVICE:-}
IOMMU_PROBE_ONLY=${HV_IOMMU_PROBE_ONLY:-0}
EFI="$ROOT/output/target/x86_64-unknown-uefi/release/mboot.efi"
RING_BOOTSTRAP_DOMAIN_ELF=${RING_BOOTSTRAP_DOMAIN_ELF:-"$ROOT/../core/target/x86_64-unknown-none/release/ring-bootstrap"}
MBOOT_LAUNCH_MANIFEST=${MBOOT_LAUNCH_MANIFEST:-${HV_LAUNCH_MANIFEST:-"$ROOT/output/launch.manifest"}}
OVMF_CODE="$ROOT/firmware/OVMF_CODE_4M.fd"
OVMF_VARS="$ROOT/firmware/OVMF_VARS_4M.fd"

for command in "$QEMU" mkfs.vfat mcopy mmd truncate; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "test-qemu: missing command: $command" >&2
        exit 1
    }
done
test -s "$EFI" || {
    echo "test-qemu: missing UEFI binary: $EFI" >&2
    exit 1
}
test -s "$RING_BOOTSTRAP_DOMAIN_ELF" || {
    echo "test-qemu: missing Shared Ring bootstrap image: $RING_BOOTSTRAP_DOMAIN_ELF" >&2
    exit 1
}
test -s "$MBOOT_LAUNCH_MANIFEST" || {
    echo "test-qemu: missing Launch Manifest: $MBOOT_LAUNCH_MANIFEST" >&2
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
mcopy -i "$ESP" "$MBOOT_LAUNCH_MANIFEST" ::/EFI/MBOOT/LAUNCH.MF
cp "$OVMF_VARS" "$VARS"

IOMMU_ARGS=()
if [[ -n $IOMMU_DEVICE ]]; then
    IOMMU_ARGS=(-device "$IOMMU_DEVICE")
fi

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
    "${IOMMU_ARGS[@]}" \
    -display none \
    -monitor none \
    -serial "file:$SERIAL" \
    -net none \
    -no-reboot \
    -no-shutdown &
QEMU_PID=$!

if [[ $IOMMU_PROBE_ONLY == 1 ]]; then
    [[ -n $EXPECT_IOMMU ]] || {
        echo 'test-qemu: HV_EXPECT_IOMMU is required for an IOMMU probe' >&2
        exit 1
    }
    PROBED=0
    for ((attempt = 0; attempt < TIMEOUT_SECONDS * 10; attempt++)); do
        if grep -Fq "IOMMU description $EXPECT_IOMMU:" "$SERIAL" 2>/dev/null \
            && grep -Fq 'PCI DMA quarantine:' "$SERIAL" 2>/dev/null \
            && grep -Fq "IOMMU DMA protection enabled: $EXPECT_IOMMU deny-all" "$SERIAL" 2>/dev/null; then
            PROBED=1
            break
        fi
        kill -0 "$QEMU_PID" 2>/dev/null || break
        sleep 0.1
    done
    if [[ $PROBED -ne 1 ]]; then
        sed -n '1,200p' "$SERIAL" >&2
        echo "test-qemu: IOMMU probe failed: $EXPECT_IOMMU" >&2
        exit 1
    fi
    grep -E 'IOMMU description|IOMMU unit|PCI DMA quarantine|IOMMU DMA protection' "$SERIAL"
    echo 'test-qemu: IOMMU probe PASS'
    exit 0
fi

BOOTSTRAPPED=0
for ((attempt = 0; attempt < TIMEOUT_SECONDS * 10; attempt++)); do
    if grep -Fq '[mBoot] bootstrap complete; 3 Domain(s) stopped cleanly; 1 crash(es) recovered' "$SERIAL" 2>/dev/null; then
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
    echo 'test-qemu: hypervisor did not complete bootstrap' >&2
    exit 1
fi
grep -Fq 'Domain 3 crashed and was isolated: exit=0x400 gpa=0x40000000' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: nested-page fault did not remain isolated to Application Domain 3' >&2
    exit 1
}
grep -Fq 'Domain 3 restarted: attempt=1' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: Application Domain 3 was not restarted once' >&2
    exit 1
}
grep -Fq '[Domain 3] Application Domain restarted' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: restarted Application Domain did not reach its clean path' >&2
    exit 1
}
grep -Fq '[Domain 1] Domain crash notification verified' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: System Domain did not read the crash notification' >&2
    exit 1
}
grep -Fq 'PCI DMA quarantine:' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: PCI DMA quarantine did not run' >&2
    exit 1
}
if [[ -n $EXPECT_IOMMU ]]; then
    grep -Fq "IOMMU description $EXPECT_IOMMU:" "$SERIAL" || {
        sed -n '1,200p' "$SERIAL" >&2
        echo "test-qemu: expected IOMMU description was not discovered: $EXPECT_IOMMU" >&2
        exit 1
    }
fi
if [[ -n $EXPECT_BACKEND ]]; then
    grep -Fq "backend=$EXPECT_BACKEND" "$SERIAL" || {
        sed -n '1,200p' "$SERIAL" >&2
        echo "test-qemu: expected backend $EXPECT_BACKEND was not used" >&2
        exit 1
    }
fi
grep -Fq 'Domain 1 stopped: reason=0 yields=0' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: System Domain did not stop cleanly' >&2
    exit 1
}
grep -Fq 'Domain 2 stopped: reason=0 yields=1' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: Hardware Domain did not stop cleanly' >&2
    exit 1
}
grep -Fq '[Domain 2] Event Channel IRQ received' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: Hardware Domain did not receive its virtual IRQ' >&2
    exit 1
}
grep -Fq '[Domain 2] x2APIC MSR interface verified' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: Hardware Domain did not verify the x2APIC MSR interface' >&2
    exit 1
}
grep -Fq '[Domain 2] Virtual CPUID model verified' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: Hardware Domain did not verify the virtual CPUID model' >&2
    exit 1
}
grep -Fq '[Domain 2] Rejected MSR delivered #GP' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: rejected MSR did not fault only the Hardware Domain' >&2
    exit 1
}
grep -Fq '[Domain 2] x2APIC self IPI received' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: x2APIC Self IPI and ICR self delivery were not verified' >&2
    exit 1
}
grep -Fq '[Domain 2] Local APIC timer IRQ received' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: virtual Local APIC timer did not fire' >&2
    exit 1
}
grep -Fq '[Domain 1] Shared Ring bootstrap entered' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: System Domain ConsoleWrite was not handled' >&2
    exit 1
}
grep -Fq '[Domain 2] Shared Ring bootstrap entered' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: Hardware Domain ConsoleWrite was not handled' >&2
    exit 1
}
grep -Fq '[Domain 2] Shared Ring request handled' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: Hardware Domain did not consume the Shared Ring request' >&2
    exit 1
}
grep -Fq '[Domain 1] Shared Ring response verified' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: System Domain did not verify the Shared Ring response' >&2
    exit 1
}
grep -Fq 'Grant 257 mapped: Domain 2 GPA 0x1f0000' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: Grant page was not mapped into the Hardware Domain' >&2
    exit 1
}
grep -Fq 'Grant 257 unmapped from Domain 2' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: Grant page was not unmapped from the Hardware Domain' >&2
    exit 1
}
grep -Fq 'Event Channel 1:1 -> 2:1' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: forward Event Channel delivery was not observed' >&2
    exit 1
}
grep -Fq 'Event Channel 2:1 -> 1:1' "$SERIAL" || {
    sed -n '1,200p' "$SERIAL" >&2
    echo 'test-qemu: reverse Event Channel delivery was not observed' >&2
    exit 1
}

grep -E '\[mBoot\]|\[Domain' "$SERIAL"
echo 'test-qemu: PASS'
