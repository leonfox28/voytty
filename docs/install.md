# Installer contracts and release history

User instructions live in the [installation guide](../wiki/install.md). This
document records the trust model, release prerequisites, and historical
migration procedures for maintainers.

## What is authenticated

The small mutable repository bootstrap selects latest stable or an exact tag,
then downloads that Release's immutable `voytty-install.sh`. Initial installation
trusts the HTTPS-delivered scripts: the versioned script pins the manifest and
archive hashes before executing any candidate. A compromised script source can
replace those pins, so this is not an independent signing-key trust root.

Before publishing a binary, the versioned script checks target/support floor,
absolute directory shape and basic writability, bounded download sizes, exact
manifest/archive hashes, and the archive's single-file inventory. Only then
does it execute the candidate's side-effect-free self-check. The candidate
verifies the detached Ed25519 signature over the exact manifest bytes and
cross-checks version, target, source commit, wire/schema versions, and
release-key ID before an fsynced, atomic no-clobber install. GitHub immutable
Releases and provenance attestations additionally bind the tag and assets
inside GitHub's supply-chain audit layer.

The candidate's own signature check confirms release consistency; it cannot
independently establish the trustworthiness of code that is already executing.
During `voytty update`, the existing trusted binary verifies the new manifest's
signature and archive digest before it executes the downloaded candidate. That
update path has a previously established signing-key trust root.

For a manual audit, download `SHA256SUMS`, `voytty-release.json`, its `.sig`, the
target archive, and `voytty-install.sh` from one exact Release. Verify the listed
SHA-256 values (`shasum -a 256` on macOS or `sha256sum` on Linux) and the GitHub
attestation, inspect the versioned script, then run it. The authenticated
candidate's internal verifier can be exercised explicitly after the archive
digest is checked:

```bash
./voytty --internal-release-verify voytty-release.json voytty-release.json.sig
```

That command succeeds only when the candidate itself is the target/build
authenticated by the signed manifest. It is an audit entry point and remains
hidden from ordinary public help.

## One-time migration from the four-target updater

This historical procedure applies only between zterm releases. To move to
voytty, use the [fresh-install transition](#transition-from-zterm) instead.

Versions through **0.1.17** require exactly four manifest targets. They reject a
new three-target manifest before downloading an update, even on supported
Apple Silicon/Linux hosts. A future binary cannot change that already installed
verifier. The new verifier accepts historical signed inventories and the new
three-target inventory without a platform allowlist or count requirement;
a missing current target returns `unsupported_platform` to the CLI.

For the first migration, use the existing authenticated installer/recovery path:

1. Download and review the bootstrap from the selected historical zterm source
   tag, select that exact zterm release, and
   install it into an empty temporary directory with `--install-dir`. This
   authenticates the candidate before touching the current executable or daemon.
2. Stop the existing daemon with the old binary's `daemon stop`. If Sessions are
   active, finish them first; that old binary also has its historical `--force`
   option. The current CLI uses `-y`/`--yes` instead.
3. Retain the old executable at a fresh backup path, leaving its original
   destination empty. Do not run `uninstall` or remove `~/.zterm`: pairing and
   identity must remain intact.
4. Run the already authenticated candidate's
   `--internal-release-install <original-absolute-executable-path>` entry. It
   uses the existing fsynced, atomic no-clobber installer. If activation fails,
   restore the retained old executable; do not overwrite an unexpected file.
5. Check the installed `--version`, then resume normal use. Subsequent updates
   use `zterm update` again. Coordinate the CLI/daemon versions across connected
   hosts according to the existing release compatibility policy.

This is a one-time maintenance procedure using the existing verified activation
boundary, not a new background updater or identity reset. Historical Intel
installations stay on their last published compatible release.

## Release-operator checkpoint

The executable release runbook is [Release operations](releasing.md). This
section records the installer trust prerequisites rather than duplicating the
operator commands.

Before any formal tag is pushed, a repository administrator must have:

1. enabled immutable Releases and confirmed it with
   `gh api repos/leonfox28/voytty/immutable-releases`;
2. created a protected `release` Environment with the required reviewer;
3. generated the long-lived Ed25519 seed outside logs/artifacts, committed only
   its reviewed public key, and stored the lowercase seed as the Environment
   secret `ZTERM_RELEASE_SIGNING_KEY` (mapped to `VOYTTY_RELEASE_SIGNING_KEY`
   by the workflow; see [signing configuration](releasing.md#build-once-publish-the-verified-bytes));
4. reviewed a successful `ci.yml` push run on `main` for the exact commit,
   including the three native candidate builds and verified unsigned assembly.

The default workflow token cannot read the repository Administration setting,
so the workflow does not pretend to verify immutable Releases through an
underprivileged API call and does not request a PAT. The environment reviewer
must confirm that immutable Releases remain enabled before approving the sole
seed-bearing signing job.

CI never creates a tag. `release-prepare` prepares the version in the feature
PR, or creates a standalone version PR from main. `release-publish` tags only
after exact main CI and its retained candidate pass; `just release VERSION PR`
coordinates the required waits and merge. The tag workflow reuses that candidate,
tests three signed installers, round-trips one late draft, emits provenance
attestations, and requires immutable publication. Relay images are not published.

A normal key rotation first ships a binary that trusts the next reviewed key
through a manifest signed by the current key. If the current private key may be
compromised, freeze publication and use an independently reviewed recovery
release process; do not silently add another signature format or replace
immutable assets.

## Historical Android APK distribution

This section records the former combined native/Android release channel. New
desktop releases do not contain an APK; mobile apps are maintained separately
in the [private app repository](mobile-repository.md).

Download `voytty-android-arm64.apk` from the selected GitHub Release and allow your
browser/file manager to install it. Android 8.0+ and arm64 are required. The
package is `com.voytty.app`; updates preserve its pairing/settings data
using the existing certificate. `voytty-android.json` records source, versionCode,
certificate and digest. The detached `SHA256SUMS.sig` authenticates the complete
Release inventory with the same reviewed Ed25519 key as the native manifest.
The shell installer and `voytty update` remain native CLI commands.

## Transition from zterm

The user instructions have moved to the
[migration guide](../wiki/install.md#transition-from-zterm). This anchor remains
available for links printed by existing installers.
