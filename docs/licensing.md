# Licensing / 许可说明

## 中文说明

voytty 采用“源码公开，非商业用途免费；商业用途另行授权”的模式，
不是 OSI 定义的开源软件。

- 自有源码适用 [PolyForm Noncommercial 1.0.0](../LICENSE)。非商业使用、
  修改和分发须遵守该许可；不要求修改者公开源码或向本项目提交修改。
- “非商业”不等于“不转卖”：一般公司的内部业务使用也不能自动视为非商业用途。
  许可对特定非商业机构的允许范围以原文为准。
- 有效的官方试用、订阅或买断可以获得[商业使用授权](../COMMERCIAL-LICENSE.md)。
  授权包含官方客户端及配套 CLI/daemon，允许该用户在有权使用和管理的工作电脑、
  服务器上部署服务端，通过 iPhone/iPad 开展开发、运维等内部工作，无需另购服务端授权。
- 单个用户的授权不等于整个公司所有员工均获授权。出售修改版、转售软件、出租或
  将 voytty 作为收费托管服务对外提供，需要另行书面授权。
- 取消续订后，已支付期间的权利继续有效；到期后的商业使用需要其他有效授权。
  买断所含功能的永久使用权不因改用订阅收费而消失。
- 第三方代码、字体和 vendor 包保留原许可。这些规则不限制它们独立授予的权利。

英文 LICENSE 和 COMMERCIAL-LICENSE.md 是对应授权文本，本节是使用场景说明。
公开源码许可与官方商业授权是两条不同的授权路径；下载文件不等于购买授权。

## Distribution and implementation

The root LICENSE is the unmodified official PolyForm Noncommercial 1.0.0
text. NOTICE identifies voytty's copyright and the scope of the project grant.
The workspace package metadata declares `PolyForm-Noncommercial-1.0.0`.
Commercial grants are conditional on an actual entitlement and are not
represented as an unconditional SPDX `OR` alternative.

The CLI embeds the project notice and both license texts in `voytty --license`,
which runs before configuration, diagnostics, or daemon startup. This keeps
the existing single-binary archive and installer contract. Android and iOS
bundle the same texts and expose them under Licenses. `just licensing-sync`
refreshes the mobile copies; `just licensing-check`, also run by `ci-policy`,
rejects drift and checks package metadata. Original third-party notices remain
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
