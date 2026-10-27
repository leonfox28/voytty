# Working on voytty

- Read `docs/development.md` for development commands and platform boundaries;
  read `docs/releasing.md` before release work. `docs/ci-release-optimization.md`
  records the measurements and rationale.
- Start substantive changes on a branch from current `origin/main`. Preserve
  unrelated work. Keep fixes scoped to the requested task.
- During editing, run targeted Clippy/tests for the affected crate. `just check-fast`
  is an optional broader checkpoint, not a mandatory step before every commit.
- Before pushing product, test, build, dependency, or workflow changes, run
  `just check`. For documentation-only changes, `just ci-policy` is sufficient
  locally; PR CI still requires dependency policy. `docs/relay.md` is consumed by
  relay tests and is not eligible for the documentation-only shortcut.
- Also run `just android-check` and `just mobile-core-check ios` when shared
  `client/core/proto/diagnostics`, patched Iroh/noq or Cargo dependencies change.
  These are compile checks. Update the private `voytty-app` core pin and run its
  `just android-check`, `just ios-device-build` and `just ios-check <simulator-uuid>`
  for mobile integration. Report unavailable platform evidence accurately.
- Prefer a visible foreground simulator for interactive review (Xcode 27 uses
  Device Hub). Command-line, XCTest and XCUITest debugging may also run headlessly,
  including while the Mac is locked, as authorized by the user. Record runtime
  and visual acceptance separately; do not block command-line checks on window access.
- Keep local and hosted checks in the shared `justfile` recipes. Do not skip
  tests, weaken assertions, or add automatic test retries to obtain a green run.
  Fix asynchronous tests by waiting for the state they assert, within their
  existing bounded deadline.
- Main must run the full CI graph and cold-build release candidates. Only main
  saves compiled dependency caches; PRs may restore them. Keep path-package
  outputs and native PR binaries out of uploaded caches/artifacts.
- Release preparation uses `just release-prepare VERSION`. Only when publication
  is requested, use `just release VERSION PR` or `just release-publish VERSION`.
  Preserve exact-SHA candidate selection, protected signing, installer checks,
  immutable assets, and the documented recovery path.
