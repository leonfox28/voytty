#!/bin/sh

set -eu

# The dependency trees below are compared byte-for-byte. Override callers such
# as GitHub Actions which force ANSI color through their ambient environment.
export CARGO_TERM_COLOR=never

reject_host_engine() {
    package=$1
    tree=$(cargo +1.98.0 tree --locked -p "$package" --charset ascii)
    if printf '%s\n' "$tree" \
        | grep -E '(^|[[:space:]])(voytty-terminal|alacritty_terminal|vte) v' >/dev/null; then
        echo "$package must remain independent of the host terminal engine" >&2
        printf '%s\n' "$tree" >&2
        exit 1
    fi
}

reject_host_engine voytty-core
reject_host_engine voytty-proto

# Only production/build edges define the distributable mobile boundary. Desktop
# tests may exercise the canonical Unix adapter; no host crate is shipped.
for package in voytty-client voytty-android; do
    for target in aarch64-apple-darwin aarch64-linux-android aarch64-apple-ios aarch64-apple-ios-sim; do
        tree=$(cargo +1.98.0 tree --locked -p "$package" --target "$target" --edges normal,build --charset ascii)
        if printf '%s\n' "$tree" | grep -E '(^|[[:space:]])(voytty-daemon|voytty-platform|voytty-cli|voytty-terminal|alacritty_terminal|vte|portable-pty|portmapper) v' >/dev/null; then
            echo "$package ($target) imports a desktop/host dependency" >&2
            printf '%s\n' "$tree" >&2
            exit 1
        fi
    done
done

vte_tree=$(
    cargo +1.98.0 tree --locked --workspace --invert vte@0.15.0 --charset ascii \
        | sed -E 's/ v[0-9][^ ]* \([^)]*\)$//'
)
expected_vte_tree='vte v0.15.0
`-- alacritty_terminal v0.26.0
    `-- voytty-terminal
        `-- voytty-daemon
            `-- voytty-cli'

if [ "$vte_tree" != "$expected_vte_tree" ]; then
    echo "vte must have exactly the pinned official Alacritty dependency path" >&2
    printf '%s\n' "$vte_tree" >&2
    exit 1
fi

echo "terminal dependency policy verified"
