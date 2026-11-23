#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
operator="$repo_root/tools/release/operator.sh"
test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT HUP INT TERM
fake_bin="$test_root/bin"
mkdir -p "$fake_bin"

fail() {
    echo "release operator fixture failed: $*" >&2
    exit 1
}

real_cargo=$(command -v cargo) || fail "cargo is required"
"$real_cargo" +1.98.0 --version >/dev/null 2>&1 \
    || fail "the pinned Cargo 1.98.0 toolchain is required"

cat >"$fake_bin/cargo" <<'FAKE_CARGO'
#!/bin/sh
set -eu
printf 'cargo %s\n' "$*" >>"$FAKE_TRACE"
case " $* " in
    *' run '*' validate-next-version '*)
        candidate=
        for argument in "$@"; do candidate=$argument; done
        [ "$candidate" = 0.1.10 ] || exit 1
        printf '%s\n' "next release version $candidate is valid"
        ;;
    *) exec "$REAL_CARGO" "$@" ;;
esac
FAKE_CARGO

cat >"$fake_bin/just" <<'FAKE_JUST'
#!/bin/sh
set -eu
printf 'just %s\n' "$*" >>"$FAKE_TRACE"
exit 97
FAKE_JUST

cat >"$fake_bin/gh" <<'FAKE_GH'
#!/bin/sh
set -eu
printf 'gh %s\n' "$*" >>"$FAKE_TRACE"
command_name=${1:-}
shift || true
case "$command_name" in
    auth) exit 0 ;;
    repo)
        case "$*" in
            *viewerPermission*) printf '%s\n' "${FAKE_RELEASE_PERMISSION:-WRITE}" ;;
            *) printf '%s\n' "$FAKE_REPOSITORY" ;;
        esac
        ;;
    release) exit 1 ;;
    pr)
        pr_command=${1:-}
        case "$pr_command" in
            view)
                merge=-
                if [ -f "$FAKE_STATE_DIR/merge" ]; then merge=$(cat "$FAKE_STATE_DIR/merge"); fi
                awk -F '\t' -v merge="$merge" \
                    '{printf "%s\t%s\t%s\t%s\t%s\t%s\n", $2, $5, $4, $6, merge, $3}' \
                    "$FAKE_STATE_DIR/pr-record"
                ;;
            checks) exit 0 ;;
            merge)
                expected=
                previous=
                for argument in "$@"; do
                    if [ "$previous" = --match-head-commit ]; then expected=$argument; fi
                    previous=$argument
                done
                head=$(awk -F '\t' '{print $5}' "$FAKE_STATE_DIR/pr-record")
                [ "$head" = "$expected" ] || exit 1
                parent=$(git rev-parse refs/remotes/origin/main)
                merge=$(printf '%s\n' 'Merge fixture PR' \
                    | git commit-tree "$head^{tree}" -p "$parent" -p "$head")
                git push --quiet origin "$merge:refs/heads/main"
                sed 's/OPEN/MERGED/' "$FAKE_STATE_DIR/pr-record" >"$FAKE_STATE_DIR/updated-pr"
                mv "$FAKE_STATE_DIR/updated-pr" "$FAKE_STATE_DIR/pr-record"
                printf '%s\n' "$merge" >"$FAKE_STATE_DIR/merge"
                ;;
            list)
                if [ -f "$FAKE_STATE_DIR/pr-record" ]; then
                    if [ "${FAKE_PR_FOLLOWS_REMOTE:-0}" = 1 ]; then
                        branch=$(awk -F '\t' '{print $3}' "$FAKE_STATE_DIR/pr-record")
                        head=$(git ls-remote --heads origin "refs/heads/$branch" | awk '{print $1}')
                        awk -v head="$head" 'BEGIN { FS = OFS = "\t" } { $5 = head; print }' \
                            "$FAKE_STATE_DIR/pr-record" >"$FAKE_STATE_DIR/updated-pr"
                        mv "$FAKE_STATE_DIR/updated-pr" "$FAKE_STATE_DIR/pr-record"
                    fi
                    cat "$FAKE_STATE_DIR/pr-record"
                fi
                ;;
            create)
                [ "${FAKE_PR_FAIL:-0}" != 1 ] || exit 1
                pr_sha=$(git rev-parse HEAD)
                if [ -n "${FAKE_PR_SHA:-}" ]; then
                    pr_sha=$FAKE_PR_SHA
                fi
                pr_branch=$(git branch --show-current)
                printf 'https://example.invalid/pull/1\tOPEN\t%s\tmain\t%s\tfalse\n' \
                    "$pr_branch" "$pr_sha" >"$FAKE_STATE_DIR/pr-record"
                [ "${FAKE_PR_AMBIGUOUS:-0}" != 1 ] || exit 1
                printf '%s\n' 'https://example.invalid/pull/1'
                ;;
            *) exit 1 ;;
        esac
        ;;
    api)
        case " $* " in
            *'/branches/main --jq .commit.sha'*) git rev-parse refs/remotes/origin/main ;;
            *'/branches/main/protection '*)
                printf '%s\n' "${FAKE_PROTECTION:-true}"
                ;;
            *'/releases?per_page=100'*)
                if [ "${FAKE_RELEASE_API_ERROR:-0}" = 1 ]; then
                    echo 'gh: service unavailable (HTTP 500)' >&2
                    exit 1
                elif [ "${FAKE_RELEASE_EXISTS:-0}" = 1 ]; then
                    printf '%s\n' '[[],[{"tag_name":"v0.1.10","draft":true}]]'
                else
                    printf '%s\n' '[[]]'
                fi
                ;;
            *'/actions/workflows/ci.yml/runs '*)
                if [ "${FAKE_GREEN_CI:-1}" = 1 ]; then
                    printf '%s\n' '123 https://example.invalid/actions/runs/123'
                fi
                ;;
            *'/actions/runs/123/artifacts?'*)
                if [ "${FAKE_CANDIDATE_MISSING:-0}" = 1 ]; then
                    printf '%s\n' '[{"artifacts":[]}]'
                else
                    head=$(git rev-parse HEAD)
                    printf '[{"artifacts":[{"id":789,"name":"release-candidate-%s-1","expired":false,"digest":"sha256:%064d","workflow_run":{"head_sha":"%s","head_branch":"main"}}]}]\n' \
                        "$head" 0 "$head"
                fi
                ;;
            *'/actions/runs/123 '*) printf '%s\n' 123 ;;
            *) exit 1 ;;
        esac
        ;;
    run)
        case "${1:-}" in
            list)
                case " $* " in
                    *'--event workflow_dispatch '*)
                        head=$(git rev-parse HEAD)
                        printf '[{"databaseId":123,"url":"https://example.invalid/actions/runs/123","headSha":"%s","headBranch":"main","event":"workflow_dispatch","status":"completed","conclusion":"success"}]\n' "$head"
                        ;;
                    *'--event pull_request '*) printf '%s\n' '122 https://example.invalid/actions/runs/122' ;;
                    *'--workflow ci.yml '*) printf '%s\n' '123 https://example.invalid/actions/runs/123' ;;
                    *) printf '%s\n' '456 https://example.invalid/actions/runs/456' ;;
                esac
                ;;
            watch) [ "${FAKE_WATCH_FAILURE:-}" != "${2:-}" ] ;;
            *) exit 1 ;;
        esac
        ;;
    *) exit 1 ;;
esac
FAKE_GH
chmod 700 "$fake_bin/cargo" "$fake_bin/just" "$fake_bin/gh"

seed="$test_root/seed"
git init --quiet --initial-branch=main "$seed"
git -C "$seed" config user.name 'Operator Fixture'
git -C "$seed" config user.email fixture@example.invalid
cat >"$seed/Cargo.toml" <<'MANIFEST'
[workspace]
members = ["crates/cli"]
resolver = "2"

[workspace.package]
version = "0.1.9"
edition = "2021"
MANIFEST
mkdir -p "$seed/crates/cli/src" "$seed/tests"
cat >"$seed/crates/cli/Cargo.toml" <<'MEMBER_MANIFEST'
[package]
name = "voytty-cli"
version.workspace = true
edition.workspace = true
MEMBER_MANIFEST
: >"$seed/crates/cli/src/lib.rs"
"$real_cargo" +1.98.0 generate-lockfile --manifest-path "$seed/Cargo.toml"
cat >"$seed/tests/workspace-version.sh" <<'VERSION_TEST'
#!/bin/sh
set -eu
manifest_version=$(sed -n '/^\[workspace.package\]$/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml)
lock_version=$(sed -n '/^name = "voytty-cli"$/{n;s/^version = "\([^"]*\)"/\1/p;}' Cargo.lock)
[ "$manifest_version" = "$lock_version" ]
if [ "${FIXTURE_VERSION_EXTRA:-0}" = 1 ]; then
    printf '%s\n' 'unexpected validator output' >unexpected-version-output
fi
VERSION_TEST
chmod 700 "$seed/tests/workspace-version.sh"
git -C "$seed" add Cargo.toml Cargo.lock crates tests/workspace-version.sh
git -C "$seed" commit --quiet -m baseline

new_case() {
    case_name=$1
    case_root="$test_root/$case_name"
    case_remote="$case_root/remote.git"
    case_worktree="$case_root/worktree"
    case_trace="$case_root/trace.log"
    case_state="$case_root/gh-state"
    mkdir -p "$case_root" "$case_state"
    git init --quiet --bare "$case_remote"
    git -C "$seed" push --quiet "$case_remote" main
    git --git-dir="$case_remote" symbolic-ref HEAD refs/heads/main
    git clone --quiet "$case_remote" "$case_worktree"
    git -C "$case_worktree" config user.name 'Operator Fixture'
    git -C "$case_worktree" config user.email fixture@example.invalid
    : >"$case_trace"
}

run_operator() {
    set -- "$@" sh "$operator" "$operator_command" "$operator_version"
    if [ "$operator_command" = finish ]; then set -- "$@" 1; fi
    env PATH="$fake_bin:$PATH" \
        REAL_CARGO="$real_cargo" \
        FAKE_TRACE="$case_trace" \
        FAKE_STATE_DIR="$case_state" \
        FAKE_REPOSITORY=fixture/voytty \
        VOYTTY_RELEASE_TEST_MODE=1 \
        VOYTTY_RELEASE_REPOSITORY=fixture/voytty \
        VOYTTY_RELEASE_WORKTREE="$case_worktree" \
        VOYTTY_RELEASE_POLL_ATTEMPTS=1 \
        VOYTTY_RELEASE_POLL_SECONDS=0 \
        "$@"
}

assert_no_remote_tag() {
    if git --git-dir="$case_remote" show-ref --tags --quiet; then
        fail "$1 created a remote tag"
    fi
}

new_case dirty
printf '%s\n' dirty >"$case_worktree/untracked"
operator_command=prepare
operator_version=0.1.10
if run_operator >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "dirty worktree was accepted"
fi
assert_no_remote_tag dirty

new_case behind
upstream="$case_root/upstream"
git clone --quiet "$case_remote" "$upstream"
git -C "$upstream" config user.name 'Operator Fixture'
git -C "$upstream" config user.email fixture@example.invalid
printf '%s\n' newer >"$upstream/remote-change"
git -C "$upstream" add remote-change
git -C "$upstream" commit --quiet -m newer
git -C "$upstream" push --quiet origin main
operator_command=prepare
operator_version=0.1.10
if run_operator >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "behind main was accepted"
fi
assert_no_remote_tag behind

new_case invalid-version
operator_command=prepare
operator_version=v0.2.0
if run_operator >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "noncanonical next version was accepted"
fi
assert_no_remote_tag invalid-version

new_case insufficient-draft-visibility
operator_command=prepare
operator_version=0.1.10
if run_operator FAKE_RELEASE_PERMISSION=READ >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "a reader without draft visibility proved release vacancy"
fi
grep -Fq 'write access to check existing drafts' "$case_root/stderr" || fail "draft visibility was not checked"
assert_no_remote_tag insufficient-draft-visibility

new_case existing-release
operator_command=prepare
operator_version=0.1.10
if run_operator FAKE_RELEASE_EXISTS=1 >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "existing Release was accepted"
fi
assert_no_remote_tag existing-release

new_case release-api-failure
operator_command=prepare
operator_version=0.1.10
if run_operator FAKE_RELEASE_API_ERROR=1 >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "Release API failure was treated as vacancy"
fi
grep -Fq 'could not prove GitHub Release v0.1.10 is vacant' "$case_root/stderr" \
    || fail "Release API failure did not fail closed at vacancy"
assert_no_remote_tag release-api-failure

new_case unprotected-main-prepare
operator_command=prepare
operator_version=0.1.10
if run_operator FAKE_PROTECTION=false >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "unprotected main was accepted during prepare"
fi
grep -Fq 'disable force pushes and deletion' "$case_root/stderr" \
    || fail "unprotected main prepare failed for an unexpected reason"
assert_no_remote_tag unprotected-main-prepare

new_case unprotected-main-publish
operator_command=publish
operator_version=0.1.9
if run_operator FAKE_PROTECTION=false >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "unprotected main was accepted during publish"
fi
grep -Fq 'disable force pushes and deletion' "$case_root/stderr" \
    || fail "unprotected main publish failed for an unexpected reason"
assert_no_remote_tag unprotected-main-publish

new_case existing-local-tag
git -C "$case_worktree" tag -a v0.1.10 -m 'existing fixture tag'
operator_command=prepare
operator_version=0.1.10
if run_operator >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "existing local tag was accepted"
fi
assert_no_remote_tag existing-local-tag

new_case existing-remote-tag
git -C "$case_worktree" tag -a v0.1.10 -m 'existing fixture tag'
git -C "$case_worktree" push --quiet origin refs/tags/v0.1.10
git -C "$case_worktree" tag --delete v0.1.10 >/dev/null
operator_command=prepare
operator_version=0.1.10
if run_operator >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "existing remote tag was accepted"
fi
[ "$(git --git-dir="$case_remote" cat-file -t refs/tags/v0.1.10)" = tag ] \
    || fail "existing annotated tag fixture was not preserved"

new_case partial-inventory
baseline_main=$(git --git-dir="$case_remote" rev-parse refs/heads/main)
operator_command=prepare
operator_version=0.1.10
if run_operator FIXTURE_VERSION_EXTRA=1 >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "unexpected release inventory was accepted"
fi
[ "$(git -C "$case_worktree" branch --show-current)" = main ] \
    || fail "inventory failure switched away from main"
[ -n "$(git -C "$case_worktree" status --porcelain)" ] \
    || fail "inventory failure did not retain the partial version diff"
inventory_diagnostic='release operator failed: release preparation inventory mismatch
expected:
Cargo.lock
Cargo.toml
actual:
Cargo.lock
Cargo.toml
unexpected-version-output'
grep -Fq "$inventory_diagnostic" "$case_root/stderr" \
    || fail "inventory failure did not print stable expected and actual sets"
[ "$(git --git-dir="$case_remote" rev-parse refs/heads/main)" = "$baseline_main" ] \
    || fail "inventory failure pushed main"
updates_before_resume=$(grep -Fc 'cargo +1.98.0 update --workspace' "$case_trace" || true)
if run_operator >"$case_root/resume-stdout" 2>"$case_root/resume-stderr"; then
    fail "dirty partial main checkout was auto-resumed"
fi
[ "$(grep -Fc 'cargo +1.98.0 update --workspace' "$case_trace" || true)" \
    -eq "$updates_before_resume" ] \
    || fail "dirty partial resume reran Cargo generation"
assert_no_remote_tag partial-inventory

new_case prepare-main-success
operator_command=prepare
operator_version=0.1.10
if ! run_operator >"$case_root/stdout" 2>"$case_root/stderr"; then
    cat "$case_root/stderr" >&2
    fail "valid main release preparation failed"
fi
[ "$(git -C "$case_worktree" branch --show-current)" = main ] \
    || fail "main prepare switched away from main"
[ "$(git --git-dir="$case_remote" rev-parse refs/heads/main)" = "$(git -C "$case_worktree" rev-parse HEAD)" ] \
    || fail "prepare did not push the release commit to main"
changed_files=$(git --git-dir="$case_remote" diff-tree --no-commit-id --name-only -r \
    refs/heads/main | LC_ALL=C sort)
[ "$changed_files" = "Cargo.lock
Cargo.toml" ] || fail "release commit changed files outside Cargo.toml and Cargo.lock"
lock_contents=$(git --git-dir="$case_remote" show refs/heads/main:Cargo.lock)
lock_version=$(printf '%s\n' "$lock_contents" \
    | sed -n '/^name = "voytty-cli"$/{n;s/^version = "\([^"]*\)"/\1/p;}')
[ "$lock_version" = 0.1.10 ] \
    || fail "real Cargo did not refresh the inherited workspace package version"
grep -Fxq 'cargo +1.98.0 update --workspace' "$case_trace" \
    || fail "prepare bypassed real Cargo workspace lock generation"
grep -Fxq 'cargo +1.98.0 metadata --locked --format-version 1 --no-deps' \
    "$case_trace" || fail "prepare did not perform locked metadata validation"
if grep -Fq 'just check' "$case_trace"; then
    fail "prepare duplicated the full quality gate"
fi
if grep -Fq 'gh pr create ' "$case_trace"; then
    fail "main prepare unexpectedly created a release PR"
fi
grep -Fq 'Next step: just release-publish 0.1.10' "$case_root/stdout" \
    || fail "main prepare did not print the next publish step"
if ! run_operator >"$case_root/idempotent-stdout" 2>"$case_root/idempotent-stderr"; then
    cat "$case_root/idempotent-stderr" >&2
    fail "already-pushed main release commit did not resume idempotently"
fi
[ "$(grep -Fc 'cargo +1.98.0 update --workspace' "$case_trace")" -eq 1 ] \
    || fail "idempotent main prepare regenerated the release lockfile"
assert_no_remote_tag prepare-main-success

new_case resume-main-push
operator_command=prepare
operator_version=0.1.10
if ! run_operator >"$case_root/seed-stdout" 2>"$case_root/seed-stderr"; then
    cat "$case_root/seed-stderr" >&2
    fail "could not create seed main release commit"
fi
release_sha=$(git -C "$case_worktree" rev-parse HEAD)
parent_sha=$(git -C "$case_worktree" rev-parse HEAD^)
git --git-dir="$case_remote" update-ref refs/heads/main "$parent_sha"
git -C "$case_worktree" update-ref refs/remotes/origin/main "$parent_sha"
: >"$case_trace"
if ! run_operator >"$case_root/resume-stdout" 2>"$case_root/resume-stderr"; then
    cat "$case_root/resume-stderr" >&2
    fail "exact unpushed main release commit did not resume push"
fi
[ "$(git --git-dir="$case_remote" rev-parse refs/heads/main)" = "$release_sha" ] \
    || fail "main push resume did not publish the exact release commit"
[ "$(grep -Fc 'cargo +1.98.0 update --workspace' "$case_trace" || true)" -eq 0 ] \
    || fail "main push resume regenerated the release lockfile"
assert_no_remote_tag resume-main-push

new_case extra-unpushed-main-commit
printf '%s\n' extra >"$case_worktree/extra.txt"
git -C "$case_worktree" add extra.txt
git -C "$case_worktree" commit --quiet -m 'unpushed product change'
extra_parent=$(git -C "$case_worktree" rev-parse HEAD)
sed 's/version = "0.1.9"/version = "0.1.10"/' "$case_worktree/Cargo.toml" >"$case_worktree/Cargo.toml.next"
mv "$case_worktree/Cargo.toml.next" "$case_worktree/Cargo.toml"
"$real_cargo" +1.98.0 update --workspace --manifest-path "$case_worktree/Cargo.toml"
git -C "$case_worktree" add Cargo.toml Cargo.lock
git -C "$case_worktree" commit --quiet -m 'chore: prepare v0.1.10 release'
operator_command=prepare
operator_version=0.1.10
if run_operator >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "prepare pushed an unrelated unpushed main commit"
fi
grep -Fq "expected $(git -C "$case_worktree" rev-parse HEAD) or its parent $extra_parent" "$case_root/stderr" \
    || fail "extra unpushed main commit failed for an unexpected reason"
assert_no_remote_tag extra-unpushed-main-commit

new_case divergent-main-remote
operator_command=prepare
operator_version=0.1.10
run_operator >"$case_root/seed-stdout" 2>"$case_root/seed-stderr" \
    || fail "could not seed main release commit"
parent_sha=$(git -C "$case_worktree" rev-parse HEAD^)
git --git-dir="$case_remote" update-ref refs/heads/main "$parent_sha"
git -C "$case_worktree" update-ref refs/remotes/origin/main "$parent_sha"
diverter="$case_root/diverter"
git clone --quiet "$case_remote" "$diverter"
git -C "$diverter" config user.name 'Operator Fixture'
git -C "$diverter" config user.email fixture@example.invalid
printf '%s\n' divergent >"$diverter/divergent-change"
git -C "$diverter" add divergent-change
git -C "$diverter" commit --quiet -m 'diverge main'
git -C "$diverter" push --quiet origin main
divergent_sha=$(git -C "$diverter" rev-parse HEAD)
if run_operator >"$case_root/resume-stdout" 2>"$case_root/resume-stderr"; then
    fail "divergent remote main was accepted"
fi
[ "$(git --git-dir="$case_remote" rev-parse refs/heads/main)" = "$divergent_sha" ] \
    || fail "operator overwrote divergent remote main"
assert_no_remote_tag divergent-main-remote

new_case wrong-release-subject
sed 's/version = "0.1.9"/version = "0.1.10"/' "$case_worktree/Cargo.toml" >"$case_worktree/Cargo.toml.next"
mv "$case_worktree/Cargo.toml.next" "$case_worktree/Cargo.toml"
"$real_cargo" +1.98.0 update --workspace --manifest-path "$case_worktree/Cargo.toml"
git -C "$case_worktree" add Cargo.toml Cargo.lock
git -C "$case_worktree" commit --quiet -m 'wrong release subject'
operator_command=prepare
operator_version=0.1.10
if run_operator >"$case_root/resume-stdout" 2>"$case_root/resume-stderr"; then
    fail "release commit with the wrong subject was accepted"
fi
grep -Fq "release commit subject is 'wrong release subject'" \
    "$case_root/resume-stderr" \
    || fail "wrong release subject failed for an unexpected reason"
assert_no_remote_tag wrong-release-subject

new_case created-pr-sha-mismatch
git -C "$case_worktree" switch --quiet -c release/v0.1.10
operator_command=prepare
operator_version=0.1.10
mismatched_pr_sha=0000000000000000000000000000000000000000
if run_operator FAKE_PR_SHA="$mismatched_pr_sha" \
    >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "newly created PR with a mismatched head SHA was accepted"
fi
grep -Fq "existing PR head is $mismatched_pr_sha" "$case_root/stderr" \
    || fail "newly created PR identity was not read back and checked"
[ "$(grep -Fc 'gh pr list ' "$case_trace")" -eq 2 ] \
    || fail "newly created PR was not queried after creation"
[ "$(grep -Fc 'gh pr create ' "$case_trace")" -eq 1 ] \
    || fail "mismatched newly created PR was duplicated"
assert_no_remote_tag created-pr-sha-mismatch

new_case resume-existing-pr
git -C "$case_worktree" switch --quiet -c release/v0.1.10
operator_command=prepare
operator_version=0.1.10
if run_operator FAKE_PR_AMBIGUOUS=1 \
    >"$case_root/first-stdout" 2>"$case_root/first-stderr"; then
    fail "ambiguous PR creation unexpectedly succeeded"
fi
[ -z "$(git -C "$case_worktree" status --porcelain --untracked-files=all)" ] \
    || fail "ambiguous PR result did not retain a clean release commit"
local_release_sha=$(git -C "$case_worktree" rev-parse HEAD)
remote_release_sha=$(git --git-dir="$case_remote" \
    rev-parse refs/heads/release/v0.1.10)
[ "$local_release_sha" = "$remote_release_sha" ] \
    || fail "ambiguous PR result did not retain the exact remote branch"
grep -Fq 'rerun: just release-prepare 0.1.10' "$case_root/first-stderr" \
    || fail "ambiguous PR result did not print the bounded resume command"
if ! run_operator >"$case_root/resume-stdout" 2>"$case_root/resume-stderr"; then
    cat "$case_root/resume-stderr" >&2
    fail "exact release commit did not resume"
fi
[ "$(grep -Fc 'cargo +1.98.0 update --workspace' "$case_trace")" -eq 1 ] \
    || fail "resume regenerated the release lockfile"
[ "$(grep -Fc 'gh pr create ' "$case_trace")" -eq 1 ] \
    || fail "resume created a duplicate PR"
[ "$(grep -Fc 'gh pr list ' "$case_trace")" -eq 2 ] \
    || fail "prepare did not reconcile PR state on both attempts"
grep -Fq 'Release PR: https://example.invalid/pull/1' "$case_root/resume-stdout" \
    || fail "resume did not return the existing PR URL"
assert_no_remote_tag resume-existing-pr

new_case divergent-remote
git -C "$case_worktree" switch --quiet -c release/v0.1.10
operator_command=prepare
operator_version=0.1.10
if run_operator FAKE_PR_FAIL=1 >"$case_root/first-stdout" 2>"$case_root/first-stderr"; then
    fail "fixture PR failure unexpectedly succeeded"
fi
diverter="$case_root/diverter"
git clone --quiet "$case_remote" "$diverter"
git -C "$diverter" config user.name 'Operator Fixture'
git -C "$diverter" config user.email fixture@example.invalid
git -C "$diverter" switch --quiet release/v0.1.10
printf '%s\n' divergent >"$diverter/divergent-change"
git -C "$diverter" add divergent-change
git -C "$diverter" commit --quiet -m 'diverge release branch'
git -C "$diverter" push --quiet origin release/v0.1.10
divergent_sha=$(git -C "$diverter" rev-parse HEAD)
if run_operator >"$case_root/resume-stdout" 2>"$case_root/resume-stderr"; then
    fail "divergent remote release branch was accepted"
fi
grep -Fq "remote branch release/v0.1.10 points to $divergent_sha" \
    "$case_root/resume-stderr" \
    || fail "remote divergence did not report the conflicting SHA"
[ "$(git --git-dir="$case_remote" rev-parse refs/heads/release/v0.1.10)" \
    = "$divergent_sha" ] || fail "operator overwrote the divergent remote branch"
assert_no_remote_tag divergent-remote

new_case closed-release-pr
git -C "$case_worktree" switch --quiet -c release/v0.1.10
operator_command=prepare
operator_version=0.1.10
if run_operator FAKE_PR_AMBIGUOUS=1 \
    >"$case_root/first-stdout" 2>"$case_root/first-stderr"; then
    fail "ambiguous PR creation unexpectedly succeeded"
fi
awk 'BEGIN { FS = OFS = "\t" } { $2 = "CLOSED"; print }' \
    "$case_state/pr-record" >"$case_state/pr-record.closed"
mv "$case_state/pr-record.closed" "$case_state/pr-record"
if run_operator >"$case_root/resume-stdout" 2>"$case_root/resume-stderr"; then
    fail "closed release PR was reused"
fi
grep -Fq 'existing PR for release/v0.1.10 is not open' \
    "$case_root/resume-stderr" \
    || fail "closed release PR failed for an unexpected reason"
[ "$(grep -Fc 'gh pr create ' "$case_trace")" -eq 1 ] \
    || fail "closed PR state created a competing PR"
assert_no_remote_tag closed-release-pr

new_case missing-green-ci
operator_command=publish
operator_version=0.1.9
if run_operator FAKE_GREEN_CI=0 >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "publish accepted a commit without exact green main CI"
fi
assert_no_remote_tag missing-green-ci

new_case publish-success
operator_command=publish
operator_version=0.1.9
if ! run_operator FAKE_GREEN_CI=1 >"$case_root/stdout" 2>"$case_root/stderr"; then
    cat "$case_root/stderr" >&2
    fail "valid release publication failed"
fi
[ "$(git --git-dir="$case_remote" cat-file -t refs/tags/v0.1.9)" = tag ] \
    || fail "publish did not create an annotated tag"
[ "$(grep -Fc 'gh run watch 456 ' "$case_trace")" -eq 1 ] \
    || fail "publish did not watch exactly one formal release run"

published_tag=$(git --git-dir="$case_remote" rev-parse refs/tags/v0.1.9)
run_operator >"$case_root/resume-stdout" 2>"$case_root/resume-stderr" \
    || fail "publication could not resume its exact existing tag"
[ "$(git --git-dir="$case_remote" rev-parse refs/tags/v0.1.9)" = "$published_tag" ] \
    || fail "publication resume replaced its annotated tag"

new_case missing-candidate
operator_command=publish
operator_version=0.1.9
if run_operator FAKE_CANDIDATE_MISSING=1 >"$case_root/stdout" 2>"$case_root/stderr"; then
    fail "publication accepted green CI without its exact candidate"
fi
assert_no_remote_tag missing-candidate

new_case feature-pr-release
git -C "$case_worktree" switch --quiet -c fix/one-release-pr
printf '%s\n' 'reviewed product fix' >"$case_worktree/fix.txt"
git -C "$case_worktree" add fix.txt
git -C "$case_worktree" commit --quiet -m 'fix: reviewed product behavior'
operator_command=prepare
operator_version=0.1.10
run_operator >"$case_root/prepare-stdout" 2>"$case_root/prepare-stderr" \
    || fail "version could not be prepared in the existing feature branch"
[ "$(git -C "$case_worktree" branch --show-current)" = fix/one-release-pr ] \
    || fail "preparation created a second release branch"
if git --git-dir="$case_remote" show-ref --verify --quiet refs/heads/release/v0.1.10; then
    fail "feature preparation pushed a competing version branch"
fi
grep -Fq 'gh pr create --repo fixture/voytty --base main --head fix/one-release-pr --fill' \
    "$case_trace" || fail "feature PR did not retain its product change description"
assert_no_remote_tag feature-pr-release

operator_command=finish
reviewed_pr_record=$(cat "$case_state/pr-record")
awk -v head="$(git -C "$case_worktree" rev-parse HEAD^)" \
    'BEGIN { FS = OFS = "\t" } { $5 = head; print }' \
    "$case_state/pr-record" >"$case_state/updated-pr"
mv "$case_state/updated-pr" "$case_state/pr-record"
if run_operator >"$case_root/mismatch-stdout" 2>"$case_root/mismatch-stderr"; then
    fail "finish accepted a PR head different from the reviewed checkout"
fi
if grep -Fq 'gh pr merge ' "$case_trace"; then
    fail "finish tried to merge an unreviewed PR head"
fi
printf '%s\n' "$reviewed_pr_record" >"$case_state/pr-record"
if run_operator FAKE_WATCH_FAILURE=123 >"$case_root/failed-stdout" 2>"$case_root/failed-stderr"; then
    fail "finish ignored failed main CI"
fi
assert_no_remote_tag failed-main-after-merge
[ "$(grep -Fc 'gh pr merge ' "$case_trace")" -eq 1 ] \
    || fail "finish did not merge the reviewed head once"
if ! run_operator >"$case_root/finish-stdout" 2>"$case_root/finish-stderr"; then
    cat "$case_root/finish-stderr" >&2
    fail "finish could not resume from the merged PR"
fi
[ "$(git --git-dir="$case_remote" rev-list -n 1 v0.1.10)" = "$(cat "$case_state/merge")" ] \
    || fail "finish tagged a source other than the PR merge commit"
[ "$(git -C "$case_worktree" branch --show-current)" = fix/one-release-pr ] \
    || fail "finish switched the caller's branch"
[ "$(git -C "$case_worktree" worktree list --porcelain | grep -c '^worktree ')" -eq 1 ] \
    || fail "finish leaked its private checkout"
run_operator >"$case_root/finished-stdout" 2>"$case_root/finished-stderr" \
    || fail "completed release could not be rejoined"
[ "$(grep -Fc 'gh pr merge ' "$case_trace")" -eq 1 ] \
    || fail "finish resume merged a second time"

new_case already-open-feature-pr
git -C "$case_worktree" switch --quiet -c fix/open-pr
printf '%s\n' 'reviewed product fix' >"$case_worktree/fix.txt"
git -C "$case_worktree" add fix.txt
git -C "$case_worktree" commit --quiet -m 'fix: reviewed product behavior'
git -C "$case_worktree" push --quiet --set-upstream origin fix/open-pr
feature_head=$(git -C "$case_worktree" rev-parse HEAD)
printf 'https://example.invalid/pull/1\tOPEN\tfix/open-pr\tmain\t%s\tfalse\n' \
    "$feature_head" >"$case_state/pr-record"
operator_command=prepare
operator_version=0.1.10
run_operator FAKE_PR_FOLLOWS_REMOTE=1 >"$case_root/stdout" 2>"$case_root/stderr" \
    || fail "could not append version preparation to an already open feature PR"
[ "$(git --git-dir="$case_remote" rev-parse refs/heads/fix/open-pr)" = "$(git -C "$case_worktree" rev-parse HEAD)" ] \
    || fail "existing feature PR did not receive the version commit"
[ "$(git -C "$case_worktree" rev-parse HEAD^)" = "$feature_head" ] \
    || fail "version preparation rewrote the existing feature history"
if grep -Fq 'gh pr create ' "$case_trace"; then
    fail "version preparation created a second PR"
fi
assert_no_remote_tag already-open-feature-pr

echo "release operator prepare, finish, and recovery fixtures passed without external state"
