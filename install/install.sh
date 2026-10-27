#!/bin/sh
set -eu

# Defer the installation body until the function has been received in full.
main() {

repository="leonfox28/voytty"
version=""
install_dir="${VOYTTY_INSTALL_DIR:-}"
maximum_script_bytes=262144

usage() {
    cat <<'EOF'
Usage: install.sh [--version <vSEMVER>] [--install-dir <directory>]

Without --version, installs the latest stable GitHub Release. An exact stable
or prerelease tag may be selected explicitly. This bootstrap downloads and
runs the immutable installer from that Release; it never runs voytty setup.
VOYTTY_INSTALL_DIR sets the default destination; --install-dir takes precedence.
EOF
}

fail() {
    echo "voytty bootstrap failed: $*" >&2
    exit 1
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ "$#" -ge 2 ] || fail "--version requires a tag"
            version=$2
            shift 2
            ;;
        --install-dir)
            [ "$#" -ge 2 ] || fail "--install-dir requires a directory"
            install_dir=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "unknown argument: $1"
            ;;
    esac
done

if [ -n "$version" ]; then
    case "$version" in
        *[!A-Za-z0-9.-]*) fail "--version contains unsupported characters" ;;
    esac
    command -v awk >/dev/null 2>&1 || fail "awk is required to validate --version"
    version_value=${version#v}
    [ "$version_value" != "$version" ] || fail "--version must be a canonical v-prefixed SemVer tag"
    printf '%s\n' "$version_value" | awk '
        /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-((0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)(\.(0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*))?$/ { valid = 1 }
        END { exit valid ? 0 : 1 }
    ' || fail "--version must be a canonical v-prefixed SemVer tag"
    installer_url="https://github.com/$repository/releases/download/$version/voytty-install.sh"
else
    installer_url="https://github.com/$repository/releases/latest/download/voytty-install.sh"
fi

# Each asset has one wall-clock budget across at most four attempts. Never
# retry certificate/protocol failures, oversize responses or permanent HTTP errors.
download() {
    download_budget=60
    if [ "$3" -gt 1048576 ]; then download_budget=600; fi
    download_deadline=$(($(date +%s) + download_budget))
    download_attempt=0
    download_delay=1
    download_http=000
    download_status=28
    while :; do
        download_remaining=$((download_deadline - $(date +%s)))
        [ "$download_remaining" -gt 0 ] || break
        download_attempt=$((download_attempt + 1))
        download_status=0
        download_http=$(
            ulimit -f "$((($3 + 511) / 512))"
            curl --disable --proto '=https' --proto-redir '=https' --tlsv1.2 \
                --fail --location --max-redirs 5 --silent --show-error \
                --connect-timeout 10 --max-time "$download_remaining" \
                --speed-limit 1024 --speed-time 30 --max-filesize "$3" \
                --write-out '%{http_code}' --output "$2" "$1"
        ) || download_status=$?
        if [ "$download_status" -eq 0 ]; then return 0; fi
        case "$download_status:$download_http" in
            5:*|6:*|7:*|18:*|28:*|52:*|55:*|56:*|22:408|22:429|22:500|22:502|22:503|22:504) ;;
            *) break ;;
        esac
        [ "$download_attempt" -lt 4 ] || break
        download_remaining=$((download_deadline - $(date +%s)))
        [ "$download_remaining" -gt "$download_delay" ] || break
        printf 'Download interrupted; retrying in %s seconds (%s/4).\n' "$download_delay" "$download_attempt" >&2
        sleep "$download_delay"
        download_delay=$((download_delay * 2))
    done
    printf 'Download failed: %s (HTTP %s, curl status %s).\n' "$1" "${download_http:-000}" "${download_status:-28}" >&2
    return 1
}

for tool in curl date sleep mktemp chmod wc tr rm rmdir; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is required; install it with your system package manager"
done
printf 'Downloading voytty installer (%s)...\n' "${version:-latest stable}" >&2

temporary=$(mktemp -d "${TMPDIR:-/tmp}/voytty-bootstrap.XXXXXX") \
    || fail "unable to create a private temporary directory"
chmod 700 "$temporary" || fail "unable to secure the temporary directory"
cleanup() {
    rm -f "$temporary/voytty-install.sh"
    rmdir "$temporary" 2>/dev/null || true
}
trap cleanup 0
trap 'exit 1' HUP INT TERM

installer="$temporary/voytty-install.sh"
download "$installer_url" "$installer" "$maximum_script_bytes" \
    || fail "unable to download immutable installer"
script_bytes=$(wc -c < "$installer" | tr -d '[:space:]')
[ "$script_bytes" -gt 0 ] 2>/dev/null || fail "downloaded installer is empty"
[ "$script_bytes" -le "$maximum_script_bytes" ] 2>/dev/null \
    || fail "downloaded installer exceeds its size bound"

if [ -n "$install_dir" ]; then
    sh "$installer" --install-dir "$install_dir"
else
    sh "$installer"
fi

}

# Keep the invocation in a complete compound command when streamed into sh.
{
    main "$@"
}
