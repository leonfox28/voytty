# Working on voytty

- Keep `README.md` concise and user-facing: project overview and benefits,
  installation, updates, basic usage, uninstallation, then brief development
  and licensing information. Put detailed user guides under `wiki/` and maintain
  its `README.md` index. Store development and maintenance documentation under
  `docs/`, including architecture, implementation contracts, workflows, and
  verification records. When a document mixes both audiences, separate the user
  instructions from the implementation details and update the relevant links.
- Read `docs/development.md` for development commands and platform boundaries;
  read `docs/releasing.md` before release work. `docs/ci-release-optimization.md`
  records the measurements and rationale.
- Start substantive work from current `origin/main` (directly on `main` or on a
  short-lived local branch for the solo-maintainer workflow). Preserve unrelated
  work. Keep fixes scoped to the requested task.
- During editing, run targeted Clippy/tests for the affected crate. Before an
  ordinary push, run `just check-changes` against current `origin/main`; it includes
  uncommitted and untracked changes. `just check` remains a full local checkpoint.
- Push-to-`main` (and optional PR) CI asynchronously selects affected owners:
  Unix/Windows Rust, patched transport, shared mobile targets, Relay, and native
  packaging. Unknown inputs run all owners. Shared public changes must also be
  integrated and tested with the pinned private app. Report unavailable local
  platform checks accurately; hosted CI owns other OSes.
- Prefer a visible foreground simulator for interactive review (Xcode 27 uses
  Device Hub). Command-line, XCTest and XCUITest debugging may also run headlessly,
  including while the Mac is locked, as authorized by the user. Record runtime
  and visual acceptance separately; do not block command-line checks on window access.
- Keep local and hosted checks in shared `justfile` recipes. Run every selected
  check; do not disable tests, weaken assertions, or add automatic test retries
  to obtain a green run.
  Fix asynchronous tests by waiting for the state they assert, within their
  existing bounded deadline.
- Keep `main` protected against force pushes and deletion (enforced for
  administrators) while allowing fast-forward pushes without mandatory PR waits.
  `just full-check` explicitly runs the entire graph and cold-builds unsigned
  candidates on exact `main` without publishing.
- Only `main` saves compiled dependency caches; PRs may restore them. Explicit
  `full-check` native candidates always start cold. Keep path-package outputs
  and non-candidate native binaries out of uploaded caches/artifacts.
- Release preparation uses `just release-prepare VERSION` (commits and pushes
  fast-forward on `main`, or appends to an existing feature PR). Only when
  publication is requested, use `just release-publish VERSION` (or
  `just release VERSION PR` for an optional PR). Preserve exact-SHA candidate
  selection, protected signing, installer checks, immutable assets, and the
  documented recovery path.
