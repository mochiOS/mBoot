#!/usr/bin/env bash
set -euo pipefail

mdriver_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
state="$mdriver_dir/.cache/local-inputs.sha256"
mkdir -p "$mdriver_dir/.cache"
candidate=$(mktemp "$mdriver_dir/.cache/local-inputs.XXXXXX")
trap 'rm -f -- "$candidate"' EXIT

while IFS= read -r -d '' input; do
    sha256sum "$input"
done < <(
    find \
        "$mdriver_dir/gpu" \
        "$mdriver_dir/init" \
        "$mdriver_dir/package/mdriver-gpu" \
        "$mdriver_dir/package/mdriver-init" \
        -type f -print0 | sort -z
) >"$candidate"

if cmp -s "$candidate" "$state"; then
    exit 0
fi

# Buildroot's local source method does not invalidate a package when its source
# directory changes.  Drop only the two tiny local packages; dependencies and
# the Linux build remain cached.
if [[ -f "$state" && -d "$mdriver_dir/output/build" ]]; then
    "$mdriver_dir/scripts/buildroot-make.sh" \
        mdriver-gpu-dirclean mdriver-init-dirclean
fi

mv -f -- "$candidate" "$state"
