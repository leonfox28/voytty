# Public core and private mobile apps

The official iOS and Android applications live in
[leonfox28/voytty-app](https://github.com/leonfox28/voytty-app), a private GitHub
repository. Access is required. This repository builds the desktop CLI, daemon,
shared protocol/client, diagnostics, terminal engine and optional Relay tools
without credentials for or a checkout of that private repository.

The app repository owns `apps/`, the `crates/mobile` (`voytty-mobile`) shared
UniFFI bridge, bindgen, mobile build scripts, UI/unit tests, mobile acceptance
runners and the askama parser backport. It imports this repository as `core/`,
pinned to an exact Git commit. Shared code is not copied. The app workspace must
apply the pinned Iroh/noq patches itself because Cargo does not inherit patches
from a dependency workspace.

For shared-input changes and explicit full validation, public CI compiles `voytty-client` for Android, iOS devices and iOS
simulators. Full validation retains all desktop, dependency, Relay and exact-SHA native
candidate checks; ordinary PRs select the affected owners. App CI owns Kotlin/Swift builds, bridge tests, Android lint/unit
tests and iOS simulator runtime. A shared change needs both layers of evidence:
the public build alone does not prove mobile app compatibility.

Public versions and desktop release candidates remain lockstep. App versions
advance independently and record their core revision in the Git tree. Update
the app pin deliberately and verify pairing, connection and session behavior
against supported host versions. Protocol changes must account for users who
upgrade their phone and hosts at different times.

Desktop releases publish twelve assets across five native targets. The signed inventory is described in [release operations](releasing.md). Historical immutable mobile
assets, tags and source history remain available under their original terms.
The shared historical Android release verifier and certificate pin remain for
compatibility; the public repository no longer builds or signs an app.

The desktop and shared source use the terms in the [project license](../LICENSE).
The private app owns subscription enforcement for mobile remote access.
Desktop connections and running host sessions operate independently of an app subscription.
