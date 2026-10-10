#!/bin/sh
set -eu

repo_root=${SECRET_SCAN_ROOT:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}

# Scan every source/deployment input, including vendored code. Build output,
# Git internals, and local runtime state are not inputs shipped to users.
scan() {
    grep -rIlE --exclude-dir='.git' --exclude-dir='target' --exclude-dir='.runtime' \
        "$@" "$repo_root"
}

found=0
if scan '(BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY|ssh-(rsa|ed25519) [A-Za-z0-9+/]{40,}|AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9]{36,})'; then
    found=1
fi
# Bare assignments in configuration and shell inputs may embed credentials.
# In Rust, Python and Kotlin, protocol expressions are often named token;
# inspect string literal assignments there without excluding the file.
if scan --exclude='*.rs' --exclude='*.py' --exclude='*.kt' --exclude='*.kts' \
    '(password|token)[[:space:]]*=[[:space:]]*[^[:space:]#]+'; then
    found=1
fi
if scan --include='*.rs' --include='*.py' --include='*.kt' --include='*.kts' \
    "(password|token)[[:space:]]*=[[:space:]]*(b|r|br|rb|f|fr|rf)?#*['\"][^[:space:]]+"; then
    found=1
fi
if [ "$found" -ne 0 ]; then
    echo "possible secret found in the zterm repository" >&2
    exit 1
fi

echo "repository secret scan passed"
