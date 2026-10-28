# Release operations

The publication scope is **macOS arm64, Linux arm64/x64, and Windows x86-64/ARM64**.
macOS Intel and relay image publication remain paused. Existing
immutable Releases and already published relay images remain historical assets.
This platform matrix belongs to the release tool. Installed updaters require
and validate only their own target entry in the authenticated manifest; they
do not enforce the presence, number, or support policy of other platforms.

```text
reviewed feature branch + version commit in one PR
  -> PR CI -> merge the exact reviewed head
  -> explicit exact-SHA full main CI + cold native builds + unsigned candidate
  -> annotated vVERSION tag -> select that main candidate by artifact ID
  -> protected signing -> five final installer proofs -> immutable Release
```

`VERSION` is canonical SemVer without the `v` prefix or build metadata.
Prereleases such as `0.2.0-rc.1` are supported. Run `just doctor` first. The
operator never installs global tools or changes repository protection settings.

## Prepare one reviewable PR

For an immediately released fix, commit the reviewed product changes on a clean
feature branch that contains current `origin/main`, then run (preferably before
the first PR push, or on an already open feature PR):

```bash
just release-prepare 0.2.0
```

This keeps the current feature branch, updates only the workspace version and
Cargo-generated lockfile, adds one version commit, pushes the same branch, and
creates or reuses its PR. New feature PRs derive their title/body from the
product commits. An existing PR keeps its description. Code and version are
reviewed together, so no second version PR/CI cycle is needed.

When accumulating changes for a later release, the same command from clean,
exact `origin/main` creates a standalone `release/v0.2.0` branch and version PR.

Cargo's `semver` owner validates the next version. `cargo +1.98.0 update
--workspace` owns lockfile generation; locked metadata, workspace-version checks,
and the exact two-file version-commit inventory validate it. Preparation does
not repeat `just check`; the required PR `CI gate` owns the full remote gate.

If generation fails, inspect the retained dirty diff. Resume after a push/PR
network ambiguity is accepted only from the exact clean version commit on the
same branch, still containing current main. Remote branch and open-PR identities
must agree; divergence, a moved main, or closed/ambiguous PR state is rejected.

## Finish an authorized release

Once the PR is reviewed and publication is authorized:

```bash
just release 0.2.0 42
```

This command is an explicit request to merge and publish PR 42. It requires the
local clean HEAD to equal the open PR head and contain the requested version,
waits for required PR checks, merges with `--match-head-commit`, and uses a
private detached worktree to request/rejoin full validation for that exact main
merge SHA before publishing. It
preserves the caller's branch and removes only its own clean temporary worktree.
The normal repository PR/protection rules still apply; no admin bypass is used.

The PR, merge SHA, Actions run and annotated tag are the recovery record. Rerun
the same command after an interrupted wait; an already merged PR is not merged
again, and an already pushed matching tag rejoins its original workflow. There
is no parallel local release database. Watchers use GitHub CLI's compact view.

Publication remains available independently from a clean checkout of exact
`origin/main`:

```bash
just release-publish 0.2.0
```

Before a new tag is pushed, this checks version, enforced main protection,
Release vacancy, completed successful exact-SHA full `ci.yml` evidence on main,
and an unexpired candidate with a server digest. It requests or rejoins that full
validation before tagging; the routine quick main check is insufficient. If main has advanced beyond the
reviewed merge, it stops before tagging instead of including unreviewed changes.
A retained local annotated tag may be pushed only if it already names that exact
source; remote tags and Release assets are never replaced.

## Build once, publish the verified bytes

Ordinary PRs run affected owners; routine main pushes do not build candidates.
`just full-check` explicitly dispatches the complete graph on clean exact main,
without publishing. The dispatch carries the expected SHA and rejects a moved
main. Release commands reuse an existing full run for that SHA or request one;
a failed run stops publication and is never automatically retried.

The full run builds all five shipped native binaries: Linux uses the pinned
glibc 2.28 builder and macOS declares the 13.0 deployment floor. These jobs embed
the exact source SHA, self-check and run the CLI without arguments. PRs changing
build/release inputs also run those packaging checks, but never upload binaries.
Main full-validation candidates remain cold builds; PR packaging checks may use
trimmed dependency caches. The Rust suite retains synthetic unsigned assembly,
reproducibility, mixed-source and tampered-input tests.

One Ubuntu job creates deterministic archives, the manifest, installer and Rust SBOM,
verifies the unsigned inventory, and ShellChecks the generated installer. The
candidate uses the source commit timestamp and is uploaded as
`release-candidate-SHA-ATTEMPT`, retained for seven days. The full-validation `CI gate`
requires this job to succeed; the ordinary main gate cannot supply a candidate.

A failed-job rerun may reuse successful native jobs from an earlier attempt.
Intermediate per-target uploads may therefore be replaced within that same CI
run; the assembled candidate always receives a new immutable artifact ID. The
lookup chooses the newest retained assembly from the **exact successful full main
run**, never a PR run or another commit. Tag validation fixes that artifact ID,
and the downloader treats digest mismatches as errors.

The tag workflow does not rebuild `voytty` or rerun ordinary CI. It rebuilds the
small reviewed signing tool before exposing the key, re-verifies the candidate
and source SHA, signs the exact native manifest and complete
SHA256SUMS inventory, and tests the final signed installers
on all five targets. It then creates one late draft, downloads and verifies its
assets, attests those exact bytes, publishes, and requires `immutable: true`.
The twelve assets are the five native archives, manifest and signature, POSIX and
PowerShell installers,
Rust SBOM, and signed SHA256SUMS inventory. No mobile app or relay image is published.
The native schema-v1 manifest/installer and protected signing checks stay unchanged.

Only the signing job uses the protected `release`
Environment. Confirm immutable Releases remain enabled, inspect the exact green
main/candidate status, and approve access to the signing key. No second signing
approval or extra full test suite is introduced.

## Mobile repository boundary

Android and iOS source, build tooling and mobile signing helpers now live in
[private voytty-app](https://github.com/leonfox28/voytty-app). Public desktop
releases no longer require an APK or Android signing secrets. Existing immutable
Releases, tags, certificate pins and repository secrets remain untouched.
Historical mixed native/Android draft recovery still checks out and executes the
verifier from the original release SHA. Do not use a new desktop-only verifier
against an old mixed inventory. See [repository boundaries](mobile-repository.md).

## Failure and recovery

- Dirty preparation retains the local diff for diagnosis; it is not an automatic
  recovery checkpoint.
- Failed PR/main checks stop the operator. Fix the owning failure; no tag is
  pushed. Failed-job reruns can retain the same successful candidate.
- An expired/deleted candidate fails before a new tag. Run `just full-check` for the
  exact current main source, then retry; publication never silently compiles a replacement.
- An interrupted tag push is reconciled against the exact annotated source.
  An existing remote tag rejoins the same `release.yml` run.
- Before draft creation, a failed release job can be rerun for the same tag.
  Signed inventories and fixture uploads use attempt-specific names; downstream
  jobs use the signing job's artifact IDs, including when only failed jobs rerun.
  After draft creation, inspect the nonpublic evidence and use the explicit
  [draft recovery](#recover-an-existing-signed-draft) entry below;
  automation never deletes drafts, clobbers assets, or force-moves tags.
- A defect in an immutable published Release needs a new version and tag.
- Removing Intel changes the manifest target inventory. Versions through
  0.1.17 require all four old targets and cannot update directly from a
  three-target manifest. Follow the one-time binary migration in
  [Install, update, and uninstall](install.md#one-time-migration-from-the-four-target-updater).

Repository administrators own immutable Releases, the signing Environment/key,
and [main protection](development.md#branch-protection-and-ongoing-checks). The operator
checks visible prerequisites and never changes those settings.

## Resume a signed inventory after a Windows fixture failure

A defect in the test harness can stop the original tag workflow after protected
signing but before a draft exists. Repair and review the harness on main first;
never move the tag or replace the retained signed product files. Windows CI now
exercises the HTTPS fixture independently, including its temporary user trust
certificate and cleanup, so an interactive certificate prompt fails before release.

After inspecting and authorizing the original release evidence, explicitly run:

```bash
gh workflow run release-resume-signed.yml --ref main \
  -f tag=v1.0.0 -f run_id=40647210683 -f artifact_id=12797742664
```

These IDs identify the first voytty release's Windows fixture timeout. The owner
requires the original annotated source, successful protected signing and Unix
installer jobs, terminal Windows failures, skipped publication, and the original
unexpired signed artifact with a server digest. Cancelled Windows jobs require
GitHub's job-timeout annotation; manual cancellation is not a substitute.

The original frozen verifier authenticates the retained inventory and generates
fixture-only installers. All five native installer proofs run again on those
exact signed bytes; Windows uses the reviewed harness repair without dropping
assertions. Only then does the workflow create a late draft, verify its complete
download with the frozen verifier, attest, publish and require immutable assets.
It never accesses a signing key, rebuilds a product binary, changes a tag or
replaces assets. Exact-source main CI remains mandatory. This is a separate
completion record; the original failed run remains intact.

If interrupted after uploading a complete draft, inspect that draft by numeric ID
and rejoin this explicit workflow. It accepts an existing draft only when its
tag, source, classification and complete asset bytes match the retained signed
inventory; it never uploads over existing assets. Partial or mismatched drafts
stop for diagnosis. After any ambiguous publish result, inspect publication
state first; already published releases are rejected. The historical draft
recovery below retains its existing evidence and verifier requirements.

## Recover an existing signed draft

If signing and every installer job from that release passed, but publication failed after
uploading the complete draft (for example, GitHub returned HTTP 500 during
round-trip download), retain the original tag, signed artifact and draft.
Inspect the failed run and the draft by numeric release ID. A draft can return
404 from the release-by-tag REST endpoint while remaining visible through the
release list, release ID and `gh release view`; that 404 does not prove vacancy.

After reviewing the original evidence and authorizing publication, dispatch
the recovery workflow from protected main with the four exact identities:

```bash
gh workflow run release-recover.yml --ref main \
  -f tag=v0.1.35 -f run_id=35559736119 \
  -f artifact_id=10621537362 -f release_id=392689610
```

These example IDs identify the v0.1.35 incident; subsequent recoveries require
their own IDs. Recovery rejects an already published release. It requires the
original annotated tag, successful exact source main CI, successful source,
signing and installer jobs, a retained signed artifact with a server digest,
and the unchanged uploaded draft assets. The original frozen release verifier
authenticates the retained signed inventory and the downloaded draft bytes.
Downloads retry HTTP 429/500/502/503/504 and timeouts at most three times, using a
fresh temporary directory each time. Permanent errors and corrupt inventories
stop immediately.

Recovery attests the verified draft bytes, rechecks tag and asset identities,
then publishes the same draft by numeric ID and requires immutable status with
the same asset IDs/digests. It does not build the product, access signing keys,
create a release, upload assets or change a tag. Its run is the completion
record; the original failed run remains evidence of the interruption. Before
the final PATCH, failures leave the draft intact and the same dispatch can be
retried after inspection. If PATCH or post-publication verification has an
ambiguous result, inspect the release ID before any further action; published
assets are never replaced.

## Windows candidate and installer boundary

The x86-64/ARM64 jobs build on matching Windows hosts through `just windows-build`,
check the PE machine type and embedded source SHA, and run the candidate CLI.
They use no compiled dependency cache. PR binaries stay on the runner; only the
explicit exact-main full run uploads candidates. Ubuntu assembles deterministic
`.tar.gz` archives containing exactly `voytty.exe`; the manifest and signed
checksum inventory include both new targets and `voytty-install.ps1`.

The Windows final installer proofs run on both native architectures over a local
HTTPS fixture, with a temporary current-user trust certificate that is removed
on exit. They verify Unicode/space-containing destinations, no-clobber behavior,
manifest/signature tampering, and the production URL boundary. The protected
signing key, exact candidate selection, immutable publication and draft recovery
remain shared with Unix. Windows inventories require both Windows installer jobs;
historical drafts retain their original installer matrix and frozen source verifier.
