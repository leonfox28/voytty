#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT HUP INT TERM
mkdir "$fixture/bin"
commit=0123456789abcdef0123456789abcdef01234567

fail() {
    echo "candidate fixture failed: $*" >&2
    exit 1
}

# Execute the production jq selection against REST-shaped responses, including
# pagination. No GitHub state, credentials, or official signing key is used.
cat >"$fixture/bin/gh" <<'GH'
#!/bin/sh
set -eu
query=
slurp=false
source="$CANDIDATE_FIXTURE/run.json"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --jq) query=$2; shift ;;
        --slurp) slurp=true ;;
        *'/actions/workflows/ci.yml/runs') source="$CANDIDATE_FIXTURE/runs.json" ;;
        *'/artifacts?'*) source="$CANDIDATE_FIXTURE/artifacts.json" ;;
    esac
    shift
done
if [ "$slurp" = true ]; then
    [ -z "$query" ] || { echo 'gh does not support --slurp with --jq' >&2; exit 1; }
    jq -s . "$source"
else
    jq -r "$query" "$source"
fi
GH
chmod 700 "$fixture/bin/gh"

jq -n --arg commit "$commit" '{
    id: 123, event: "push", head_branch: "main", head_sha: $commit,
    status: "completed", conclusion: "success", path: ".github/workflows/ci.yml",
    run_attempt: 3
}' >"$fixture/run.json"
jq -n --arg commit "$commit" '[1, 2] | .[] | {artifacts: [{
    id: (100 + .), name: ("release-candidate-" + $commit + "-" + tostring),
    expired: false, digest: ("sha256:" + ("ab" * 32)),
    workflow_run: {id: 123, head_branch: "main", head_sha: $commit}
}]}' >"$fixture/artifacts.json"

# The full dispatch must be discoverable alongside historical push evidence;
# newer PR, wrong-source and wrong-branch successes must not be selected.
jq -n --arg commit "$commit" '{workflow_runs: [
    {id: 1, event: "push", head_branch: "main", head_sha: $commit, created_at: "2026-01-01"},
    {id: 123, event: "workflow_dispatch", head_branch: "main", head_sha: $commit, created_at: "2026-01-02"},
    {id: 900, event: "pull_request", head_branch: "main", head_sha: $commit, created_at: "2026-01-03"},
    {id: 901, event: "workflow_dispatch", head_branch: "topic", head_sha: $commit, created_at: "2026-01-04"},
    {id: 902, event: "workflow_dispatch", head_branch: "main", head_sha: ("f" * 40), created_at: "2026-01-05"}
] | map(. + {status: "completed", conclusion: "success", html_url: ("https://example.test/runs/" + (.id | tostring))})}' >"$fixture/runs.json"
green=$(CANDIDATE_FIXTURE="$fixture" PATH="$fixture/bin:$PATH" \
    sh "$repo_root/tools/release/find-green-main-ci.sh" "$commit")
[ "$green" = '123 https://example.test/runs/123' ] || fail "exact full main validation lookup chose unrelated evidence"

lookup() {
    CANDIDATE_FIXTURE="$fixture" PATH="$fixture/bin:$PATH" \
        sh "$repo_root/tools/release/find-candidate.sh" 123 "$commit"
}

[ "$(lookup)" = 102 ] || fail "failed-job retry did not reuse the newest successful assembly"

jq '.event = "workflow_dispatch"' "$fixture/run.json" >"$fixture/dispatch.json"
mv "$fixture/dispatch.json" "$fixture/run.json"
[ "$(lookup)" = 102 ] || fail "exact full-dispatch candidate was rejected"

cp "$fixture/artifacts.json" "$fixture/valid.json"
jq '.artifacts |= map(.expired = true)' "$fixture/valid.json" >"$fixture/artifacts.json"
if lookup >"$fixture/stdout" 2>"$fixture/stderr"; then
    fail "expired candidates were accepted"
fi
jq '.artifacts |= map(.workflow_run.head_sha = ("f" * 40))' \
    "$fixture/valid.json" >"$fixture/artifacts.json"
if lookup >"$fixture/stdout" 2>"$fixture/stderr"; then
    fail "candidate from another source was accepted"
fi
jq '.artifacts |= map(.digest = null)' "$fixture/valid.json" >"$fixture/artifacts.json"
if lookup >"$fixture/stdout" 2>"$fixture/stderr"; then
    fail "candidate without a server digest was accepted"
fi
printf '%s\n' '{"artifacts":[]}' >"$fixture/artifacts.json"
if lookup >"$fixture/stdout" 2>"$fixture/stderr"; then
    fail "quick CI without a candidate was accepted"
fi
cp "$fixture/valid.json" "$fixture/artifacts.json"
jq '.event = "pull_request" | .head_branch = "feature"' \
    "$fixture/run.json" >"$fixture/not-main.json"
mv "$fixture/not-main.json" "$fixture/run.json"
if lookup >"$fixture/stdout" 2>"$fixture/stderr"; then
    fail "PR evidence was accepted as release authority"
fi

echo "exact main candidate lookup verified"
