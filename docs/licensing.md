# License distribution and implementation

The [user licensing guide](../wiki/licensing.md) explains permitted uses and
commercial entitlements. This document covers packaging and implementation.

## Distribution and implementation

The root LICENSE is the unmodified official PolyForm Noncommercial 1.0.0
text. NOTICE identifies voytty's copyright and the scope of the project grant.
The workspace package metadata declares `PolyForm-Noncommercial-1.0.0`.
Commercial grants are conditional on an actual entitlement and are not
represented as an unconditional SPDX `OR` alternative.

The CLI embeds the project notice and both license texts in `voytty --license`,
which runs before configuration, diagnostics, or daemon startup. This keeps
the existing single-binary archive and installer contract. Android and iOS in the private `voytty-app` repository bundle the same texts
and expose them under Licenses. That repository owns `just licensing-sync`
and copy-drift checks; this repository's `just licensing-check` validates the
canonical license and package metadata. Original third-party notices remain
separate from the project notice.

The App Store app uses Apple's Standard EULA; the commercial-use document is
a separate additional grant covering internal work and companion software,
including supported non-Apple hosts. It is not a replacement custom Apple EULA.
Keep the grant available with server downloads and on the eventual purchase
screen, along with the applicable subscription terms and privacy policy.

**Current status:** selecting and bundling these terms does not implement
StoreKit, sell a subscription, verify purchases, activate a trial, or create
commercial rights merely by downloading a development build. No App Store
product has been configured by this change. A purchasing release must connect
the documented entitlement to its actual products, disclose prices and renewal
terms, support restoration, cancellation, refunds, and subscription-to-buyout
transitions, and provide a privacy policy. The prices discussed during planning
are not hard-coded into the license.

Apply the new project license prospectively. It does not revoke rights granted
with earlier releases, overwrite third-party licenses, or establish ownership
of someone else's contributions. For future contributions, follow
[CONTRIBUTING.md](../CONTRIBUTING.md), including the explicit commercial
permission confirmation before merge.

## References

- [Official PolyForm text](https://polyformproject.org/licenses/noncommercial/1.0.0.txt)
  — SHA-256 `ffcca38841adb694b6f380647e15f17c446a4d1656fed51a1e2041d064c94cc8`.
- [Apple Standard EULA](https://www.apple.com/legal/internet-services/itunes/dev/stdeula/).
- [Open Source Definition](https://opensource.org/osd).
