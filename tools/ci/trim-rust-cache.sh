#!/bin/sh
set -eu

# Keep third-party compilation outputs; test binaries, workspace outputs and
# incremental state cost more to transfer than to rebuild on this repository.
# Path packages (workspace members and patched vendor crates) are fingerprinted
# by source mtime, so a fresh checkout rebuilds them whether or not they are
# cached.
packages=$(cargo +1.98.0 metadata --locked --format-version 1 \
    | jq -r '.packages[] | select(.source == null) | "\(.name)@\(.version)"')
for package in $packages; do
    cargo +1.98.0 clean --package "$package"
done
if [ -d target/debug/deps ]; then
    find target/debug/deps -type f -perm -u+x \
        ! -name '*.so' ! -name '*.dylib' -delete
fi
maximum_kib=2097152
size=0
for directory in target/debug/deps target/debug/build target/debug/.fingerprint; do
    if [ -d "$directory" ]; then
        size=$((size + $(du -sk "$directory" | awk '{print $1}')))
    fi
done
echo "Compiled dependency cache: $size KiB (maximum $maximum_kib KiB)"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
    if [ "$size" -le "$maximum_kib" ]; then
        echo 'cacheable=true' >>"$GITHUB_OUTPUT"
    else
        echo 'cacheable=false' >>"$GITHUB_OUTPUT"
    fi
fi
