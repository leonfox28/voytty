# Public core and private mobile apps

The official iOS and Android applications live in
[leonfox28/voytty-app](https://github.com/leonfox28/voytty-app), a private GitHub
repository. Access is required. This repository builds the desktop CLI, daemon,
shared protocol/client, diagnostics, terminal engine and optional Relay tools
without credentials for or a checkout of that private repository.

The app repository owns `apps/`, the historically named `crates/android` shared
UniFFI bridge, bindgen, mobile build scripts, UI/unit tests, mobile acceptance
runners and the askama parser backport. It imports this repository as `core/`,
pinned to an exact Git commit. Shared code is not copied. The app workspace must
apply the pinned Iroh/noq patches itself because Cargo does not inherit patches
from a dependency workspace.

Public CI still compiles `voytty-client` for Android, iOS devices and iOS
simulators. It retains all desktop, dependency, Relay and exact-SHA native
candidate gates. App CI owns Kotlin/Swift builds, bridge tests, Android lint/unit
tests and iOS simulator runtime. A shared change needs both layers of evidence:
the public build alone does not prove mobile app compatibility.

Public versions and desktop release candidates remain lockstep. App versions
advance independently and record their core revision in the Git tree. Update
the app pin deliberately and verify pairing, connection and session behavior
against supported host versions. Protocol changes must account for users who
upgrade their phone and hosts at different times.

New desktop releases have nine assets and no APK. Historical immutable mobile
assets, tags and source history remain available under their original terms.
The shared historical Android release verifier and certificate pin remain for
compatibility; the public repository no longer builds or signs an app.

The intended product boundary is free desktop use and a subscription for the
official mobile client's remote access. Billing is not implemented here, and
existing license texts remain unchanged pending separate work. Peer-reported
platform strings are not a subscription security boundary. The split introduces
no subscription dependency into desktop connections or running host sessions.
