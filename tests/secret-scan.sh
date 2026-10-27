#!/bin/sh
set -eu

repo_root=${SECRET_SCAN_ROOT:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}
# The maintainer deliberately tracks this Penpot MCP connection and its token.
# The exemption covers only the generic assignment and token-shape rules below;
# a recognizable key in the same file is still rejected.
approved_connection="$repo_root/.codex/config.toml"

# Scan every source/deployment input, including vendored code. Build output,
# Git internals, and local runtime state are not inputs shipped to users.
scan() {
    grep -rIlE --exclude-dir='.git' --exclude-dir='target' --exclude-dir='.runtime' \
        "$@" "$repo_root"
}

scan_unapproved() {
    scan "$@" | grep -Fvx "$approved_connection"
}

found=0
if scan '(BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY|ssh-(rsa|ed25519) [A-Za-z0-9+/]{40,}|AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9]{36,})'; then
    found=1
fi
# Bare assignments in configuration and shell inputs may embed credentials,
# including camel-case names such as userToken or dbPassword.
# In Rust, Python and Kotlin, protocol expressions are often named token;
# inspect string literal assignments there without excluding the file.
if scan_unapproved -i --exclude='*.rs' --exclude='*.py' --exclude='*.kt' --exclude='*.kts' \
    '(password|token)[[:space:]]*=[[:space:]]*[^[:space:]#]+'; then
    found=1
fi
if scan_unapproved -i --include='*.rs' --include='*.py' --include='*.kt' --include='*.kts' \
    "(password|token)[[:space:]]*=[[:space:]]*(b|r|br|rb|f|fr|rf)?#*['\"][^[:space:]]+"; then
    found=1
fi
# JWT/JWE bearer values begin with a base64url JSON header.
if scan_unapproved 'eyJ[A-Za-z0-9_-]{10,}[.][A-Za-z0-9_-]{10,}[.][A-Za-z0-9_-]{10,}'; then
    found=1
fi
if [ "$found" -ne 0 ]; then
    echo "possible secret found in the voytty repository" >&2
    exit 1
fi

echo "repository secret scan passed"
