# Working on voytty

- Read `docs/development.md` for development commands and platform boundaries;
  read `docs/releasing.md` before release work. `docs/ci-release-optimization.md`
  records the measurements and rationale.
- Start substantive changes on a branch from current `origin/main`. Preserve
  unrelated work. Keep fixes scoped to the requested task.
- During editing, run targeted Clippy/tests for the affected crate. Before an
  ordinary push, run `just check-changes` against current `origin/main`; it includes
  uncommitted and untracked changes. `just check` remains a full local checkpoint.
- PR CI selects affected owners: Unix Rust, patched transport, shared mobile
  targets, Relay, and native packaging. Unknown inputs run all owners. Shared
  public changes must also be integrated and tested with the pinned private app.
  Report unavailable local platform checks accurately; hosted CI owns other OSes.
- Prefer a visible foreground simulator for interactive review (Xcode 27 uses
  Device Hub). Command-line, XCTest and XCUITest debugging may also run headlessly,
  including while the Mac is locked, as authorized by the user. Record runtime
  and visual acceptance separately; do not block command-line checks on window access.
- Keep local and hosted checks in shared `justfile` recipes. Run every selected
  check; do not disable tests, weaken assertions, or add automatic test retries
  to obtain a green run.
  Fix asynchronous tests by waiting for the state they assert, within their
  existing bounded deadline.
- Keep lightweight PRs and the required `CI gate`; ordinary main pushes run only
  quick policy/dependency checks. `just full-check` explicitly runs the entire
  graph and cold-builds unsigned candidates on exact main without publishing.
- Only reviewed main saves compiled dependency caches; PRs may restore them.
  Keep path-package outputs and native PR binaries out of uploaded caches/artifacts.
- Release preparation uses `just release-prepare VERSION`. Only when publication
  is requested, use `just release VERSION PR` or `just release-publish VERSION`.
  Preserve exact-SHA candidate selection, protected signing, installer checks,
  immutable assets, and the documented recovery path.
