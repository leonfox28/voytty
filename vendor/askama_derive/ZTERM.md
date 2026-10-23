# Askama TOML parser backport

This is the published `askama_derive` 0.16.1 crate, including its MIT and
Apache-2.0 licenses and upstream tests. The registry installation marker is
omitted. Published crate SHA-256:
`071ee5ebf2138e3ad180e0aacf6940c2cab5e6d8333741d9925c7bee2b153f39`.
Original upstream revision: `9ae4d5bdf4352b35e34106e38944c90d7a7ede3e`.

The only code change backports upstream commit
[`3863808cf594054f7644979ed92caaafc878f378`](https://github.com/askama-rs/askama/commit/3863808cf594054f7644979ed92caaafc878f378):
replace the archived `basic-toml` dependency with `toml`, enabling only `parse`
and `serde`. Both the original and normalized manifests reflect that change;
the standalone test lockfile is refreshed for the replacement. UniFFI 0.32
requires Askama's config feature, which exercises borrowed string parsing.
This removes RUSTSEC-2026-0374 without adding an advisory exception.

`just ci-askama` runs the upstream library tests, including config parsing,
custom syntax and escapers. The product's Kotlin and Swift binding generation
also exercises the patched parser with UniFFI's real templates. The root patch
is a path dependency, so the existing CI cache trimmer excludes its outputs.

Remove this directory, the root patch/exclusion and `ci-askama` when a released
Askama version compatible with pinned UniFFI contains the upstream change.
