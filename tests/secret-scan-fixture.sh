#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
scanner="$repo_root/tests/secret-scan.sh"
test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT HUP INT TERM

fail() {
    echo "secret scan fixture failed: $*" >&2
    exit 1
}

mkdir -p "$test_dir/target" "$test_dir/deploy"
printf '%s%s\n' 'tok' 'en = part.strip()' >"$test_dir/target/example.py"
SECRET_SCAN_ROOT="$test_dir" sh "$scanner" >/dev/null \
    || fail "generated build output was scanned"

assert_secret_rejected() {
    case_name=$1
    shift
    rm -f "$test_dir/deploy/credential.txt"
    "$@" >"$test_dir/deploy/credential.txt"
    if SECRET_SCAN_ROOT="$test_dir" sh "$scanner" >/dev/null 2>&1; then
        fail "$case_name fixture was accepted"
    fi
}

begin_marker=BEGIN
private_marker=PRIVATE
key_marker=KEY
# Positional parameters belong to the child shell.
# shellcheck disable=SC2016
assert_secret_rejected private-key \
    sh -c 'printf "%s %s %s\n" "$1" "$2" "$3"' \
    sh "$begin_marker" "$private_marker" "$key_marker"
assert_secret_rejected github-token \
    sh -c 'printf "ghp_%040d\n" 0'
assert_secret_rejected aws-access-key \
    sh -c 'printf "AKIA%016d\n" 0'

# Runtime protocol values are not embedded credentials. Vendored source stays
# in scope: both literal assignments and recognizable keys must be rejected.
rm -f "$test_dir/deploy/credential.txt"
mkdir -p "$test_dir/vendor/protocol"
printf '%s%s\n' 'let tok' 'en = incoming.decode()?;' >"$test_dir/vendor/protocol/value.rs"
printf '%s%s\n' 'tok' 'en = uuid.uuid4().hex' >"$test_dir/value.py"
printf '%s%s\n' 'val tok' 'en = command.getString("id")' >"$test_dir/value.kt"
SECRET_SCAN_ROOT="$test_dir" sh "$scanner" >/dev/null \
    || fail "computed source values were treated as stored credentials"

for extension in rs py kt kts; do
    source_file="$test_dir/vendor/protocol/literal.$extension"
    printf '%s%s\n' 'tok' 'en = "fixture-credential"' >"$source_file"
    if SECRET_SCAN_ROOT="$test_dir" sh "$scanner" >/dev/null 2>&1; then
        fail "$extension literal credential was accepted"
    fi
    rm -f "$source_file"
done
printf 'ghp_%040d\n' 0 >"$test_dir/vendor/protocol/key.rs"
if SECRET_SCAN_ROOT="$test_dir" sh "$scanner" >/dev/null 2>&1; then
    fail "recognizable credential in vendored source was accepted"
fi
rm -f "$test_dir/vendor/protocol/key.rs"
assert_secret_rejected unquoted-config \
    sh -c 'printf "%s%s\n" "pass" "word=fixture-credential"'
assert_secret_rejected camel-case-config \
    sh -c 'printf "%s%s\n" "userTok" "en=fixture-credential"'
assert_secret_rejected bearer-token-shape \
    sh -c 'printf "%s%s.%s.%s\n" "ey" "JfixtureHeader0" "fixturePayload0" "fixtureSignature"'

# Only the exact tracked Penpot connection is exempt, and only from the generic
# assignment and token-shape rules.
rm -f "$test_dir/deploy/credential.txt"
mkdir -p "$test_dir/.codex"
approved="$test_dir/.codex/config.toml"
printf '%s%s%s.%s.%s"\n' 'url = "https://example.invalid/mcp?userTok' 'en=ey' \
    'JfixtureHeader0' 'fixturePayload0' 'fixtureSignature' >"$approved"
SECRET_SCAN_ROOT="$test_dir" sh "$scanner" >/dev/null \
    || fail "the approved Penpot connection was rejected"
mkdir -p "$test_dir/other/.codex"
cp "$approved" "$test_dir/other/.codex/config.toml"
if SECRET_SCAN_ROOT="$test_dir" sh "$scanner" >/dev/null 2>&1; then
    fail "the Penpot exemption matched a path other than the exact tracked file"
fi
rm -rf "$test_dir/other"
printf 'ghp_%040d\n' 0 >>"$approved"
if SECRET_SCAN_ROOT="$test_dir" sh "$scanner" >/dev/null 2>&1; then
    fail "a recognizable key inside the approved connection was accepted"
fi
rm -f "$approved"

echo "secret scan scope and credential fixtures verified"
