#!/usr/bin/env bash
set -euo pipefail

mdriver_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
state="$mdriver_dir/.cache/linux-inputs.sha256"
mkdir -p "$mdriver_dir/.cache"
candidate=$(mktemp "$mdriver_dir/.cache/linux-inputs.XXXXXX")
trap 'rm -f -- "$candidate"' EXIT

while IFS= read -r -d '' input; do
    sha256sum "$input"
done < <(
    find \
        "$mdriver_dir/board/mdriver/patches/linux" \
        "$mdriver_dir/board/mdriver/linux.config" \
        -type f -print0 | sort -z
) >"$candidate"

if cmp -s "$candidate" "$state"; then
    exit 0
fi

if [[ -f "$state" && -d "$mdriver_dir/output/build" ]]; then
    "$mdriver_dir/scripts/buildroot-make.sh" linux-dirclean
fi

mv -f -- "$candidate" "$state"
