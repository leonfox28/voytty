#!/bin/sh
set -eu

fail() {
    echo "glibc builder setup failed: $*" >&2
    exit 1
}

[ "$(id -u)" -eq 0 ] || fail "the pinned Debian container must run setup as root"
[ -n "${GITHUB_WORKSPACE:-}" ] || fail "GITHUB_WORKSPACE is required"
[ -f /etc/apt/sources.list ] || fail "Debian package sources are unavailable"

sed -i \
    -e 's|^# deb http://snapshot|deb [check-valid-until=no] http://snapshot|' \
    -e 's|^deb http://deb|# deb http://deb|' \
    /etc/apt/sources.list
apt-get -o Acquire::Check-Valid-Until=false update
DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends \
    binutils build-essential ca-certificates curl file git jq pkg-config

git config --global --get-all safe.directory | grep -Fx "$GITHUB_WORKSPACE" >/dev/null \
    || fail "the workflow must trust only the exact checkout before builder setup"
# Official rustup 1.29.0 archive digests, one per container architecture.
case "$(uname -m)" in
    x86_64) sha=4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10 ;;
    aarch64) sha=9732d6c5e2a098d3521fca8145d826ae0aaa067ef2385ead08e6feac88fa5792 ;;
    *) fail "unsupported builder architecture" ;;
esac
staging=$(mktemp -d)
installer=$staging/rustup-init
trap 'rm -rf "$staging"' EXIT
curl --proto '=https' --proto-redir '=https' --tlsv1.2 --max-time 120 \
    --fail --silent --show-error --location --output "$installer" \
    "https://static.rust-lang.org/rustup/archive/1.29.0/$(uname -m)-unknown-linux-gnu/rustup-init"
printf '%s  %s\n' "$sha" "$installer" | sha256sum --check --status
chmod 700 "$installer"
"$installer" -y --no-modify-path --profile minimal --default-toolchain 1.98.0
echo "$HOME/.cargo/bin" >>"$GITHUB_PATH"

echo 'pinned Debian 10 / glibc 2.28 builder is ready'
