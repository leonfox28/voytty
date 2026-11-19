#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
relay_config="$repo_root/deploy/relay/relay.toml"
container_name="voytty-relay-smoke-$$"

relay_arch=$(docker version --format '{{.Server.Arch}}')
case "$relay_arch" in
    amd64 | arm64) ;;
    x86_64) relay_arch=amd64 ;;
    aarch64) relay_arch=arm64 ;;
    *)
        echo "unsupported Docker architecture: $relay_arch" >&2
        exit 64
        ;;
esac
image="voytty/iroh-relay:test-$relay_arch"
if ! docker image inspect "$image" >/dev/null 2>&1; then
    echo "missing $image; run tests/relay/build-platforms.sh first" >&2
    exit 1
fi

cleanup() {
    docker rm --force "$container_name" >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

docker run --detach --name "$container_name" \
    --read-only --cap-drop ALL --security-opt no-new-privileges:true --pids-limit 128 --memory 256m \
    --publish 127.0.0.1::38451 \
    --mount "type=bind,src=$relay_config,dst=/etc/iroh-relay/relay.toml,readonly" \
    "$image" >/dev/null

relay_binding=$(docker port "$container_name" 38451/tcp)
[ "${relay_binding%:*}" = "127.0.0.1" ]
relay_port=${relay_binding##*:}

attempt=0
until health=$(curl --fail --silent --show-error \
    "http://127.0.0.1:$relay_port/healthz" 2>/dev/null); do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 30 ]; then
        docker logs "$container_name" >&2
        echo "relay /healthz did not become ready" >&2
        exit 1
    fi
    sleep 1
done

printf '%s\n' "$health" | grep -Fq '"status":"ok"'
printf '%s\n' "$health" | grep -Fq '"version":"1.3.0"'
curl --fail --silent --show-error --output /dev/null \
    "http://127.0.0.1:$relay_port/generate_204"

sh "$repo_root/tests/relay/public-handshake.sh" "http://127.0.0.1:$relay_port"
echo "relay health and authenticated handshake passed"
