# 2026-10-13 审查后的顺序修复方案

起点：`origin/main` / `0.1.39` / `ed5df8d860224bde390ba92040ce23ca338ed3ff`。
工作分支：`fix/verification-followups`。按以下顺序实施，每项保留失败对照、修正和验证结果。
本方案同时记录执行状态；历史失败不由新的一次成功覆盖。

| 顺序 | 范围 | 修复与验收标准 | 状态 |
| --- | --- | --- | --- |
| 1 | R11 / P41 遗漏的 CLI 滚动路径 | 恢复时保留旧历史请求的在途边界；旧回应到达前只合并最新滚动目标，之后发出一次新请求。覆盖滚轮、拖动、再次恢复、旧回应丢弃及真实传输错误；终端保持可输入。 | 完成；CLI 109 项、Clippy 及最终核心 146/146 通过，其中接管／并发接管 32/32 |
| 2 | R14 本地 IPC | 先固定 fatal-listener 测试的清理时序，保留会话仍受所有者管理及最终释放断言；再复现 blocked-PTY 超时并按实际根因修复。不得延长期限、弱化断言或自动重试。 | 完成；并发 PTY 160/160、整套 IPC 320/320、监听恢复 160/160 及 Clippy 通过，Linux 完整检查通过 |
| 3 | R12 迟到的 Relay Initial | 在受控 Direct / 延迟 Relay 场景验证已关闭连接的 Initial 副本不会重复占用未认证握手许可；优先在拥有连接 ID 生命周期的协议层处理，边界包含新连接及其他对端的新握手，不改握手期限。 | 完成；跨地址回归、NOQ 441 项及三平台门禁通过，最终扩展／核心所收集日志均无同类慢认证失败 |
| 4 | 文档与证据状态 | 更新当前 main/P36–P43 状态、证据根目录和模拟器授权撤销覆盖；保留每轮旧制品及失败原义。分别列出已修复、待归因和缺验收。 | 完成；最终结果、旧样本和验收边界分别记录；审计工具支持显式约定时长 |
| 5 | 统一验证 | 定向 Rust 测试及 Clippy；最终 `just check`。共享 Rust/传输补丁改变时执行 `just android-check`。Linux 证据单列，随后在同源制品上执行三端定向、扩展 88 项、核心一轮 146 项和十分钟稳定性及审计。 | 完成；三平台门禁、扩展 88/88、核心 146/146、604.4 秒／45 动作通过，审计零问题 |
| 6 | R01/R02/R04/R05/R06/R07/R09 收尾 | 对新矩阵中的失败保存同次链路/操作/关闭阶段证据，再修复能够确证的产品原因；旧证据已丢失的样本保留未归因，不以新的通过率宣称历史根因解决。 | 本轮收尾完成；未出现新的矩阵／稳定性失败。历史样本仍待归因，R09 历史十轮／一小时及外部设备验收未完成 |

## 实施约束

- 用户在本轮明确将稳定性时长改为十分钟；执行核心一轮及完整扩展矩阵。
  本轮记录不等同于历史文档中的核心十轮／一小时验收。
- 每项先建立能够验证行为的失败对照，再修改实现；测试器缺陷与产品缺陷分开记录。
- 保留五秒/十五秒请求预算、恢复期限、操作去重与未知结果语义。若证据表明必须改变产品契约，先说明具体取舍。
- 不改真实 `~/.zterm`、用户会话、网络代理或系统防火墙。三端实验仅使用运行器拥有的 A/B/C 与既有故障清理机制。
- 验证证据放在 `${XDG_STATE_HOME:-~/.local/state}/zterm-verification/`，不放 `target/`。
- 真手机节电/键盘、真实跨物理网络、登录注销及正式签名包验收需要对应设备/环境，不能用模拟器或本机单元测试代替。
- 本次不发布。提交、推送或 PR 如需进行，遵守仓库的完整检查与平台边界。

## 执行记录

2026-10-13：已重新 fetch origin/main，确认起点干净；审查复现与报告保存在
`~/.local/state/zterm-verification/reviews/2026-10-13-ed5df8d/`。开始顺序 1。

顺序 1：CLI 保留旧历史请求的在途边界，等待旧回应后仅补发最新滚动目标。失败对照
`cli-before-02.log`，最终定向结果 `cli-after-02.log`（109 通过、3 忽略）及
`cli-clippy-02.log`。原始首次编译日志保留，失败原因是旧临时工作树的 proto 构建缓存。

顺序 2：fatal-listener 测试移入库测试，通过持有真实子进程所有权互斥锁控制清理窗口；
保留原有会话、活进程及最终套接字移除断言。PTY 路径在 macOS 27.0.1 上复现：8 并发、
160 次中 152 次关闭超时；仅调整 READY 输出顺序仍有 157 次超时。线程采样显示输入线程
停在原生 `write`、关闭线程停在 `wait4`，子进程处于退出中。平台现使用非阻塞 I/O、
关闭唤醒和有界原生写入，保留有序字节传递及尾部输出排空。最终 160 次全部通过，
耗时 9.16 秒；另增真实背压后的取消与有序续写测试。daemon 216 / platform 34 / IPC 6
项通过（两个库各 1 项忽略），PTY 生命周期 gate 及 Clippy 通过。

本轮原始记录统一位于 `~/.local/state/zterm-verification/checks/sequential-fixes-20261013/`。
其中 `r14-interruptible-after/` 误选旧测试二进制，已标为 INVALID，不作为修复后证据；
其他中间候选的失败结果保留，最终结果为 `r14-chunked-full-after/summary.json`、
`r14-targeted-final.log`、`r14-clippy-final.log`。这不证明其他 macOS 版本或 Linux 的内核行为。

顺序 3 / P46 初版：真实 NOQ 报文在握手成功、两端完全排空后延迟 275 ms 重放，旧实现重新
分配 Incoming，见 `r12-before-02.log`。现按原始地址对、版本和双向 Initial CID 保存
有界退休记录（1024 项 / 60 秒，使用可注入的服务端时间源）；重复报文不续期。
首轮完整 NOQ 439 项通过，随后补充身份匹配边界测试，纳入本机统一门禁。
该保护在到期或容量淘汰后恢复常规准入，不是永久抗重放或全部 R02 超时的解释。

顺序 4：已纠正文档顶部的 main/P36–P43 状态、证据根目录，以及 Android 模拟器授权
撤销覆盖；保留真机验收和历史待归因项。顺序 5 开始执行 `just check`。

统一验证：`just-check-01.log` 的完整本机门禁通过；NOQ 共 440 项通过，补丁 crate 的
独立 Clippy 也通过（`noq-clippy-01.log`）。`android-check-01.log` 通过，隔离 A/B/C 制品
重建及网络预检通过。宿主机门禁未执行的异平台、签名和发布项保持原边界。

最终本机追加压力验证：8 并发整套 IPC 320 次全部通过（`r14-final-ipc-all/summary.json`），
另 8 并发监听恢复 160 次全部通过（`r14-final-fatal/summary.json`）；两组同时运行，并有
Linux VM 编译负载。它们是保留全部结果的预设压力实验，没有对失败自动重试。

Linux 首轮 `just ci-unix false` 在 CLI 的身份重置测试失败，见 `linux-ci-unix-01.log`。
账户 `umask 0002` 使共享夹具新建的 home 为 `0775`，触发已有 autostart 目录安全检查；
现让夹具显式创建 `0700`，不改变产品权限检查。相同 Linux 环境下原失败用例通过
（`linux-fixture-umask-after.log`），随后重新执行完整 Linux 和本机门禁。
本机 `fixture-umask-probe.log` 编译期间夹具已修改，属于修复后探针，不作为失败对照。

夹具修正后，本机完整 `just check` 再次通过（`just-check-02.log`）。Android 门禁之后
仅修改测试夹具和执行记录，Android 产品代码及依赖未再改变。

Linux 第二轮发现身份重置的库测试也自行创建同样的组可写 home，见
`linux-ci-unix-02.log`；现复用共享私有目录夹具。该用例在 Linux 和本机 `umask 0002`
下均通过（`linux-reset-fixture-after.log`、`reset-fixture-after.log`），Clippy 通过。
两处均为夹具权限修正，未改变重置的锁等待、总截止时间或生产路径校验。

Linux 第三轮已通过 daemon 库及 IPC，随后自启动正向单测出现相同目录权限失败
（`linux-ci-unix-03.log`）。`tempfile` 的默认目录也受 `umask` 影响，现显式设置自启动
测试家庭目录和共享状态夹具外层目录为 `0700`；保留拒绝符号链接和可写目录的检查。
macOS 与 Linux 两项自启动定向均通过（`autostart-fixture-after.log`、
`linux-autostart-fixture-after.log`）。每次完整检查失败均原样保留，修正对应夹具后重新验证。

上述夹具修正后的本机 `just check` 通过（`just-check-04.log`）。Linux 第四轮工作区
843 通过、9 忽略，daemon 216 / platform 34 项通过；NOQ 440、Iroh 33 项补丁回归通过，
`just ci-unix false` 完整通过（`linux-ci-unix-04.log`）。

三端初版归档 `a98cc087c431fa3e922a2b1aac3e65674c8a91996d6dfcbf5559dccc73ed0b37`，
`sequential-fixes-takeover-01` 32/32，审计零问题。随后扩展矩阵的功能行通过，但日志
仍出现 R12 的十秒入站认证超时；不能把功能通过当成过滤器生效。对照分析脚本在旧
`extended-main-01` 读出 7 次，本轮部分结果已有 4 次；保留完整第一轮后继续修正。

P46 补正：Iroh 将 Direct 原始 IP 和 Relay 虚拟 IPv6 地址分别交给 NOQ。同一 Initial
跨路径后地址对会变，初版包含地址的键无法命中。新增真实报文跨地址回归在初版失败
（`r12-cross-path-before.log`）；现与活动连接的 Initial CID 索引保持一致，按版本及
双向初始 CID 匹配，地址不参与。其他对端的新握手由新的 CID 正常准入；从另一地址
重放同一报文属于同一握手的副本，不能用它代表新的对端握手。相关 5 项通过
（`r12-cross-path-after.log`），继续完整门禁并重建三端制品。

初版扩展 `sequential-fixes-extended-01` 最终 88/88，审计零问题，但同次日志有 8 次
九秒以上的 `authenticating_transport` 失败（均为十秒 `deadline_exceeded`），见
`extended-handshakes-01.json`；旧轮相同口径 7 次，见 `old-extended-handshakes.json`。
这轮不能作为 P46 生效证据。补正后的 Android 检查通过（`android-check-02.log`），
NOQ 独立 Clippy 通过。`just-check-05.log` 中途停止且没有完成结果，保留为未完成，
未记为通过；重新执行完整本机门禁，并在 Linux 重新执行共享完整配方。

跨路径补正后的完整本机 `just check` 通过（`just-check-06.log` / `.exit=0`），NOQ
441 项、Iroh 33 项通过。Linux 第五轮在 `cross_uid` 复制测试程序时遇到磁盘满
（`linux-ci-unix-05.log`，不是产品断言失败）；仅清理隔离 A VM 的
`~/zterm-build/target/debug/incremental`，释放约 18 GB，源码、二进制和证据保留，
见 `linux-incremental-cleanup.json`。释放空间后原样重新执行完整 Linux 配方。

最终跨路径补正的 Linux 完整配方通过（`linux-ci-unix-06.log` / `.exit=0`），含 NOQ
441 项及 Iroh 33 项。macOS、Linux、Android 检查均完成，开始重建最终三端制品。

最终三端制品构建及六向预检通过（`e2e-build-03.log` / `.exit=0`）。源码仍是基线提交加本轮
未提交修改，以归档摘要标识，不能只用 `ed5df8d` 指代受测内容：

| 制品 | SHA-256 |
| --- | --- |
| 源码归档 | `5a898c86ade951e1cb9cb28a03785c36a6eecebd89f21f441e2c8ba65798fe6e` |
| A / B Linux 二进制 | `a4ce541ee1a3d9987cf777a6b79d6223116bf595ca8c1185d26ac9471a4f7be2` |
| Android APK | `c5c82fe84ae4689891e4ac8b2c3ff99bfe450b184f90d9ed741e30e562e44157` |

`sequential-fixes-extended-02` 自 08:50:16 UTC 开始验证该归档。
最终本机工作区 834 通过／16 忽略，Linux 工作区 843 通过／9 忽略；
两个平台 NOQ 均为 441 项、Iroh 补丁回归均为 33 项。Android 构建、JVM 测试及 lint 通过。

最终扩展矩阵 `sequential-fixes-extended-02` 为 **88/88**，A／B × Direct／Relay 各
22/22，标准审计零问题。对同轮 Linux 日志快照去重并排除开跑前事件后，2679 条事件中
九秒以上入站认证失败为 **0**（`extended-handshakes-02.json`）；第一版相同矩阵有 8 次。
快照尾部的 62 条不完整 JSON 行被明确跳过；这是已收集日志范围内的统计，不声称完整网络捕获。
跨路径库回归提供因果对照，三端日志提供真实环境下的验证；两种证据分别保留。
随后在相同制品上启动 `sequential-fixes-core-01`：核心一轮 146 项、种子 20261003，
完成后执行 600 秒稳定性。

## 最终验收与收尾

`sequential-fixes-core-01` 核心一轮 **146/146**，没有失败、阻塞、跳过或运行事件。
包含接管 24/24、并发接管 8/8、冷启动 8/8、首次打开 8/8、配对 8/8；
上轮 `0059-race-takeover-A-B-C-relay-1` 的失败组合本轮通过。
核心记录的连接步骤最慢 3.010 秒，恢复最慢 15.944 秒、断网检测最慢 54.464 秒，均在原期限内。

同源十分钟稳定性实际 **604.353 秒**，准备另计 2.844 秒，**45/45 动作通过**，
连接失败 0。A／B 两个原会话都保留，连续性检查分别核对 56／62 条唯一执行记录，
进程与目录未变，后台计数分别由 1 增至 2370／1179。A 的会话还跨越整个 B 阶段。
最终核心日志 5859 条去重事件中九秒以上入站认证失败为 0（尾部不完整 JSON 113 条跳过），
见 `core-handshakes-01.json`。四次出站十五秒拨号超时来自注入断网／交接的行，均在故障
解除前开始尝试，相关行按原期限恢复；逐次记录在 `core-outage-dial-timeouts-01.json`，
不据此归因旧 R02 样本，也不宣称日志中完全没有连接失败。

最终审计发现审计器把一小时写死，最初报告 1 个时长问题，见
`core-audit-before-duration-option.json` 和 `e2e-core-audit-01.log`。增加显式
`--soak-seconds` 后默认仍是 3600；本轮指定 600 并必须与原运行清单相符，不能回改清单
缩短要求。两端覆盖、原会话、唯一执行、计数进展、失败动作及连接失败断言保持；
不足时长、非有限时长、清单不符或连续性损坏均拒绝。失败回归
`audit-duration-before.log` 保留，新增 4 项含各边界子例在 macOS／Linux 均通过，原运行器
48 项通过；新增审计回归纳入共享 `just ci-policy`。修正后的审计命令：

```sh
python3 tests/e2e/multi-device-audit.py sequential-fixes-extended-02
python3 tests/e2e/multi-device-audit.py --soak-seconds 600 sequential-fixes-core-01
```

两轮最终审计均为零问题。核心报告分别记录 `required_seconds=600`、
`completed_required_duration=true`、`completed_60_minutes=false`；原始矩阵、稳定性数据和
受测制品未修改，不需要重复设备实验。最终本机完整 `just check` 再次通过
（`just-check-07.log` / `.exit=0`）；Linux 产品完整门禁为 `linux-ci-unix-06.log`，
Android 为 `android-check-02.log`。此后只有审计工具、其测试、共享检查配方及文档变动，
产品与运行器仍对应最终归档。Linux 审计工具追加回归见 `linux-audit-duration-after.log`。
最终核对归档中 579 个文件：构建后仅文档、审计工具及其共享检查配方变化，另增审计回归，
产品和运行器一致；差异清单及最终审计器摘要见 `final-source-comparison.json`。

最终日志收集、安装摘要和六向网络预检通过。独立核对 A／B／C 的 IPv4／IPv6 实验规则
均为空，故障和临时身份恢复记录均为空，见 `e2e-final-rule-check.json`。
A／B 虚拟机和 C 模拟器于 **2026-10-13 10:12:17 UTC／18:12:17 上海时间**停止，
数据与全部失败证据保留（`e2e-final-stopped.json`）。未操作真实用户身份或无关模拟器。

本轮实现和约定验收已完成，按用户要求由 `fix/verification-followups` 提交 PR 评审，未发布。
仍开放的是 R01/R02/R04/R05/R06/R07 的历史待归因样本、历史 R09 核心十轮／一小时标准，
以及真手机、真实跨物理网络、登录注销和签名安装升级等外部环境验收；不能用本轮通过替代。
