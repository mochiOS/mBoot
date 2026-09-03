#!/usr/bin/env bash
set -euo pipefail

root_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mdriver_dir="$root_dir/mdriver"
config_file="${MBOOT_CONFIG:-$root_dir/config/qemu.toml}"
mnu_dir="${MNU_DIR:-$root_dir/../core}"

need_command() {
    command -v "$1" >/dev/null 2>&1 || {
        printf 'mBoot setup: %s was not found\n' "$1" >&2
        exit 1
    }
}

for command_name in bash curl make perl sha256sum tar rustup cargo; do
    need_command "$command_name"
done

toolchain=$(perl "$root_dir/scripts/config-value.pl" "$config_file" toolchain)

"$mdriver_dir/scripts/setup-buildroot.sh"
make -C "$mdriver_dir" defconfig
make -C "$mdriver_dir" source

if ! rustup run "$toolchain" rustc --version >/dev/null 2>&1; then
    rustup toolchain install "$toolchain" --profile minimal --component rust-src
elif ! rustup component list --toolchain "$toolchain" --installed \
        | grep -Fxq rust-src; then
    rustup component add --toolchain "$toolchain" rust-src
fi
cargo "+$toolchain" fetch --manifest-path "$root_dir/Cargo.toml"
if [[ -f "$mnu_dir/Cargo.toml" ]]; then
    cargo "+$toolchain" fetch --manifest-path "$mnu_dir/Cargo.toml"
fi

mkdir -p "$mdriver_dir/.cache"
{
    printf 'buildroot=2025.02.16\n'
    printf 'toolchain=%s\n' "$toolchain"
    printf 'config=%s\n' "$config_file"
} >"$mdriver_dir/.cache/setup-complete.new"
mv "$mdriver_dir/.cache/setup-complete.new" "$mdriver_dir/.cache/setup-complete"

printf 'mBoot setup complete\n'
