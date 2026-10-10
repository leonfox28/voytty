#!/bin/sh
# Run only inside a disposable Linux VM; its loopback must have no custom qdisc.
set -eu

case "$(uname -s)" in Linux) ;; *) echo 'Linux disposable VM required' >&2; exit 1;; esac
repeat=${1:-10}
case "$repeat" in ''|*[!0-9]*|0) echo 'positive repetition count required' >&2; exit 1;; esac
export CARGO_BUILD_JOBS=1
transport_test_binary=$(cargo test -p zterm-daemon --test two_daemon_transport --no-run --message-format=json |
    python3 -c 'import json, sys
artifacts = [json.loads(line) for line in sys.stdin]
print(next(item["executable"] for item in artifacts if item.get("target", {}).get("name") == "two_daemon_transport" and item.get("executable")))')
test -x "$transport_test_binary"
sudo tc qdisc add dev lo root netem delay 60ms
trap 'sudo tc qdisc del dev lo root' EXIT INT TERM
iteration=1
while [ "$iteration" -le "$repeat" ]; do
    echo "reverse-confirmation-delay iteration=$iteration delay=60ms"
    "$transport_test_binary" \
        reverse_confirmation_survives_losing_duplicate_arbitration --exact --nocapture
    iteration=$((iteration + 1))
done
