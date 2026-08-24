#!/usr/bin/env bash
set -euo pipefail

mdriver_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
images_dir="$mdriver_dir/output/images"
artifacts_dir="$mdriver_dir/output/artifacts"
kernel_configs=("$mdriver_dir"/output/build/linux-*/.config)

for source_file in "$images_dir/vmlinux" "$images_dir/rootfs.cpio"; do
    [[ -f "$source_file" ]] || {
        printf 'mDriver build: missing %s\n' "$source_file" >&2
        exit 1
    }
done

if [[ ${#kernel_configs[@]} -ne 1 ]] || [[ ! -f "${kernel_configs[0]}" ]]; then
    printf 'mDriver build: final Linux configuration was not found\n' >&2
    exit 1
fi

mkdir -p "$artifacts_dir"
cp "$images_dir/vmlinux" "$artifacts_dir/vmlinux.new"
cp "$images_dir/rootfs.cpio" "$artifacts_dir/initramfs.cpio.new"
cp "${kernel_configs[0]}" "$artifacts_dir/linux.config.new"
mv "$artifacts_dir/vmlinux.new" "$artifacts_dir/vmlinux"
mv "$artifacts_dir/initramfs.cpio.new" "$artifacts_dir/initramfs.cpio"
mv "$artifacts_dir/linux.config.new" "$artifacts_dir/linux.config"
