# 多设备验收历史过程

归档时间：2026-10-04 05:41 UTC。以下正文保留此前逐轮记录；其中“当前”指归档时的阶段。
最新结论与后续验证见 [验收报告](multi-device.md)。原始失败、阻塞与未完成记录没有改写。

## 2026-10-08 续修记录：P30、T30

继续定位 R02，确认 Relay 拨号过程每三秒清空发送队列会误删新入队的握手重传。
P30 为每项保存首次入队单调时间，仅淘汰达到原三秒寿命的包；路由／退避不重置年龄，
恢复发送时再次检查。使用原有有界通道加一个前瞻项，保留 FIFO／批量发送，生产 QUIC
计时器、Relay 连接期限及产品五秒期限不变。三项无 socket 回归覆盖新包存活、混合年龄
FIFO 与退避过期；真实 Relay／QUIC／TLS 回归将连接延迟 3.3 秒，测试夹具使用 300 ms
初始 RTT，以确定性安排清理前的重传。原实现首次请求 5.003 秒超时，修复后 3.414 秒成功，
最终四项通过。首次 DNS 挂起夹具自身先超时，没有执行目标清理，单独保留为无效复现。

同源 `9593ab6bd1f16fb4` 安装三端，447 项构建输入匹配。Mac 完整工作区 786 通过／
13 忽略、Linux 792 通过／9 忽略，Linux Iroh 完整库 142 通过／1 忽略，Mac Iroh
无 socket 定向 14 项通过；Clippy、Android 构建／lint／JVM 与静态检查通过。
第一次设备协调器因证据目录命名重复在设备操作前退出，第二次 A→B UDP 预检失败，均保留。
第三次全六向预检通过，`cold-start-01` 完整记录 32 项：27 通过／5 初连失败、无阻塞／跳过，
全部 27 项通过证据及覆盖审计无不一致。A/Direct 7/8、A/Relay 6/8、B/Direct 8/8、
B/Relay 6/8；15 项实际 detach 通过，另 1 项初连失败未执行 detach。
三项 Linux 初连失败、两项 Android Hello/Welcome 失败仍未闭环；不将 27/32 与上轮
28/32 的单次样本差异解释为改善或退化。

追查独立 Android 失败时发现 T30：Lab.start 只启动 VM／模拟器，预检接受产品 daemon
仍 Stopped，独立 instrumentation 前遗漏主机准备。P29 第二轮及本轮首批均有 Stopped
证据；日志收集随后执行 local 查询才启动主机。矩阵自身有主机就绪步骤，其初连失败仍有效。
P29 首轮位于安装／setup 之后，没有同等离线证据，不能一并重分类；原始失败全部保留。
新增 `tests/e2e/multi-device-hosts.py`，在所属测试环境锁内用本地查询启动已配置 daemon，
记录前后状态并等待 Online；不重启活跃服务、不建会话、不请求 peer 或预热控制端。
新增三项回归及完整运行器 48 项通过，README 已补齐独立 instrumentation 的前提命令。

第四次协调器只修正 Android 前提并复测。A／B 从 Stopped 转为 Running／Online 后，
重新启动 C 进程，基础检查仍为 63 通过／1 失败／35 条件跳过，权限 4 项、指定主机 UI
2 项通过。失败在 NativeTerminalTest 创建会话前首次列表：准备 65 ms、解析 1 ms，
QUIC 握手 4,937 ms 后触及原五秒期限，尚未进入 Hello；因此 R02 确有剩余产品失败。
主机附近的接收／认证事件未有跨端连接 ID 关联，设备时钟未校准，不能据时间差唯一归因。

日志收集、最终六向预检及停止均成功；447 项输入再次匹配归档。测试实例于
2026-10-08 07:54:51 UTC 停止，无残留故障规则、运行或排队任务。没有修改防火墙、提交
或发布。当前完整核心 1460 项、完整扩展 88 项及 3600 秒稳定性未执行；全部证据位于
`checks/ordered-fixes/cold-start-fix/`，最终汇总为 `final-summary-01.json`，T30 解释更正为
`host-readiness-analysis-02.json`，正确前提下的剩余失败为 `android-host-ready-failure-analysis-02.json`。

## 2026-10-08 续修记录：P29

继续定位 R05。纯内存链路上使用真实 QUIC／TLS，首先复现三个确定缺陷：取消关闭后重入
提前返回、并发关闭提前返回，以及 Drop 因已进入 closing 而遗漏任务取消。异步互斥锁保留
actor 句柄并串行化关闭；取消后可继续等待，最终所有者会中止剩余任务。六项关闭永久回归
及五项已有重启回归全部通过；没有放宽五秒期限或跳过协议关闭等待。

受控约两秒 RTT 的链路，回应可达时约两秒正常关闭，且对端收到原应用关闭码。屏蔽回应时，
五秒截止仍处于 `wait_all_draining`，actor 尚未退出；基线协议计时器于 11.565 秒触发。
该实验复现一种 R05 等待机制，但原 `0029` 现场没有库内／报文证据，仍不能做唯一归因。

同源 `61806d256b4ec6ed` 已安装三端，446 项构建输入匹配。Mac 完整工作区 786 通过／
13 忽略、Linux 792 通过／9 忽略，Linux Iroh 完整库 138 通过／1 忽略，静态检查通过。
Android 基础设备两轮均 63 通过／1 请求超时／35 条件跳过。只增加阶段日志的一次复测
确认失败发生在 NativeTerminalTest 创建会话前的首次列表请求。该复测后续发现主机未启动，
其产品失败归类需结合上方 T30 修正，原始结果不改写。权限每轮 4 项通过；
指定主机 UI 第一轮 2 项通过、第二轮 2 项请求超时，不能由后续样本覆盖原失败。
`endpoint-close-01` 两轮冷启动／正常 detach 共 32 项，28 通过／4 初连失败，无阻塞或跳过，
28 项通过证据与覆盖审计无不一致；A/Direct 7/8、A/Relay 6/8、B/Direct 8/8、B/Relay 7/8。
15 项实际 detach 均通过，另 1 项在初连失败而未执行。按设备／进程／操作关联的 22 次
Endpoint 关闭均有 Closed，最长 1,768 ms；不能把这些样本当作任何网络条件下的保证。
日志收集、最终六向预检和停止均成功，446 项构建输入再次匹配归档，A／B／C 于
2026-10-08 06:51:28 UTC 停止。没有残留故障规则、运行或排队任务，没有提交或发布。
当前完整核心 1460 项、完整扩展 88 项及 3600 秒稳定性未执行，之前 `740376ef` 的
72/88 完整扩展结果独立保留。证据统一位于 `checks/ordered-fixes/endpoint-close-fix/`，
汇总为 `final-summary-01.json`；原 Android 失败和第一次设备协调器提前停止的记录保留。

## 2026-10-08 续修记录：P27、P28

两项隔离候选已正式实现并安装到既有 A／B／C。P27 固定 Iroh 1.3.0，提供默认关闭的
`persistent_stateless_reset` 配置，生产 daemon／移动端显式启用。稳定重置密钥从秘密身份
以独立用途派生，CID 使用 128 位随机值；同身份只能有一个存活 Endpoint，替换必须等待
旧实例完全关闭。身份轮换、原协议期限、TLS ticket／地址验证及变更请求身份规则保持原义。
旧随机密钥签发的连接不在单端升级后的自动修复范围。

补齐的“仅随机 CID”对照首次查询仍在 5.002 秒超时；三个生产配置的永久回归在修复前均
超时，修复后 14.6–16.1 ms 成功，使用真实 TLS／Hello／Welcome／列表请求。五项 Iroh
永久回归覆盖密钥隔离、CID、默认配置，以及路径零退役后的非零路径重置。P28 将半关闭
隧道修正合入生产，继续接收合法迟到响应，并由 P26 清理结束后的无响应闲置连接。
daemon 单元测试 204 通过／1 忽略，最终半关闭定向五项通过。

新源码归档为 `source-740376ef812fb3e2.tar.gz`，445 项构建输入与归档逐项一致。
已安装 Linux 二进制 `34a6b0e9`、Android APK `12595629`，instrumentation APK 仍为
`47efe28d`；完整 SHA-256 位于 `checks/ordered-fixes/endpoint-restart-fix/installed.json`。
六向预检通过；Mac 完整工作区 786 通过／13 忽略，Linux 完整工作区 792 通过／9 忽略，
均按外层测试二进制统计。两端 Clippy、格式／源码／依赖／秘密扫描、45 项运行器回归通过。
Linux Iroh 完整库 132 通过／1 忽略；Android 静态检查、基础设备 64 通过／35 条件跳过、
通知权限 4 项和指定主机 UI 2 项均通过。三端 `restart-extended-01` 完整 88 项为
72 通过／16 失败、无阻塞／跳过；A/Direct 21/22、A/Relay 11/22、B/Direct 22/22、
B/Relay 18/22。72 项通过证据及覆盖审计无不一致，12 项在初连失败，4 项在场景检查失败。
其中 `0029` 主机已提交接管但客户端结果不完整；`0033` 精确重开已结束 Session 超时；
`0043`／`0073` 的第二控制端在接管前连接失败，不能记为接管操作已执行。

`0029` 清理时业务流已释放，随后 ClosingEndpoint 到约 5 秒后 ProcessFailed，
具体库内等待点没有记录。该证据采于下一行 `0030` 的日志，关联分析单独保存，
不能归为 P28 的半关闭泄漏。`0082` 查询超时期间接收计数增长 168,432 字节、Relay RTT
增至 4,215 ms；这既不是完全静默的证据，也不足以断言服务端限速或识别数据内容。

详细日志窗口在 03:00、03:12、03:26 UTC 完整开启；03:24 一次续开恰逢 B VM 重启，
SSH 断开且仅完成 A，原失败保留，03:26 在旧窗口到期前完成三端续开。
日志收集、结束前六向预检和停止均成功，445 项输入仍与归档一致，A／B／C 于
2026-10-08 03:27:10 UTC 停止，无故障规则残留和运行／排队任务。汇总为
`checks/ordered-fixes/endpoint-restart-fix/final-summary-01.json`；当前核心 1460 项与
3600 秒稳定性未执行，完整验收仍未通过。

Mac 全 Iroh 运行时用户拒绝测试程序传入连接，只读防火墙清单确认其被阻止。该轮出现网络
超时后已停止；没有改动防火墙，Mac 内存传输回归通过，完整库网络检查转到 Linux VM。
Linux 第一次 workspace 重建链接时磁盘耗尽，未运行测试；保留失败后仅清理明确的生成
debug 缓存，确认安装二进制摘要不变，以相同源码关闭测试调试符号／增量缓存重跑通过。
这些环境失败、前后对照、平台检查和安装记录统一保存在
`checks/ordered-fixes/endpoint-restart-fix/`；没有提交或发布，完整验收仍未通过。

## 2026-10-06 进展记录：扩展矩阵与隔离候选

本轮产品构建输入未改变，仍为 `65dd801d`。首次启动因 B→A UDP 预检超时结束，
原证据位于 `checks/ordered-fixes/ordered-extended-validation-01.json`。
新编号 `ordered-extended-02` 通过预检后完成 88 项：72 通过、16 失败，无阻塞／跳过。
15 项失败在初连，1 项是 Linux A VM 重启后的重连；全部通过证据及覆盖审计无不一致。
日志收集、最终预检和停止均完成，实例于 2026-10-05 15:25:38 UTC 停止，无故障规则残留。

半关闭隧道探针在真实 Unix socket 与 TLS 链路确认：前端写半关闭后完全退出，远端静默且
路径不变时，原适配器继续持有流。隔离修正利用已有路径观察周期检测本地写入失败，
保留正常半关闭后的迟到响应与原 2 秒关闭期限。前失败日志、格式化补丁、204 通过／1 忽略
的 daemon 单元检查和 Clippy 结果都保存在 `checks/ordered-fixes/half-close-probe/`。

Endpoint 重启探针确认，同 Endpoint 仅丢失连接状态时首次查询约 11.199 ms 恢复；
相同身份完整重建 Endpoint 时，原配置首次查询 5.003 秒超时、下一次独立查询 6.285 ms 成功。
只固定重置密钥仍失败；同时采用稳定重置密钥和 16 字节随机 CID 后首次查询约 1.943 秒成功。
探针及三次配置对照日志位于 `checks/ordered-fixes/reset-query-probe/` 及同级
`reset-query-endpoint-restart-01.log`、`reset-query-stable-key-01.log`、`reset-query-stable-key-cid-01.log`。
两条自定义路径的早期夹具未建立第二路径、未进入故障注入，其失败单独保留。

两项修改均只存在于隔离副本，尚未并入主工作区、正式固定 Iroh 依赖或安装到 A/B/C。
旧缓存重启恢复的正式 API、身份／CID 边界回归，以及新握手失败定位仍待完成。
当前制品的 1460 项核心与 3600 秒稳定性未执行；没有提交或发布。
完整进展快照为 `checks/ordered-fixes/ordered-progress-20261006.json`，续修入口为
`checks/current-handoff.json` 和 [问题清单](multi-device-issues.md)。

## 2026-10-05 续修记录：P25、P26

P25 确认并修正 NOQ 多路径重置令牌的登记、路径路由和退役校验。七项同源回归在旧源码上
全部失败、修正后通过，独立完整库 436 项通过。`43826c75` 三端制品的 80 项定向运行为
75 通过、5 失败，审计无不一致；一次 Android 新握手超时，三次 Linux 复用同一未响应
候选，另一次旧连接结束。原失败与阶段日志保留于 `reset-host-outage-01`。

由重复复用失败定位 P26：unary 清理没有覆盖前端超时退出的透明终端隧道。
本地真实 TLS 使用生产前端原 5 秒期限，三项修复前失败、修复后通过；保留并发双向流、
明确主机错误、发送前取消和迟到旧候选的保护检查。最终该组 9 项通过，不重发未知创建。

新同源 `65dd801d` 的 Mac 完整工作区 780 通过／13 忽略，Linux 786 通过／9 忽略，
Clippy、格式及策略检查通过。Android lint／JVM 通过，基础设备检查 64 通过／35 条件跳过，
通知权限 4 项、指定主机 UI 2 项另行通过。NOQ 和运行器源码未变，沿用 P25 的 436／45 项结果，
没有将重复计数当作新增覆盖。

`tunnel-host-outage-01` 完成 A 主机／Direct 的八个方向各十轮：20 项断网、60 项接管
全部通过，80 项覆盖及通过证据审计无不一致。结束前预检及 363 个构建输入核对通过。
A / B / C 于 14:00:48 UTC 停止，无故障规则残留；原失败没有改写。汇总记录为
`checks/ordered-fixes/tunnel-final-summary.json`。完整核心 1460 项、扩展 88 项和一小时
稳定性仍待当前制品验收，R01／R02 的全部历史原因尚未解释。

---

# A / B / C 多设备连接与恢复验收

基线：`main`，`0.1.36`，`24654dc5da563877f2059f23e9c71ab26e49def7`。
日期：2026-10-03 至 2026-10-04（上海时间）。开发机：macOS ARM64。运行说明见
[端到端测试说明](../../tests/e2e/README.md)。本记录随本轮验证更新；只有已完成断言的
行才计为通过，冒烟测试不替代每个核心场景十次重复和完整一小时稳定性测试。

**验收仍在进行，尚未通过。** 已完成的一小时运行存在真实失败。复核发现旧测试器的
`offline` 只阻断出站流量，旧控制端仍可能收到接管通知；这些运行仅作为单向丢包证据。
已补齐双向隔离并完成本制品的完整十轮矩阵，一小时测试继续执行；矩阵中仍有失败和
阻塞，不能用旧结果代替完全断网后的恢复验收。

当前三端制品来自 `environment/source-83a65a8fec19bc67.tar.gz`，已包含首个有效路由
立即用于连接的修正。Linux SHA-256 为
`ac345b3f0ac1588355b4e7b0ecc9a487d44cf55e6e8f9e5f172b1c5dbd66203e`，debug APK 为
`694086da515050a9600f49b394629480a637bd6ceec0dafcf8b6e3fa7c516696`，instrumentation 为
`de36c7eeb537826c6cb0d59ea0c9a026d7e4ba4895d5f8fce01ff0cc63c5115a`。
三端安装／校验／预检已通过，记录为 `checks/first-route-installed.json` 和
`checks/final-validation-v18.json`。该源码的 macOS workspace **746 通过、13 ignored**，
Clippy / fmt、Android 构建／lint／单元测试通过。Linux workspace **752 通过、9 ignored、
0 失败**，Clippy / fmt 通过；Android 基础运行 **41 通过、4 条件跳过**，通知权限
允许／拒绝下的前台服务回归共 **4/4 通过**。协调器／审计 **28 项通过**，源码规则
通过，证据见 `checks/linux-tests-16-summary.json`、`checks/first-route-android-runtime.json`、
`checks/coordinator-49.log` 和 `checks/source-policy-25.log`。

| 本制品最终运行 | 计划数量 | 当前状态 |
| --- | ---: | --- |
| `first-route-cold-smoke-01` | 48 | 43 通过、5 失败；无阻塞／跳过，证据审计无不一致 |
| `extended-final-08` | 88 | 70 通过、18 失败；无阻塞／跳过，证据审计无不一致 |
| `core-final-09` | 1460（146 × 10） | 1320 通过、139 失败、1 阻塞、0 跳过；覆盖齐全，证据审计无不一致 |
| `soak-final-10` | 3600 秒 | 已启动，固定种子 `20261003`；待完成 |

完整十轮矩阵于 **2026-10-04 05:35:17 UTC** 结束，所有场景均执行十次，未中途重启
或用后续通过覆盖失败。按主机和计划路径的结果如下；失败项不代表曾成功选中该路径。

| 主机／路径 | 通过 | 失败 | 阻塞 | 跳过 |
| --- | ---: | ---: | ---: | ---: |
| A／本地 | 10 | 0 | 0 | 0 |
| A／Direct | 364 | 16 | 0 | 0 |
| A／Relay | 305 | 34 | 1 | 0 |
| B／本地 | 10 | 0 | 0 | 0 |
| B／Direct | 363 | 17 | 0 | 0 |
| B／Relay | 268 | 72 | 0 | 0 |

最终审计核对 **1320 项**通过证据；通过样本的最长断线检测 **96.688 秒**、恢复可输入
**51.676 秒**、旧端退出 **49.459 秒**、主机释放 **86.832 秒**、接管 **13.441 秒**。
恢复最大值来自 `1434-recover-long-B-C-C-relay-10`，失败与阻塞未混入这些耗时。
`acceptance-audit.json` 和 `matrix-final-1460.json` 确认完整覆盖且没有重复、计划外行。
公开命令日志共 **60,423 条、60,423 个独立文件**，没有覆盖冲突或不可解析记录，见
`checks/core-final-09-command-log-audit.json`。证据一致不等于产品验收通过。

核心矩阵第一轮已完整执行 146 行：**134 通过、12 失败**，没有阻塞或跳过。
A / Direct 为 37/38、A / Relay 为 27/34、B / Direct 为 37/38、B / Relay 为
31/34，两台主机的本地连接均通过。通过样本的断网检测最长 **53.899 秒**、恢复
可输入最长 **14.641 秒**、接管最长 **9.702 秒**；失败样本未混入这些耗时。
第二轮为 **139 通过、7 失败**：A / Direct 36/38、A / Relay 33/34、B / Direct
37/38、B / Relay 31/34，两项本地连接均通过。后续轮次继续执行；这两轮不替代
十轮结论，详见 `core-final-09/iteration-01-summary.json` 和 `iteration-02-summary.json`。
第三轮 **126 通过、20 失败**：A / Direct 38/38、A / Relay 30/34、B / Direct
37/38、B / Relay 19/34，两项本地连接均通过。前三轮共 **399 通过、39 失败、
0 阻塞、0 跳过**，第三轮详见 `iteration-03-summary.json`。
第四轮 **122 通过、24 失败**：A / Direct 32/38、A / Relay 31/34、B / Direct
38/38、B / Relay 19/34，两项本地连接均通过。前四轮共 **521 通过、63 失败、
0 阻塞、0 跳过**，详见 `iteration-04-summary.json`。
第五轮 **139 通过、7 失败**：A / Direct 38/38、A / Relay 33/34、B / Direct
37/38、B / Relay 29/34，两项本地连接均通过。前五轮累计 **660 通过、70 失败、
0 阻塞、0 跳过**，详见 `iteration-05-summary.json`。
第六轮 **140 通过、6 失败**：A / Direct 38/38、A / Relay 34/34、B / Direct
37/38、B / Relay 29/34，两项本地连接均通过。前六轮累计 **800 通过、76 失败、
0 阻塞、0 跳过**，详见 `iteration-06-summary.json`。
第七轮 **123 通过、23 失败**：A / Direct 35/38、A / Relay 34/34、B / Direct
38/38、B / Relay 14/34，两项本地连接均通过。前七轮累计 **923 通过、99 失败、
0 阻塞、0 跳过**，详见 `iteration-07-summary.json`。
第八轮 **125 通过、20 失败、1 测试阻塞**：A / Direct 38/38，A / Relay 17 通过、
16 失败、1 阻塞，B / Direct 34/38、B / Relay 34/34，两项本地连接均通过。
前八轮累计 **1048 通过、119 失败、1 阻塞、0 跳过**，详见
`iteration-08-summary.json`。阻塞来自短时断网缺少出站丢包计数，
与产品连接错误分别统计。
第九轮 **143 通过、3 失败**：A / Direct 36/38、A / Relay 34/34、B / Direct
37/38、B / Relay 34/34，两项本地连接均通过。前九轮累计 **1191 通过、122 失败、
1 阻塞、0 跳过**，详见 `iteration-09-summary.json`。
第十轮 **129 通过、17 失败**：A / Direct 36/38、A / Relay 32/34、B / Direct
31/38、B / Relay 28/34，两项本地连接均通过，详见 `iteration-10-summary.json`。
第九轮三项失败均在初始连接阶段：Linux 已有会话首次连接未完成安全连接，Android
冷启动安全连接超时，Android 路径切换项完成安全连接后首次查询超时、尚未开始路径
切换。详见 `1171`、`1177`、`1312` 各行的 `connection-stage-analysis.json`。
第六轮六项失败均停在连接或初始查询：`0818` 为普通占用检查前的安全连接超时，
`0846` 为安全连接／协议检查完成后的首次只读会话查询超时，`0847`／`0848`
未完成安全连接，`0856`／`0862` 为 Android 竞争连接的主机查询超时。
各行的 `connection-stage-analysis.json` 区分实际主机会话与客户端保留的历史 ID，
没有将查询失败归为已发生的接管、连错会话或自动创建替代会话。
第 5 轮的 `0709` Android 打开失败具有不同表现：客户端先记录 active／inputReady，
5 秒后才报告期限错误；截图仍显示 Relay 路径、可关闭的错误提示和可用工具栏。
主机只读查询确认同一 Session 已存在。连接后的会话列表刷新可能是错误来源，但当前
日志未标识具体 Android 操作，不能确定归因或据此计为通过，详见该行
`active-terminal-error-analysis.json`。`0714` 则再次记录同步中状态后的重复选择，
这次返回期限错误；与先前三项重复选择一并交由后续修正运行器回归，原失败保留。
第十轮 `1447` 再次确认相同重复选择：首次打开已将目标会话置为 synchronizing，
随后额外 select 释放该 attachment 并再次连接，第二次操作最终返回
`operation_outcome_unknown`。只读核对仍为原 Session，状态 Detached；原失败保留，
证据为该行 `retained-selection-analysis.json`，后续固定运行器已有对应修正。
`0719` 的 Android 主动返回首页后，Linux 普通连接先收到占用，主机随后才记录旧
attachment 释放。最小回归已复现共享客户端只等待写端关闭就结束 detach。
候选修正在原 5 秒控制期限内继续读取，直到主机结束流或返回明确错误；三项针对性
回归通过。CLI 原有 2 秒外层 detach 上限也保持不变，真实 Relay 下超过这一期限
仍应保留为失败。第一次完整检查发现事件接收端退出后的输入通道关闭也被延迟，已调整为
立即关闭输入通道并取消排队命令，再等待网络清理；共享客户端 **97 通过、1 ignored**。
第二次完整 macOS workspace 已 **758 通过、0 失败、13 ignored**，Clippy／fmt
及源码规则通过。Android debug／instrumentation 构建、lint 和 JVM 检查通过，
JVM 结果为 11 项、0 失败（未改动的任务可复用 Gradle 结果）。待安装 debug APK 为
`97b5a20295c2794bd81353e8206e1f063568de304583d6e6adf2715d7b5f523e`，instrumentation
仍为 `144e5d0f40094591bf1690b59ebbdc5756a350b449de7c236d19ff0c2b1055d8`，
详见 `checks/detach-release-android-prepared.json`。三端制品未替换。
首次失败、两次回归记录及候选差异位于
`checks/detach-release-repro/`，尚不能视为真实三端回归完成。
执行至 260 行时的阶段审计核对了 244 项通过记录，未发现路径、执行唯一性或连续性
证据不一致；唯一问题为尚未完成 1460 行覆盖，记录为
`core-final-09/acceptance-audit-progress-0260.json`，不属于最终验收通过。
执行至 444 行时再次核对 405 项通过证据，仍只有尚未完成矩阵覆盖这一审计问题。
通过样本的断网检测最长 **91.676 秒**、恢复可输入最长 **27.635 秒**，主机释放
控制权最长 **86.832 秒**、旧控制端恢复后退出最长 **22.607 秒**，均未超过验收上限。
明细为 `core-final-09/acceptance-audit-progress-0444.json`，失败样本未混入这些耗时。
执行至 753 行时核对了 **683 项**通过证据，仍只有尚未完成矩阵覆盖的问题。
通过样本的检测／恢复最长值保持 91.676／27.635 秒，旧控制端恢复后退出最长
24.115 秒；详见 `acceptance-audit-progress-0753.json`。
执行至 882 行时，**806 项**通过证据的阶段审计仍仅缺少剩余矩阵覆盖；另行核对
已完成行没有重复，均属于计划矩阵。通过样本的检测、恢复、主机释放和旧端退出
最长值分别为 91.676、27.635、86.832、24.115 秒，证据为
`acceptance-audit-progress-0882.json`，仍不代表十轮最终验收通过。
执行至 1001 行时，阶段审计核对了 **912 项**通过证据，仍仅缺少剩余矩阵覆盖；
独立核对已完成行均属于原计划且没有重复。上述检测／恢复／释放／退出最长值保持
不变，详见 `acceptance-audit-progress-1001.json`；随后的 1003 行唯一性核对为
`matrix-progress-1003.json`。
执行至 1171 行时，阶段审计核对 **1051 项**通过证据，仍仅缺少剩余矩阵覆盖。
通过样本的检测、恢复可输入和旧端退出最长值分别为 **96.688、44.639、49.459 秒**，
主机释放最长仍为 86.832 秒，均在原定上限内。记录为
`acceptance-audit-progress-1171.json`；失败和阻塞未混入这些耗时。
完成第九轮后的 1315 行阶段审计核对了 **1192 项**通过证据，仍只有剩余矩阵覆盖
这一问题；独立核对没有重复或计划外行。上述检测、恢复、旧端退出和主机释放最长值
均未改变，详见 `acceptance-audit-progress-1315.json`；随后 1318 行的唯一性核对为
`matrix-progress-1318.json`。

本制品的 Linux 网络补充测试首次未完成：Case B 的原始 UDP 地址反射控制项超时，
尚未进入注入候选地址后的 Direct 断言；Case C 强制 TCP Relay、Case D 受控 QAD
Direct 完成，Case A 在 official-n0 双 NAT 下仍为 Relay。已确认网络命名空间无残留、
转发开关恢复。首次失败保存在 `checks/first-route-network-gate.log` 和
`checks/first-route-network-gate-analysis.json`，待三端协调器释放设备后独立复查。
该项不能记为补充网络测试整体通过，也不替代下列真实产品链路矩阵。

冷连接冒烟中 A / Direct 为 9/12，A / Relay 为 11/12，B / Direct 为 12/12，
B / Relay 为 11/12。失败为 Linux 新身份配对期限、Linux 冷启动隧道期限，以及
三项 Android 首次连接期限。三项 Android 查找分别耗时 16、24、58 毫秒：一项安全
连接耗时接近 5 秒，一项未在期限内完成安全连接，另一项完成协议检查后未在期限内
完成首次只读会话查询。首个有效路由修正没有消除这些剩余启动失败，不能归因于
查找等待或视为已修复。明细、阶段事件和独立主机就绪证据保存在该 run 的
`summary.json`、各失败行的 `diagnostic-window.json`；没有额外发起连接来替代失败。

在扩展矩阵运行期间，离线复核另修正了新身份配对的证据收集：先保存主机授权表及
新增身份，再撤销并保存清理后的授权表；日志收集失败不会阻止这一步或覆盖原始配对
错误。原回归复现了错误覆盖／清理被跳过，审计也复现了将会话查询误作配对核对的
缺口。修正后 **31 项**协调器／审计测试通过，证据为
`checks/pair-reconciliation-before-01.log`、`checks/pair-audit-before-01.log` 和
`checks/coordinator-50.log`。已启动的冷连接和扩展运行仍使用各自固定的旧运行器；
后续核心矩阵采用带 `host-authorization-before-revoke-v1` 标记的新运行器。
产品源码、已安装制品和验收期限保持一致。

`extended-final-08` 的 B / Direct Android 撤销项在识别授权时失败，未执行撤销。
已证实额外的未命名授权来自 `core-final-03/0366-fresh-pair-B-A-A-direct-3` 的 Linux A
临时身份，其公钥与该行保存的 `fresh-identity/A-status.txt` 一致。旧配对返回超时，
11:03:28 UTC 的清理查询尚无该授权，主机在 11:03:29.585 才记录授权提交；单次授权
查询不足以证明此类跨端操作已稳定结束。查询原文保存在
`environment/logs/20261003T110328Z-6455-35848.log`。
当前运行仍保留这一失败；协调器释放设备后将仅撤销有来源证据的残留身份并单独复测。
撤销夹具已改为核对控制端自身公钥，避免用显示名推断 Android 身份；**32 项**
协调器回归和源码规则通过。新的 instrumentation 仅增加本地公钥观测，待本轮完成后
安装验证，当前运行的 APK 未替换。证据为
`checks/orphan-pair-authorization-analysis.json`、`checks/coordinator-51.log` 和
`checks/revoke-identity-android-build-01.log`。

清理时序也有独立回归：旧运行器在一次查询后返回，模拟的在途主机任务随后留下
新授权。新运行器对未成功返回的临时配对，先停止该测试主机并从其 UID 的进程列表
确认 daemon 已退出，再进行授权查询／撤销；旧票据对应的内存状态随进程退出失效。
原始配对错误仍保留，等待进程退出属于失败后的清理，不延长产品配对期限。
**33 项**协调器／审计测试和源码规则通过，证据为
`checks/pair-cleanup-barrier-before-01.log`、`checks/coordinator-54.log` 和
`checks/source-policy-29.log`。当前核心矩阵仍固定原运行器；此修正供后续运行和回归使用。
随后源码复核发现退出确认失败时仍可能继续下一行。最小回归模拟主机进程持续存在，
复现原运行器没有停止矩阵；现保留原配对错误、只读授权查询与清理失败，再停止矩阵，
剩余行记为跳过。**44 项**协调器／审计测试和源码规则通过，证据为
`checks/pair-barrier-stop-repro/`、`checks/coordinator-63.log` 和
`checks/source-policy-41.log`。这是离线复现的运行器边界，未声称当前设备实际触发过。
稳定性循环另有状态统计回归：原代码把故障注入条件不足的 `Blocked` 也写成失败，
现单独保留动作的阻塞状态；整次稳定性验收仍记为未通过。审计同时拒绝含失败、阻塞
或跳过动作却声称通过的报告。两台原始会话在阻塞后继续采样的离线回归通过，协调器／
审计共 **45 项通过**，源码规则通过，详见 `checks/soak-status-repro/`、
`checks/coordinator-64.log` 和 `checks/source-policy-42.log`。后续 `soak-final-10` 会采用
这一运行器；已运行的核心矩阵与旧稳定性记录均保持原样。

该旧失败的控制端 daemon 实际在 11:03:28.067 UTC 记录 `pair_outcome_unknown`，
CLI 却得到 `deadline_exceeded`。独立源码副本已复现：有效授权证明发出后，等待者
期限覆盖了后台操作的结果分类；本地两次相同请求的回复都丢失时也错误返回
`cancelled`。修正保留原 15 秒期限、相同操作 ID 的一次重试、明确拒绝和写入前
失败，仅准确报告可能已提交的接受操作。针对性配对与本地 IPC 回归通过，完整
macOS workspace **752 通过、0 失败、13 ignored**，Clippy／fmt 通过；四个 daemon
文件已按摘要核对后应用工作区，尚未替换三端制品。Linux 和真实设备回归待当前完整
矩阵结束后执行。证据位于 `checks/pair-deadline-repro/`，包含首次失败、候选差异和
`promotion.json`。当前冷连接冒烟的配对失败中，后台错误仍为
`deadline_exceeded`；不能据此断言已发出证明，也不能把所有冷配对失败归因于这一缺陷。

第 7 轮 `0984-fresh-pair-B-A-A-relay-7` 再次复现相同错误分类和晚提交清理问题：
03:06:58.243 UTC 后台报告 `pair_outcome_unknown`，CLI 显示本地请求期限错误；
当时的授权查询未发现新身份，主机在 58.931 记录授权提交。后续 `0991` 保存的
授权表确认该次临时 Linux A 公钥仍为 Allowed，其来源与 `0984/fresh-identity/A-status.txt`
完全一致，详见该行 `pair-error-classification-analysis.json`。该身份也纳入已排队的
精确来源清理，当前运行不增加设备操作。`0985`／`0989` 的默认会话操作结果不确定，
主机只读查询分别找到已创建的原会话；`0989` 停在初始连接，尚未执行长时断网，
不能将它归为恢复逻辑失败。两行的 `connection-sequence-analysis.json` 保留了
查询时点与主机事件，未重发创建操作。

第 7 轮 `1006` 的创建结果不确定且只读查询为空，紧随其后的 `1007` 在初始化阶段
收到占用；主机在后者期间才创建并释放会话。这一时序与前一请求晚到一致，不能
依靠单次空查询隔离独立场景。离线回归复现了晚提交污染下一行，以及退出未确认时
仍继续执行的问题。后续运行器在保存原始错误、只读查询和诊断后，停止该测试主机
并确认旧 daemon 退出；若无法确认则停止矩阵，剩余行保留跳过。审计要求对应的
`session-cleanup-barrier.json`。**42 项**协调器／审计回归通过，证据位于
`checks/session-isolation-repro/` 和 `checks/coordinator-61.log`。该清理仅用于失败后的
独立矩阵行，不重发操作、不延长产品期限，也不改变连续会话稳定性测试。在途核心
矩阵仍使用原运行器，新修正由 v19 后续运行验证。

第 8 轮 `1068-recover-short-A-C-C-relay-8` 保留为环境阻塞：实际丢弃 10 个入站包，
出站计数为 0，未满足双向丢包证据要求。短时测试原先只驱动主机输出；入站包被
丢弃后，Relay 控制端可能没有新的出站业务。后续运行器在故障中增加一次与当前尺寸
不同的真实 resize，并在原 60 秒恢复上限内确认原 Session 应用该尺寸。不会在断网
期间输入 shell 命令，也不强制短时故障出现 reconnecting。双向计数要求保持不变，
43 项协调器／审计回归通过，首个阻塞、回归及差异为 `checks/short-outage-traffic-repro/`，
完整检查为 `checks/coordinator-62.log`。旧阻塞不改为通过，三端回归待后续队列执行。

后续 `final-validation-v19` 已排队，只有 v18 完整执行 1460 行、尝试其一小时运行并
正常停止设备后才会启动。它将构建上述修正，执行 Linux／Android 检查、本地公钥观测、
有来源证据的旧临时授权清理，以及独立的网络补充复查，再执行 112 行配对／撤销／
并发接管／释放后接续／主动 detach 后接续／短时断网定向回归、
88 行扩展、1460 行核心和完整一小时稳定性测试。342 个编译源文件的摘要已固定；
源码变化会阻止排队构建。此队列当前只读取状态，不操作设备；记录为
`checks/after-v18-validation-v19.json`，不同制品的结果分别保存。

当前核心运行另出现一项私有配对创建命令失败，旧错误包装隐藏了全部错误详情，
因此原失败仍保留为原因未确定。后续运行器只保留白名单中的公开错误码，继续隐藏
票据、私有输出和错误详情；回归验证未知字符串不会泄露。**34 项**协调器测试和
源码规则通过，见 `checks/private-error-kind-before-01.log`、`checks/coordinator-56.log`
及 `checks/source-policy-32.log`。这不会改变已启动矩阵的原始记录。

离线并发回归另复现命令日志命名竞争：两个命令同时结束时，旧运行器读取同一个
最新序号，可能覆盖日志。后续运行器在命令开始前分配并保留独立序号，并串行追加
索引；**35 项**协调器测试和源码规则通过，证据为
`checks/concurrent-command-evidence-before-01.log`、`checks/coordinator-57.log` 和
`checks/source-policy-33.log`。截至 22:41 UTC，当前核心进程的 7492 条公开命令
记录未发现重复路径；这是独立复现的潜在缺口，不能据此声称当前证据已被覆盖。

矩阵预览另有一项离线复现：`matrix --group core --repeat 10` 原先忽略筛选，
显示全部 2340 行，实际 `run` 的筛选正确。两种入口现共用筛选逻辑，预览正确显示
核心 1460 行、扩展 88 行及定向回归 48 行，并拒绝未知场景。**36 项**协调器测试
及源码规则通过，见 `checks/matrix-preview-filters-before-01.log`、
`checks/coordinator-58.log` 和 `checks/source-policy-35.log`；在途运行保持原矩阵。

第 3 轮 `0388-handoff-late-B-A-C-direct-3` 中，B 在 23:52:37.873 UTC 已释放
旧控制端，40.177 已将原 Session 交给 Android。测试器随后对同步中的同一会话
又发了一次 `select`，40.391 收到占用错误。真实界面对当前 `synchronizing`／
`reconnecting` 会话只关闭面板；此次属于测试动作偏离界面行为。后续运行器保留
已连接目标的帧并等待原连接就绪，不重复选择；其他会话和明确的占用／接管仍按
原流程处理。复现、主机事件和命令顺序分别保存在该行的
`retained-selection-analysis.json` 及 `checks/android-retained-selection-before-01.log`。
**37 项**协调器回归、源码规则通过，`checks/coordinator-59.log` 和
`checks/source-policy-36.log`；原失败保留，真实三端修正回归将在 v19 的 112 行定向
运行及完整核心矩阵执行。此修正未改动 Android／Rust 源码和已安装制品。

`0407-fresh-pair-B-C-C-relay-3` 已成功配对，随后首次会话操作返回
`operation_outcome_unknown`。原证据中的主机只读会话查询在临时身份清理前、后均为空，
日志窗口未记录创建；这些观测不证明未来不会提交，失败没有重发。结构化核对原先
只保存了配对授权，现对“配对已成功、后续会话结果不确定”另写独立会话查询，保留
授权核对，并由新审计检查。**39 项**协调器／审计回归和源码规则通过，证据为
该行 `post-pair-session-analysis.json`、`checks/post-pair-session-reconciliation-before-01.log`、
`checks/coordinator-60.log` 和 `checks/source-policy-37.log`。在途旧运行的原始记录不改写。

`core-final-09/0278-race-takeover-B-A-C-relay-2` 中主机已提交接管，旧 Linux
仍在初始化时却返回 `cancelled`。独立副本复现了一个可产生相同症状的分支：等待
操作租约时暂存有效的 `TerminalLeaseLost`，随后 EOF 覆盖了这一明确通知。
候选修正立即验证并处理该通知，且不会因迟到的租约回复发送接管；原 5 秒期限保持
不变。5 项定向测试、macOS workspace **755 通过、0 失败、13 ignored**、Clippy
及 fmt 均通过，修正已应用工作区，设备尚未替换。Android 构建、lint 和单元测试
检查通过；当时准备的 debug APK 为
`03a88dee49ade2b55a6f3edb5824303f05e8e257606c51b135b5e5e909413416`，instrumentation
仍为 `144e5d0f40094591bf1690b59ebbdc5756a350b449de7c236d19ff0c2b1055d8`。
证据位于 `checks/lease-wait-repro/`、`checks/lease-wait-android-prepared.json`；v19
现将构建同时包含上述 detach 修正的源码，待安装 APK 已更新为前述 `97b5a202…`，
两版均保留完整制品和检查记录。真实失败没有记录帧内容，不能把
其具体内部执行分支当作已直接观测的事实；原失败始终保留。

扩展矩阵完整结果：A / Direct **21/22**、A / Relay **13/22**、B / Direct **20/22**、
B / Relay **16/22**。18 项失败包括 12 项连接／初始画面期限、2 项创建结果不确定、
1 项 Android 进程自动恢复、1 项 daemon 停止准备，以及上述 2 项授权识别夹具失败。
11 项停在连接阶段、1 项停在准备阶段；其余 6 项进入了各自的场景阶段，仍保留具体
未完成的断言。两项创建结果不确定均以主机只读查询确认了已创建的原 `main`，没有
换操作 ID 重发。通过的 4 项上传断网检测最长 **30.573 秒**，通过的 12 项断网／
输出恢复可输入最长 **8.696 秒**。Android 在两台主机的 Direct 进程重开和模拟器
重启共 4 项均自动选择原命名 Session，后台计数持续增长。详见
`extended-final-08/summary.json`、`acceptance-audit.json` 和
`passed-direct-process-restoration-summary.json`；这些单轮扩展结果不替代核心十轮。

最近一次完整一小时为上一制品的 `soak-final-09`：3606.093 秒、216 次动作通过、
3 次 Android 重开失败，两个原 Session 及 523 条唯一执行记录核对通过，审计无不一致。
它不替代上述当前制品的完整验收。以下保留各轮问题、修复及首次失败的证据。

较早的一项制品加入 NOQ 连接空闲计时修复，源码为
`environment/source-c5bb098c6429602b.tar.gz`。Linux SHA-256 为
`907fb638dd5605e8bd8c72513ad0fd675565368a0aa1e6abcdb243b39f396bca`，debug APK 为
`69a8cf729c0507d992cea7d6c2b7bb4e3b79887a4b8539a723a90f03c6e4af38`，instrumentation 为
`6cf99a747b2541133981d29ecbb90be6a6116ccaea10d2ea522737b04524ab31`。
制品、源码和预检核对已通过；macOS workspace 为 741 通过／13 ignored，
NOQ 单元测试为 428 通过。旧运行阶段见 `checks/final-validation-v12.json`。
以下较早制品的结果继续保留，不能与该制品的通过项混计。

上述旧制品的 `noq-idle-smoke-01` 已完成 **41/48 通过、7 失败**，无 blocked 或 skipped，
证据审计无不一致。A / Direct 为 11/12，A / Relay 为 8/12，B / Direct 为 10/12，
B / Relay 为 12/12。失败为两项首次连接、两项 daemon 退出准备、三项 Relay 初始
快照；没有完成场景断言的行仍计为失败。通过的 15 项长断网／主机断网恢复中，
检测最大 54.147 秒、恢复可输入最大 13.979 秒。
后续 `noq-egress-regression-01` 使用新的绝对时间判据，已完成 **21/24 通过、3 失败**，
无 blocked 或 skipped，证据审计无不一致。三项失败分别为两项首次连接和一项 daemon
退出准备，均未注入断网；21 项已执行的长断网检测最大 56.043 秒，恢复可输入最大
18.544 秒。这是单向丢包补充证据。

双向故障稳定性 `soak-final-07` 在 **2501.711 秒中止，未达到一小时**；141 次动作中
139 通过、2 失败，另有 3 次独立记录的原 Session ID 重开失败。Android 进程恢复时
一次 secure connection 启动达到原有 5 秒期限；另一失败发生在 Linux A 控制 B 时：
双向规则实际丢弃 444 个出站、89 个入站包，主机约 48 秒后释放控制权，控制端却在
120 秒内未进入 reconnecting。首项 NOQ 修复未消除该检测失败；后续放弃路径计时的
独立复现与修复见下文，原运行本身缺少各路径 RTT，无法据此确定其唯一根因。
恢复网络后，三次显式重开原 ID 均在初始快照阶段超时，稳定性运行因而中止。
原 Session 与连续性证据仍保留，证据审计无不一致；这些事实不能计为完整一小时通过。
首次失败见 `soak-final-07/soak-A/failed-action-0006/analysis.json` 与
`soak-final-07/soak-B/failed-action-0141/analysis.json`。
`core-final-05` 在已验证的快照修复进入三端构建前正常中止，保留为 **138 通过、27 失败、
1295 未执行**；证据审计无不一致。其中首轮 146 行完整执行为 120 通过、26 失败。
该结果不满足十轮要求。停止证明为 `environment/stopped.json`（`20261003T184159Z`），
A / B Stopped、C 退出、故障和临时身份恢复记录为空。新制品仍需执行完整核心十轮、
88 行扩展矩阵和完整一小时稳定性运行；当时的后续阶段见 `checks/final-validation-v14.json`。

为继续收集完整时段的样本，后续稳定性测试将最多三次显式重开分配给 Linux、Android、
Linux，仍只选择已确认存在的原 Session ID，后续动作使用实际成功的控制端。
每次失败均保留并使整轮失败；未知操作结果或原会话结束仍立即停止。协调器回归先在
旧逻辑失败，再通过 25 项检查，见 `checks/soak-alternate-reopen-before.log`、
`checks/coordinator-45.log`。旧核心矩阵保留此前固定的运行器，不受此变化影响。

初始快照的传输开销已另行复现：空白 80×25、65×48、131×41 画面分别编码为
33626、51615、87611 字节。真实 `SessionWireServer` 在受控 4 KiB/s 链路上，原逻辑
无法在既有 5 秒期限内送完初始快照。隔离源码中的候选修复增加附件级显式启用的
zlib 画面帧编码，同一回归在 0.342 秒完成，并核对原 Session ID 和完整画面尺寸；
旧客户端仍收到原格式。解压保留原有 8 MiB 上限，拒绝截断、尾随数据和未知编码。
证据位于 `checks/snapshot-size-probe-01/`。这是受控带宽复现，不是公共 Relay 限速
测量。修复已应用工作区，完整 macOS workspace 为 **745 通过、13 ignored**，Clippy 和
格式检查通过；源码摘要见 `checks/snapshot-size-probe-01/validation.json` 和
`checks/compression-promotion.json`。新制品现已安装并通过预检；Linux workspace **751 通过、9 ignored**，Clippy 和格式检查通过，
见 `checks/linux-tests-14.log`、`checks/linux-clippy-14.log`、`checks/linux-fmt-14.log`。
Android 回归和真实三端验证继续执行，剩余断网检测问题仍待完成。

首个快照压缩制品的源码归档为 `environment/source-e0cadc72cd1dcabb.tar.gz`，Linux SHA-256 为
`e3558c3d1f11f4fb34514aaa0c7664ea78b974b0e0064fb2eee95bdd653b432f`，debug APK 为
`6f8ea24898d03fe9d9f97ba4e2aba59f17fc4bb52b043ca53ffa254ff5aa8ce1`，instrumentation 为
`6cf99a747b2541133981d29ecbb90be6a6116ccaea10d2ea522737b04524ab31`。完整摘要见
`checks/compression-installed.json`；A / B 二进制和已安装 APK 均通过核对。
Android 构建日志确认 `voytty-proto`、`flate2`、`voytty-client` 与 native 库重新编译，
instrumentation 源码没有变化，因此测试 APK 摘要与上一制品相同。

`final-validation-v13` 的 Linux 全量检查在链接 `session_limits` 时因 VM 磁盘耗尽
中止，未计为测试通过。首次清理仅释放约 7.64 GiB 的旧测试可执行文件，仍留下
约 16 GiB 增量缓存。该次失败和正常停止记录均保留；随后进一步清理旧测试程序及
增量缓存，保留新编译结果、源码和证据，使可用空间恢复至 23.65 GiB。
`final-validation-v14` 使用同一制品重跑所需检查，未修改代码、编译配置或测试时限。
记录见 `checks/linux-tests-13.log`、`checks/linux-cache-cleanup-04.json`。

该轮 Rust 检查及 Android lint / 单元测试通过，基础 instrumentation 为 **40 通过、
1 失败、4 条件跳过**，通知权限允许／拒绝的前台启动回归合计 4/4 通过。失败发生在
`SettingsUiTest` 返回首页后的 1 秒准备等待，尚未进入通知开关断言；三端随后正常停止。
定向对照在无保存会话时通过、在冷启动恢复保存会话且应用出站受阻时于相同行失败，
见 `checks/settings-restore-before-01/`。测试现将返回首页的清理等待与设置断言分开，
原设置断言的 1 秒等待、产品期限和验收 120 / 60 秒边界均未改变。

修正后相同对照各重复 10 次，**20/20 通过**，原应用状态逐字节恢复；见
`checks/settings-restore-after-01/results.json`。测试 APK 更新为
`de36c7eeb537826c6cb0d59ea0c9a026d7e4ba4895d5f8fce01ff0cc63c5115a`，源码归档为
`environment/source-bf86de9aafd6ea02.tar.gz`；Linux 二进制和主 APK 摘要保持一致。
`checks/final-validation-v15.json` 记录后续 Android 检查与最终矩阵。

该测试 APK 的基础 instrumentation **41 通过、4 条件跳过**，前台启动回归 **4/4
通过**，Android lint / 单元测试、协调器 25 项与源码规则通过。真实 Relay 冒烟
`compression-relay-smoke-01` 完整运行 **10/12 通过、2 失败**，证据审计无不一致。
两项失败为 B 主机上 Linux A 的首次连接；冷启动一次收到 `operation_outcome_unknown`，
主机只读列表确认 `main` 已创建，未重发变更操作。另一项在 secure connection 阶段超时。
两项均未注入断网。四项 Relay 长断网恢复全部通过；冒烟仍不等同完整矩阵。

剩余断网检测另有独立协议复现：在确定性模拟链路中延迟实际 QUIC 数据包，建立 20 秒 RTT 路径，再建立
2 毫秒路径并放弃旧慢路径；连接在完全没有入站数据的情况下直到 127.6 秒才关闭。
原因是为清理保留的已放弃路径仍决定连接空闲期限。新增修正仅让连接空闲计算使用
尚未放弃的路径，并保留最后一次有效收发的计时起点；路径清理、密钥丢弃和连接关闭
的等待仍计算所有保留路径。未改变协商期限、三倍 PTO 策略或验收界限。
回归覆盖立即放弃、断网 20 秒后放弃，两者均在 30 秒关闭；慢路径仍可用时仍为
127.6 秒。隔离候选 **429/429 通过**且 Clippy 通过，见
`checks/noq-abandoned-idle-repro/`。该复现证明库内缺陷，不能证明旧稳定性运行中的
实际各路径 RTT。修正随后完成整合检查和三端构建，结果见下文。

完成上述冒烟后正常停止 v15，未完成的 `core-final-06` 保留为 **1 通过、1459 未执行**。
当时的后续流水线为 `checks/final-validation-v16.json`；其实际完成及中止情况见下文，
不作为完整核心十轮通过的证据。

包含第二项 NOQ 修正的新制品已安装并通过预检，源码归档为
`environment/source-5288a61758fb27db.tar.gz`。Linux SHA-256 为
`234dc554cd0bde8f77879216e6d26cace927128b47f34a478813144f68517a99`，debug APK 为
`5ecdfc4bb4a38fb56802f26f103a7f11d32fff226185219ab8e6d73c23261c49`，instrumentation
APK 仍为 `de36c7eeb537826c6cb0d59ea0c9a026d7e4ba4895d5f8fce01ff0cc63c5115a`。
完整记录为 `checks/idle-active-installed.json`。重建前仅清理 A 中 62 个旧测试程序及
增量缓存，使可用空间恢复至 35.21 GiB；见 `checks/linux-cache-cleanup-05.json`。
该制品的 macOS workspace **745 通过、13 ignored**，Linux workspace **751 通过、
9 ignored**，均为 0 失败；Linux Clippy / fmt 通过。证据为 `checks/rust-tests-15.log`、
`checks/linux-tests-15.log`、`checks/linux-clippy-15.log` 和 `checks/linux-fmt-15.log`。
工作区 NOQ 全量 **429/429 通过**，macOS Clippy / fmt、Android lint / 单元测试通过；
Android 基础 instrumentation **41 通过、4 条件跳过**，前台启动回归 **4/4 通过**。
协调器 25 项、源码规则、依赖与许可证检查、密钥扫描通过。对应证据为
`checks/noq-tests-16.log`、`checks/rust-clippy-15.log`、`checks/android-checks-12.log`、
`checks/idle-active-android-runtime.json`、`checks/coordinator-47.log`、
`checks/source-policy-22.log` 及 `checks/idle-active-{cargo-deny,secret-scan}.log`。
`idle-active-recovery-smoke-01` 已完整运行 **14/16 通过、2 失败**，无 blocked / skipped，
证据审计无不一致。A / 要求 Direct 为 2 通过、2 次 Android 首次连接失败；A / Relay、
B / Direct、B / Relay 各 4/4 通过。两项失败均停在 secure connection 的原有 5 秒期限，
尚未执行断网注入；没有在失败后继续选会话以掩盖错误。实际执行的 14 项双向断网恢复
均通过，检测最长 **53.795 秒**，恢复可输入最长 **17.631 秒**。证据及首次分析见
`idle-active-recovery-smoke-01/summary.json` 和各失败行的 `analysis.json`。
`core-final-07` 在确认场景间准备状态泄漏后正常停止，保留为 **28 通过、10 失败、
1422 未执行**，证据审计无不一致。主机断网恢复项通过后，下一项新身份配对在主机仍
为 `Degraded` 时开始；旧协调器仅在切换传输模式时等待 Online。已增加每项独立远程
场景开始前的主机本地就绪检查，不向 peer 发请求，不预启动冷控制端，也不改变场景内
120 / 60 秒期限。准备失败仍保留为失败，本地终端不要求网络 Online。该修正不意味着
所有首次连接超时均已定位。旧行为的四个失败断言见 `checks/host-readiness-before-01.log`，
修正后 **28 项**协调器／审计回归通过，见 `checks/host-readiness-after-01.log`。
三端停止证明为 `environment/stopped-20261003T194846Z.json`；停止原因与流程分别见
`checks/host-readiness-stop-v16.json` 和 `checks/final-validation-v16.json`。
相同产品制品的新轮次由 `checks/final-validation-v17.json` 记录，完成主机就绪定向
复测和完整一小时 `soak-final-09`。该轮数据写入并审计后已正常停止三端，后续完整
矩阵改用下述新增路由修正后的三端制品。尚未执行的 `extended-final-07` 保留为
**88 skipped**，审计无不一致；没有把它记为扩展矩阵通过。
相同制品的 Linux 双 NAT／受控 QAD 补充测试已完成：测试函数通过、四个子场景完成。
公开 `official-n0` 的普通双 NAT 子场景仍选 Relay；显式注入候选地址的 Direct、
非 DNS UDP 被阻断时的 WSS/TCP Relay，以及受控 QAD 的自动 Direct 均符合各自
断言。没有把普通双 NAT 的 Relay 结果记为 Direct 通过。namespace 无泄漏，转发设置
已恢复；见 `checks/idle-active-network-gate-summary.json` 和原始 `.log`。这些结果与
三设备真实产品链路分开记录，真实跨物理网络的地址发现仍待单独验收。
`host-readiness-smoke-01` 完整 **16 项**定向复测为 **12 通过、4 失败**，无 blocked /
skipped，审计无不一致。四项主机双向断网恢复全部通过，检测最长 **51.447 秒**、
恢复可输入最长 **0.736 秒**。两次断网后的 Android 新身份配对分别等待主机本地
Online 约 **16.975 秒**、**5.679 秒**后通过。四项失败均已通过主机准备检查，仍在
独立的首次连接阶段失败（Android 冷启动一次、已有会话首次连接三次），阶段日志见
各失败目录的 `stage-analysis.json`。没有通过选会话或额外请求覆盖原错误。完整统计
为该目录的 `summary.json`；Android 冷启动 Direct 截图已人工核对并保存
`0005-cold-A-C-C-direct-1/visual-review.json`。

`soak-final-09` 已完整运行 **3606.093 秒**，**216 次动作通过、3 次失败**，整轮仍判
失败。A 为 109 通过／2 失败，B 为 107 通过／1 失败；三项失败都是 Android 进程重开
的原有 5 秒启动期限（动作 16、111、180），各自保留首次日志、只读原 ID 核对及之后
明确的原 ID 接续记录。没有额外重开失败。两个原 Session 最终仍在，A 的 262 条、
B 的 261 条执行记录均通过连续性／唯一 nonce 核对，后台计数分别由 1 增至 14097、
6933。断网检测最长 **38.239 秒**，网络恢复后可输入最长 **3.147 秒**；证据审计无
不一致，见 `soak-final-09/summary.json`。相同种子的第 141 个 B 主机／A 控制端长断网
动作本轮以检测 32.491 秒、恢复 0.687 秒通过，实际丢弃 154 个出站、85 个入站包；
对照为 `checks/soak09-action141-comparison.json`。真实路径／PTO 历史没有被固定为
与旧失败完全相同，不能据此唯一归因旧失败。

随后在隔离副本复现另一处可避免的冷连接延迟：地址查询已返回目标身份的有效 Relay
后，旧代码仍等待其他来源结束，可能耗尽 2 秒查询预算。新增回归在旧代码下超过
500 毫秒失败；提前使用首个有效结果后通过。修改保留目标身份校验、Relay 数量上限、
fresh / cache / ticket 顺序及原有产品期限；无可用结果时仍按原预算等待。修正已加入
工作区，隔离完整 Rust workspace **746 通过、13 ignored、0 失败**，工作区 Clippy /
fmt 和源码规则通过；其他 278 个 Rust／协议／构建清单文件与被测副本一致。证据为
`checks/first-route-repro/{before-01.log,after-01.log,workspace-01-summary.json,promotion.json}`。
这证明了确定性等待问题，但真实失败日志未记录各查询来源的独立时序，不能将其宣称
为所有首次连接失败的唯一根因。上述 `soak-final-09` 使用修正前已安装制品，独立保留
完整时段；三端正常停止后，`final-validation-v18` 已开始构建与安装新制品。新 Android debug 与 instrumentation
APK 的本地构建、lint、单元测试已通过，见 `checks/first-route-android-build-checks.json`；
本地预构建没有提前更换稳定性测试中的 APK。自动衔接流程只在上一轮完成审计且三端正常停止、
故障记录清空后开始 `checks/final-validation-v18.py`。它包含 Linux 全量检查、Android
运行检查、**48 项**冷连接定向复测、**1460 行** `core-final-09`、**88 行**
`extended-final-08` 及新制品的完整 **3600 秒** `soak-final-10`；这些结果尚待执行。

## 环境与边界

| 设备 | 实际配置 |
| --- | --- |
| A | `voytty-md-20261003t024103z-a`，Ubuntu 24.04 ARM64，VZ，4 vCPU / 6 GiB / 60 GiB，账户 `voytty-a`（UID 2001） |
| B | `voytty-md-20261003t024103z-b`，同配置，账户 `voytty-b`（UID 2002） |
| C | `voytty-md-20261003t024103z-c`，API 36 Google APIs ARM64，2 vCPU / 3 GiB，`emulator-5586`，独立可写数据目录，`adb root` 已验证 |

Linux 二进制在 A 构建后复制到两端，并核对 SHA-256；Android debug 和 instrumentation
APK 来自同一工作区。各次运行的 manifest 记录源码基线、改动文件摘要和制品摘要。
仅使用独立测试账户的 `~/.voytty`，未操作开发者真实身份与会话。默认 `official-n0`。

原方案的单独 `vzNAT` 无法让两个 VM 互访。实际预检复现这一限制后，保留 `vzNAT`
用于 Mac / C 访问 VM，增加无需特权网络组件的共享 `user-v2` 网卡用于 A ↔ B。
该边界与 [Lima vzNAT 文档](https://lima-vm.io/docs/config/network/vmnet/)
及 [user-v2 文档](https://lima-vm.io/docs/config/network/user-v2/) 一致。
修正后的 ping 与 UDP 预检覆盖 Mac → A/B、C → A/B、A ↔ B，均已通过。
原始预检失败仍保存在 environment 目录，不计为产品场景通过。

## 判据和运行方式

`tests/e2e/multi-device.py` 提供 create / start / build / preflight / matrix /
run / soak / collect / cleanup / stop。Linux 使用真实 SSH PTY 运行正式 CLI；
Android instrumentation 通过私有阶段信号调用真实 Application repository，使用真实
Activity 生命周期、输入、resize 和上传控制器。它不是 UI 点击覆盖率测试。

每个场景保留 Session ID、shell PID、cwd、shell 变量、后台进程 PID 与计数器，以及
主机侧命令执行 nonce。控制权交接后检查旧输入无效、新命令只执行一次、旧 resize
不覆盖新控制端。自动恢复坚持原 Session ID；主机 daemon 崩溃/主机重启使原会话结束。

Direct / Relay 依据产品实际选中路径判断。最终运行的强制 Relay 阻断三端测试 UID 的
非 DNS UDP（IPv4 和 IPv6），随后建立新连接；guest 重启后重新应用该设备的规则。
较早运行的主机单端 UDP 阻断结果单独保留，仍可用 `--relay-scope host` 复现。
Direct → Relay → Direct 在已有连接上注入单端故障。
断网检测上限 120 秒，恢复可输入上限 60 秒。短暂丢包不要求一定出现 reconnecting。
没有放宽产品超时、伪造路径或使用预热请求掩盖冷连接。
最终核心矩阵的 Linux `cold` / `first-existing` 先停止控制端 daemon，再由真实 CLI
自行启动，连接计时包含 daemon 与 Endpoint 启动。较早诊断的同名 Linux 行曾等待
本地 Network Online；它们只覆盖网络端点就绪后的首次 peer 连接，不能替代完整的
CLI / daemon 冷启动。Android 场景仍在新应用进程初始化后发起首次主机请求。
独立远程场景开始前，主机通过本地 status 在 60 秒内报告 Online；此检查先于控制端
冷启动，耗时及结果单独记录为 `host_readiness`，manifest 标记
`local-host-online-before-scenario-v1`。它隔离前一场景的准备状态，不用于场景内恢复
或稳定性测试的连续动作。冷控制端没有 Online 预等待，也没有 peer 预热查询。

所有变更结果不确定的失败均保留；通过主机只读列表确认结果，禁止自动换操作 ID
重发。Fault 的独立守护进程在协调器管道断开后恢复自己添加的规则；SSH / adb
管理通道不受测试 UID 故障影响。正常退出停止三端，保留实例与证据供复查。

核心组包含本地/远程首次连接、配对、detach、接管、长短断网、主机断网、跨设备接续、
并发竞争和路径变化，展开为每轮 146 行。进程/系统故障、会话/授权失效、Android
生命周期、持续输出/resize/输入及上传展开为另外 88 行。最终命令分别运行核心十轮、
扩展一轮和固定种子 `20261003` 的 3600 秒稳定性检查。场景准备或首次连接失败时，
后续故障未被执行；报告中的失败阶段用于区分这种情况。

## 本轮发现与修复

反向配对曾出现“主机已提交授权，控制端 normal confirmation 超时”。最小回归在
两个真实 Iroh daemon 之间先建立较小身份发起的连接，再请求反向授权确认。在回环
链路加入 60 毫秒延迟后，修复前于 15 秒期限失败。原因是 Welcome 的 `finish()`
仅排队 FIN，重复连接裁决立即关闭连接，可能丢弃尚未送达的 Welcome。

`connection_broker` 现在在原有握手期限内等待 Welcome 发送流完成，再允许重复连接
裁决关闭承载连接。FIN 确认或正常读完单个消息后的 `STOP_SENDING(0)` 均可完成；
非零停止码或连接错误仍失败。客户端仍按完整帧读取，不要求额外等待传输 EOF。
未改变授权方向、去重或产品期限。相同 60 毫秒回归修复后通过；对应集成用例和
可重复的延迟运行脚本已新增。

控制端重启还暴露了旧 primary 尚未超时、却按候选键排序淘汰新连接的问题。新增
可选 16 字节 `ConnectionIncarnation`（Hello 字段 8、Welcome 字段 7）：同一连接
代的并发/反向候选继续确定性裁决；经身份与授权检查的新连接代退休旧候选。
daemon 的每个 peer 在候选表清空后生成新代，Android 在没有可复用连接时生成新代，
同时覆盖进程重启和单边 QUIC 超时。旧代墓碑在完整握手窗口内拒绝迟到握手，数量
受连接上限约束。真实 Iroh 回归保留旧连接、用排序更大的新候选连接，并验证迟到的
旧 Hello 不会抢回连接。字段缺省兼容旧端；双方升级后才能获得完整的连接代隔离。

Linux 回归复现 Unix socket 快速 unlink/rebind 后 inode 与时间戳被复用，旧所有权
令牌因此可能误删新 socket。Linux 现在用 `O_PATH | O_NOFOLLOW` 文件描述符保持
原 inode 存活，恢复和清理仍验证文件身份；128 次快速替换回归通过。

VZ / user-v2 环境还复现 `UDP_SEGMENT` 批量发送失败和初始快照停滞。daemon 采用与
Android 相同的普通 UDP 数据报发送方式，关闭 GSO；这是吞吐与虚拟网卡兼容性的取舍，
本轮没有测量最大吞吐。相同 Direct 连接/接管子集由初始快照超时恢复到 12/12 通过。
这项修正没有消除全部 public Relay 冷连接或大快照超时，剩余失败继续计入验收。

持续输出时 Android → Linux 接管复现 `not_synchronized`：接管改变控制端颜色后需要
新快照确认，成功响应却可能先发出，CLI 也过早预取历史。终端流的唯一 writer 现在
先发送接管要求的快照，再发送成功响应；CLI 到 Active 后才开始历史预取。固定调度
回归在修复前收到错误顺序，修复后验证正确顺序、必需 ACK 和旧控制端仍无权输入。

重连与接管交错还复现旧 Android 每秒重试 `session_occupied`，一直停留在恢复中。
这会留下新控制端退出后被旧设备自动接续的风险。现在其他控制端占用是终止恢复的
明确结果；主机只有在“相同授权主体、相同 resume view 的旧传输尚未释放”时返回
可重试的传输错误。定向回归覆盖停止占用重试和等待自身旧传输这两个分支。

`core-final-01/0019-takeover-A-B-C-direct-1` 触发真实应用崩溃：
`ForegroundServiceDidNotStartInTimeException`。快速收到占用结果后，应用用 `stopService`
取消了尚未完成前台提升的启动请求，系统立即报告“仍在等待 start foreground 时停止服务”。
仅将提升移到 `onStartCommand` 不足以修复；该中间制品在 `android-takeover-final-01`
中仍有 4/20 次接管崩溃。新增最小测试在同一主线程轮次启动后立即取消，修复前复现
相同进程崩溃，见 `checks/android-pending-start-before.log`。

现在保留已提交的服务启动，由每次 `onStartCommand` 先提升，再依据最新状态自行停止。
停止过程中收到的新前台连接请求，等销毁回调完成后重新评估；失败或系统停止不触发
后台重启。前台提升顺序遵循
[Android 前台服务启动流程](https://developer.android.com/develop/background-work/services/fgs/launch)。
先前的普通快速停启压力测试在旧 APK 上也通过，未覆盖“启动尚未送达就取消”的分支，
不能用它证明原竞态已修复。首次失败、最小复现及真实三端回归分别保留。
发现崩溃后中断的核心和扩展轮次，其未完成行明确记为 skipped。
最新制品在通知权限允许／拒绝下各执行“启动后立即取消”和快速停启两项测试，
4/4 通过，见 `checks/android-pending-start-after.json`。真实链路随后在
`android-takeover-final-02` 执行 Linux → 本地及 Linux → Android 各十次；
其中 Linux → Android 为 10/10，Linux → 本地为 9/10，合计 19/20 通过；首项在
首次连接阶段超时。整轮无 instrumentation 崩溃，通过项的后台计数推进与执行记录复核通过。

完整 Rust 检查还暴露既有配对日志测试偶发计数 0 而非 1，单独执行正常。本次将其
日志断言隔离到子进程，保留“恰好一次”和票据内容不泄露的原断言，避免并行 tracing
dispatcher 的全局 callsite 竞争。此前同一现象见已有会话连续性记录。

测试器修正与产品修复分别保留：PTY 扩展 CSI 解析、重复关闭描述符、VM 重启后 PID
复用、Android mailbox 发布顺序、正常 guest reboot 与强杀模拟器进程的区别，以及
重启恢复时添加独立 `main` 会话以检测错误回退。没有将测试器异常或未完成断言记为通过。
SSH PTY 尺寸变更还需显式向测试器拥有的 SSH 子进程发送 SIGWINCH；仅设置
TIOCSWINSZ 未触发远端 resize 的早期结果保留为夹具失败。
`core-final-02/0040-recover-long-A-B-B-relay-1` 又暴露了输入就绪观察的竞态：
底层 driver 报告 Active 时，CLI 仍可能等待断网期间积累的 resize 同步。测试器现在
同时观察真实 PTY 的输入呈现状态；本轮 shell 夹具用光标可见表示输入栅栏已打开。
恢复后仍在原有 60 秒绝对期限内只发送一次命令，不重试 nonce。协调器回归 9/9
通过，见 `checks/coordinator-19.log`；该中间核心轮次的 47 通过、38 失败和 1375
未完成行分别保留，独立的 Relay 超时不归因于此夹具问题。
真实定向复测 `cli-ready-recovery-01` 的四项长断网恢复为 3 通过、1 准备失败：
Linux A → B 和 Android → A/B 的 Relay 恢复均通过，Linux B → A 因 daemon 停止
超时未开始断网。后续短断网测试先启动有界的真实 PTY 输出，保存故障链的实际丢包
数量；未观察到丢包的规则注入只记为 blocked，避免空闲期间没有流量却声称验证了丢包。

## 原始证据

本机根目录：`target/verification/multi-device/`。`environment/` 保留 VM / AVD 配置、
制品摘要、命令日志和预检。每个 run-id 目录保留矩阵、汇总、单项结果、耗时、PTY、
Android 原生日志、logcat 和关键截图。包含测试私有数据，权限限制为当前用户，未纳入 Git。

早期 `smoke-01` 至 `smoke-06` 保留配对失败、测试终端解析/清理问题及冷连接失败。
`smoke-07` 的连接/接管子集为 18/20 通过；两项 Android Direct 冷连接附近的
`deadline_exceeded` 仍按失败记录，不能用随后连接成功覆盖。

`extended-01` 使用调整前夹具，80 行中 39 通过、41 失败。命名会话恢复夹具和上传
故障时机随后修正，原结果未覆盖。`relay-direction-01` 的 8 行诊断为 7 通过、1 失败；
单端限制与双端限制分别记录，样本不足以将全部冷连接失败归因于路径选择。

较早产品制品的扩展矩阵 `extended-final-02` 已完成全部 88 行，原始报告为 75 通过、
13 失败，无 blocked 或 skipped。随后离线审计发现 Android 打开主机失败后，测试器
仍可能继续发送指定会话请求，掩盖首次错误；该轮因此保留为中间证据，不能作为最终
验收结果。修正后完整复跑为 `extended-final-03`，后续核心十轮也使用修正后的测试器。
审计记录见 `cold-open-audit.json`，测试器修复前后回归见
`checks/android-open-before.log`、`checks/coordinator-13.log`。
原始结果按主机与要求路径分组如下；准备阶段失败的行没有实际完成
该行要求的全部路径和故障断言。

| 主机 / 路径 | 通过 / 总数 |
| --- | --- |
| A / Direct | 18 / 22 |
| A / Relay | 21 / 22 |
| B / Direct | 21 / 22 |
| B / Relay | 15 / 22 |

13 项中，7 项在首次连接阶段失败，6 项在场景操作阶段失败。操作阶段失败涉及
控制端重启、Android 杀进程后恢复，以及持续输出中的接管。连接阶段失败涉及
生命周期、网络切换和上传行；这些行不能作为相应故障已验证的证据。
全部通过项的后台计数推进已离线复核，未发现新的前台服务启动崩溃，见
`extended-final-02/acceptance-audit.json`。

三项 `operation_outcome_unknown` 分别发生在 A 上持续输出时接管，以及 B 上
Wi-Fi 切换、上传断连场景的首次连接准备中。对应目录保留只读
`outcome-reconciliation.json`，没有重新生成操作 ID 重发。持续输出接管那一项，
主机日志显示已提交控制权变更，但客户端未在期限内确认成功；详见该行的
`analysis.json`。该结果仍为失败，不能由主机侧已经提交改记为通过。

`soak-final-01` 在 2801 秒时主动中断，以便用上述修正后的测试器重新计时；它不是
完整的一小时结果。已完成 118 个操作，其中 109 通过、9 失败，另一个在执行中的
操作因协调器中断而终止。失败包括 Android 进程恢复/连接超时，以及 Android 和
Linux 各一次超过 120 秒未报告断连。后二者中主机已经释放控制权，前端仍未报告
reconnecting；根因尚未确定，不能以原 Session 仍在而改记为自动恢复通过。
修正测试器后的 `soak-final-02` 在 419 秒提前失败：23 个操作中 21 通过、2 失败；
断连检测超时后，显式重连原 ID 又收到 `transport_unavailable`，测试因此停止，
没有新建会话来继续计时。对应故障链记录实际丢弃了 676 个 IPv4 数据包，见
`environment/fault-evidence/ZMD_5a57e5c613d6.json`。这也不是完整一小时结果。
`soak-final-03` 又在初始连接阶段超时，7 秒内结束，未开始随机操作。所有未满一小时
的运行分别记录，不能拼接成一小时通过结果。
`soak-final-04` 已完成 3642.8 秒，固定种子下执行 143 个操作：134 通过、9 失败，
整轮判为失败。失败为三次超过 120 秒未检测断连、三次 Android 进程恢复超时、
三次普通连接超时而未返回占用结果。A/B 原 Session ID、shell 基线与后台任务在
结束时仍保留；331 条命令记录的 nonce 唯一、基线一致、计数推进，离线审计通过。
该轮 PTY 文件名在部分显式重新打开时被复用，无法提供每次打开的完整独立原始 PTY；
命令证据和失败日志仍保留，限制见 `soak-final-04/pty-evidence-note.json`。
后续测试器已改用唯一 PTY 文件名，并在失败点单独复制当前终端证据。

`core-final-03` 在续接时发现协调进程已退出，未留下完整结束记录。原始 568 行结果
另存后，将其余未执行行明确记为 skipped：363 通过、205 失败、892 跳过。
原因未确定，不能视作核心十轮完成。通过项的离线证据审计没有不一致。
其中四项并发接管失败由测试器在已知占用响应后未继续显式接管造成，另有一项在
第二个并发请求尚未完成时过早判定胜者改变。原失败不改写；测试器现等待双方终态，
并允许已明确请求的接管在 `session_occupied` 后继续，其他未知错误仍禁止重试。
协调器及离线审计回归为 13/13，见 `checks/coordinator-25.log`。
测试器现将初始准备/失败后的显式接续限定为最多三次原 ID 连接，每次错误单独保存，
任何一次失败都使整轮失败；未知操作结果或原会话消失立即停止。该辅助流程不用于
普通场景或冷连接测试。计时从 A 的原 shell 与后台任务建立后开始，准备时间单列。
相关边界回归见 `checks/coordinator-19.log`。各 run 都保存自己的测试器源码与摘要。

剩余连接与断线检测问题正在验证 Iroh `1.3.0` 候选升级。上游
[PR #4512](https://github.com/n0-computer/iroh/pull/4512) 修复 Relay 发送队列满时阻塞
remote actor 收件箱的问题，并覆盖拥塞时 Direct 发送、查询和关闭的回归。原固定
`1.0.3` 中存在对应的串行等待路径；症状相关，但不能仅据上游修复认定本轮全部超时
已有共同根因。工作区现固定 `iroh` / `iroh-relay` 为 `1.3.0`，应用检测与恢复期限未修改。
Android DNS 暂保留接收 `SocketAddr` 的兼容 API，以保留 IPv6 接口 scope；新构造器
仅接收 `IpAddr`，直接迁移会丢失该值。隔离兼容性检查和依赖公告／许可／来源检查通过，
全量检查随后完成：macOS 738 通过／13 ignored、Linux 744 通过／9 ignored，均无失败；
两端 Clippy、Android 构建／lint／单元测试通过。新依赖下 60 ms 反向配对为 10/10，
双 NAT / 受控 QAD 四项门禁也通过，结论仍为 `GO_WITH_DEFERRED_ADDRESS_DISCOVERY`。
证据为 `checks/iroh-1.3-*`。真实三端 `iroh-1.3-smoke-01` 为 12 通过／12 失败；
其中六项并发接管由测试器误判 CLI 明确的 `lease_lost` 退出造成，六项是 B / Relay
首次连接失败。修正测试器后的 `iroh-1.3-race-01` 为 5 通过／3 失败，其中一项 Android
接管后收到 `not_synchronized`。升级并未消除 public Relay 超时，旧失败继续保留。

随后针对地址发现建立了确定性最小复现：一个来源先返回有效 Relay 地址，另一个
来源一直不结束。原实现将结果列表放在超时 future 内，2 秒期限届满时丢弃了已验证
的有效结果，回退到可能过时的缓存。现在保留期限内已收到、与目标身份一致的地址，
期限和候选上限不变。修复前两项回归为 1 通过／1 失败，修复后 2/2；另一项验证
不得保留其他身份的结果，见 `checks/partial-lookup-before.log` 和 `-after.log`。
该版三端已构建安装并通过预检，macOS workspace 为 740 通过／13 ignored；Clippy、
Android lint／单元测试通过，基础 instrumentation 为 41 通过／4 条件跳过，通知
允许／拒绝的前台服务回归合计 4/4。真实矩阵仍需完成，不能据单元回归认定 Relay
故障全部修复。

接管日志复核又引出快照确认交错的确定性复现：较旧快照的 ACK 到达时，主机已发送
较新快照，于是补发相同 revision；客户端依次确认两份已收到的快照，第二次合法
ACK 却被拒绝为 `not_synchronized`。`checks/overlapping-snapshot-before.log` 保留
原失败。当前修复记录最后一次已确认 revision，重复确认不再关闭附件，也不得
放行或覆盖更新的同步屏障；未发出过的 revision 仍拒绝，已失去控制权的附件仍拒绝。
定向回归已通过，三端复跑结果见下文。这一最小复现不能单独证明所有同名错误都来自相同交错。

两项新增修复后的 Linux workspace 为 747 通过／9 ignored，Clippy／fmt 通过。
macOS 首轮在诊断日志并发轮转用例的 flush 校验失败，原记录为
`checks/rust-tests-8.log`；该用例随后单独复测及 10 次重复均通过。保留原失败后，
用 `RUST_TEST_THREADS=1` 重新执行完整 workspace，结果为 741 通过／13 ignored，
Clippy 也通过。用例内部显式启动的并发进程仍保持并发。此结果不能消除首次失败的
不确定性，也没有修改日志写入期限或该用例断言。证据为 `checks/rust-tests-9.log`、
`checks/diagnostic-flush-repeat.json`、`checks/linux-tests-9.log`。

快照修复版本的 Linux SHA-256 为
`c492a2d17690104be5b6af94a32f1f412e2713c8d8ee3a82316728342bc810c4`，
源码归档为 `environment/source-218af571116dfd1a.tar.gz`；debug APK 为
`1ae0c4a7c787026c44e9da949dbe7dbf431d6e55831cb9a9d4a3c58513bc56f7`，
instrumentation APK 为 `6cf99a747b2541133981d29ecbb90be6a6116ccaea10d2ea522737b04524ab31`。
快照修复只改动 daemon，Android 制品与已验证的 partial-lookup 版本摘要相同。
三端预检通过。一次 B 安装后启动因 `daemon_already_running` 失败，随后显式启动和
完整重新安装成功；首次失败及当时日志单列在 `checks/build-8-failure-analysis.json`，
根因未确定，不能将随后成功改写为首次成功。

进一步的隔离回归确认一个启动竞争：只读状态／诊断检查也会短暂持有 `daemon.lock`，
原 daemon 启动只尝试一次，将观察者误判为另一个 daemon。新增 `single_instance`
回归保留观察锁 100 ms，再释放，修复前稳定返回 `DaemonAlreadyRunning`。启动现在
在原有 5 秒预算内等待锁，仍须获得真实独占所有权，未延长 CLI 启动期限。
修复后同一回归通过，见 `checks/startup-observation-before.log`、`-after.log`。
它证明该竞争存在；现场首次失败没有锁持有者追踪，不能据此排除其他同名失败原因。

上一组制品的 `overlap-smoke-01` 完成 24 项，18 通过／6 失败，审计无不一致；
失败阶段为首次连接 4 项、并发接管中的连接 1 项、daemon 重启准备 1 项。
6 个通过的长断网样本检测为 36–56 秒，恢复为 1.5–24.3 秒；未再现重复快照 ACK 错误。
`soak-final-05` 在确认上述启动缺陷后主动停止：367.4 秒内完成 25 个通过操作，无
操作失败或准备重试，整体仍记为未完成／failed，不能算一小时通过。对应停止原因
记录于 `checks/final-validation-v9-stop-reason.json`，停止检查确认 A/B/C 已停止、
待恢复故障规则记录为空。随后恢复实例，安装包含启动修复的制品并完整重新计时。
启动修复版本的 Linux SHA-256 为
`3417c7f1e2d5c7958ab9e5fc2d0c644c5e3a5c5a2ca1d626d1a39ac9087facb3`，
源码归档为 `environment/source-4a4d82e350335be9.tar.gz`；Android 两个 APK 的摘要保持
不变。版本核对覆盖全部已修改的 Rust／Kotlin／协议源文件及 Cargo 清单与锁文件，
新一轮预检通过，完整运行计划和当前阶段保存在 `checks/final-validation-v10.json`。
该精确源码版本的完整检查已经通过：Linux 747 通过／9 ignored、macOS 741 通过／
13 ignored，均无失败；两端 Clippy 通过，Linux fmt 通过。见
`checks/linux-tests-10.log`、`checks/rust-tests-11.log`、
`checks/linux-clippy-10.log`、`checks/rust-clippy-11.log`。
启动修复版的 `startup-fix-smoke-01` 已完成 16/16，通过两台主机各自 Direct／Relay
冷连接及 Linux ↔ Android 双向并发接管。16 项原始证明和实际路径的离线审计无不一致，
没有 blocked、skipped 或运行间事件。随后启动完整一小时 `soak-final-06`。

验收器复核还发现正常退出的断言缺口：原 CLI 清理在 detach 超时后发送 SIGTERM，
却未使正常退出场景失败；非零退出也没有单独检查。隔离回归复现了两种错误接受，
见 `checks/normal-detach-before.log`。现在保留清理，但正常 detach 必须无需强制终止且
返回 0，退出码和清理方式写入单项 metrics。14 项协调器回归通过，见
`checks/coordinator-31.log`。此变化应用于随后核心／扩展矩阵；已经运行的
`soak-final-06` 保持其固定测试器源码，不能单独用它证明每次 CLI 都是正常退出。
产品源码和三端制品摘要未改变。
此外，运行器现在固定每个稳定性操作的 metrics 快照，把失败后显式接续的观测放入
`continuation.metrics`。旧运行的失败 metrics 可能包含随后显式接续的路径和耗时；
失败时的 Android 快照、PTY、主机日志和时间线仍独立保留，不以接续成功覆盖失败。
当前协调器与源码规则检查见 `checks/coordinator-33.log`、`checks/source-policy-12.log`。

NOQ 修复前制品的 `soak-final-06` 已完整运行 **3609.3 秒**，固定种子 `20261003`：
171 个操作中 **168 通过、3 失败**，整轮判为失败。A 半程为 85 通过／3 失败，
B 半程为 83 通过。失败分别为 Android 进程恢复时首次协议握手耗尽原有 5 秒期限，
以及 Linux、Android 各一次超过 120 秒仍未报告断连。后两项中主机已释放控制权，
前端仍保持 Active；不能因原 Session 保留而判为恢复成功。Linux 那次失败后的
首次显式接续又返回 `transport_unavailable`，这 1 条辅助接续失败另行保留，随后
只接续原 ID，未重发不确定变更。首次失败分析及关键截图位于
`soak-A/failed-action-0006`、`-0066`、`-0072`；辅助失败位于 `connection-failure-0001`。

离线审计无不一致，A/B 原 Session ID 和 shell 基线均保留：A 为
`55a248aef34c37e7dc9b18be9ca37049`，B 为 `b13ca8531b361d6ee7c324cec2cbad6a`。
409 条执行记录核对通过，后台计数分别从 1 推进到 14090 和 7049。
成功样本中，37 次长断网检测为 32.0–67.9 秒，74 次长短断网恢复为 0.16–2.02 秒，
33 次接管为 0.47–8.63 秒；失败样本没有混入成功耗时统计。
这证明本轮完成了一小时和原会话连续性检查，不能将整轮改记为通过。
核心十轮 `core-final-04` 共 1460 行，在发现单向断网覆盖缺口后正常中断：
32 通过、3 失败、1425 未执行（skipped），离线审计无不一致。失败为一次旧 Linux
控制端超过 120 秒未检测断网，以及两次初次连接返回 `operation_outcome_unknown`；
两次均只读查询主机会话列表（为空），没有自动重发。运行器摘要固定为
`beab9b5880bc375af5f76eef4358135e72c9109e3c13dd8836de1a2e874dac3b`。
14:56:46 UTC 已确认两个 VM 与模拟器停止、故障和身份恢复日志为空，然后启动环境进行
双向断网修订后的复测。

新的 `offline` 保留 UID 出站限制，并从内核 socket 表提取测试 UID 当前持有的
TCP/UDP 端口，双栈阻断相应入站包；回环、SSH 和 adb 管理流量保留。IPv6 通配 socket
可能接收 IPv4 映射包，因此两个地址族均覆盖这些端口。只持久化测试端口，不保留其他
进程 socket 信息。规则先写恢复日志，正常退出或协调器被杀后由独立 EOF guard 清理。
这是因为 [iptables owner 匹配仅支持出站链](https://man7.org/linux/man-pages/man8/iptables-extensions.8.html)，
入站端口的 UID 依据 [内核 socket 表字段](https://kernel.org/doc/html/v5.12/networking/proc_net_tcp.html)。
每项故障分别保存入站与出站计数，任一方向未观察到丢包不得计为通过。
端口范围、双栈中途失败、异常清理、guard 恢复及双向计数回归共 18 项通过
（`checks/coordinator-34.log`）。旧测试器不满足新增回归的证据保留在
`checks/bidirectional-fault-before.log`。产品制品和产品超时均未因此改变。

双向断网冒烟 `bidirectional-short-01` 为 **8/8 通过**，覆盖两台主机、Linux/Android
控制端及 Direct/Relay。每项均观察到入站和出站丢包，恢复可输入为 0.157–0.783 秒。
`bidirectional-recovery-01` 完成 **40 项：25 通过、15 失败**，无 blocked/skipped，
审计无不一致。15 项均未进入断网断言：13 项首次连接失败，2 项 daemon 重启准备失败。
准备失败是停止阶段达到原有 5 秒期限；与观察锁启动竞争不是同一错误，仍待回归确认。

### NOQ 连接空闲计时缺陷

对依赖的隔离最小复现确认：完全停止收包后，每 20 秒发起一个新路径探测，
`noq-proto 1.3.0` 仍可在 130 秒后保持连接存活；对端已经关闭，接收包计数没有增加。
原因是每条新路径都有独立的 `permit_idle_reset`，首个探测包又重置了整连接的计时器。
这证明传输层缺陷存在，与现场长时间保持 Active 的现象一致；不能仅凭此断定每次
现场超时都由该原因引起。

修复将整连接的发送侧重置许可与每路径许可分开，遵循
[RFC 9000 §10.1](https://www.rfc-editor.org/rfc/rfc9000.html#section-10.1)：
收到并认证对端包后，整连接允许一次发送侧重置。新建未获响应的路径不能反复续期。
原协商空闲时长、路径时长、心跳间隔、连接期限均不变。最小回归从失败转为通过，
并断言原 30 秒空闲设置下不迟于 40 秒关闭；完整上游单元测试 428/428 通过。
证据见 `checks/noq-idle-130s-before-2.log`、`checks/noq-idle-130s-after.log`、
`checks/noq-vendored-suite-01.log`。更早的 40 秒复现和首次诊断测试编译失败也保留。

为保证 Linux 与 Android 使用同一补丁，源码固定在
[`vendor/noq-proto`](../../vendor/noq-proto/VOYTTY.md)，保留上游许可证、版本与来源摘要；
Cargo patch 选择该源码，Android Gradle 也将 vendor 源码纳入构建输入。
除测试外仅修改连接状态机的两个文件。`--offline-scope egress` 可复现旧单向丢包条件，
它独立记入 manifest，不能代替默认双向矩阵。最终流水线依次执行补丁冒烟、三次重复的
单向丢包回归、一小时双向故障稳定性、1460 行核心矩阵及 88 行扩展矩阵。
完整 NOQ 回归已加入 `just ci-noq` 和常规 Unix CI。协调器审计进一步逐项核对原始
防火墙计数，伪报双向丢包不能通过；19 项协调器回归见 `checks/coordinator-39.log`。

随后复核发现接续场景给客户端检测与主机释放各自分配了 120 秒，可能累计超限。
测试器现从故障开始共用一个绝对期限，同时记录主机释放、接管和旧端退休耗时；
慢查询在期限之后才返回成功也不能通过。两个最小回归在修复前失败，修复后通过；
包含审计边界的 22 项回归通过，见 `checks/shared-deadline-before.log`、
`checks/coordinator-42.log`。后续运行在 manifest 标记
`absolute-offline-deadlines-v1`；已经启动的 `noq-idle-smoke-01` 保留原测试器摘要，
仅作补丁冒烟证据，不代替使用新判据的最终矩阵。

随后进一步发现 Linux 冷启动夹具先 `daemon restart` 并等 Network Online，再启动
CLI，漏掉了 CLI 自动启动 daemon 时的连接时序。新增回归在旧夹具的 `cold` 和
`first-existing` 两个分支均失败；现改为先 `daemon stop`，直接交给真实 `connect`
启动 daemon，不再预先等网络就绪。`connect_seconds` 包含这段启动时间，停止旧
daemon 的准备时间单列。新 manifest 标记 `cli-autostart-daemon-v1`，审计检查对应
启动模式；24 项协调器／审计回归通过，见 `checks/cold-cli-bootstrap-before.log`、
`checks/coordinator-44.log`。`soak-final-07` 未使用这两个场景，保留其已固定的
运行器源码；完整核心矩阵使用修正后的夹具。产品源码和安装制品没有因此改变。

补丁后的单向长断网回归按两台主机、Linux / Android、Direct / Relay 各重复三次。
`noq-egress-regression-01` 的 A / Direct 为 4/6，A / Relay 为 6/6，B / Direct 为 6/6，
B / Relay 为 5/6。通过项的检测中位数 50.460 秒、最大 56.043 秒，恢复可输入中位数
4.969 秒、最大 18.544 秒；原 Session、shell、后台计数与 42 条命令执行记录均核对通过。
三项失败仍按原阶段保留：`0008`、`0009` 首次连接超时，`0014` 控制端 daemon 在
原有 5 秒期限未完成退出。该退出事件已关闭 primary，但现有日志未标明最终等待点，
根因未确定；不能把它归因于断网检测缺陷，也不能以采证时随后启动成功改记通过。
完整结果与分组耗时见该运行的 `acceptance-audit.json`。

该冒烟中的 A / Relay / Android → Linux 三项接续仍在首次终端快照的 5 秒期限失败。
被动抓包显示失败前后 TCP 载荷仍持续到达。A 上的独立传输探针使用临时身份、禁用
IP 传输，不读取产品账户或会话；两次各 128 KiB 的传输分别耗时 16.805 和 46.774 秒，
第一轮在 5.022 秒仅收到 24,214 字节。见 `checks/public-relay-probe-01/`。
这证明同环境公共 Relay 传输偏慢，不能据此认定每项产品超时都由同一因素造成。
[上游说明](https://www.iroh.computer/blog/shared-relays) 公共 Relay 按总体未认证负载
限速，但本探针没有收到明确的限速通知；这里不把慢传输直接判定为服务端限速。
仍使用 `official-n0`、原有产品期限和失败判定，未以替换 Relay 或预热请求覆盖结果。

加入上游源码后，原密钥扫描将计算得到的协议 `token` 变量误报为凭据。
扫描器继续覆盖 vendor 和已知凭据格式，并区分源码中的字符串字面量赋值与运行时表达式；
配置、shell 中的未加引号赋值仍检查。回归包含 vendor 内真实格式的拒绝验证，
仓库扫描与 shellcheck 通过，见 `checks/noq-vendor-secret-*.log`。

新制品的 Linux 全量测试首次重建在链接阶段遇到 VM `No space left on device`，
未形成测试通过结果；`checks/linux-tests-11.log` 保留原错误。流水线正常停止三设备，
确认无故障规则后，清理 65 个 16:03:50 UTC 前的旧测试可执行缓存，释放 16.46 GiB，
保留源码、安装制品和验收证据。清单见 `checks/linux-old-test-cache-prune-02.json`。
实例重新启动后由 `linux-tests-12` 和 `final-validation-v12` 完整重跑。

`partial-lookup-smoke-01` 的临时启动器错误地解析虚拟环境 Python 符号链接，Linux
PTY 因缺少 `pyte` 失败，随后主动中断；该轮不作为最终验收。A 的旧测试可执行文件
缓存占用过大，独立 Linux 编译在磁盘耗尽前停止，仅清理 12:00 UTC 前的构建缓存，
释放约 16 GiB，源码、已安装制品和原始证据均保留。随后 Linux 全量检查已完整重跑通过；清理记录为
`checks/linux-old-executable-cache-prune.json`。

完整复跑 `extended-final-03` 为 **68/88 通过、20 失败**，无 blocked 或 skipped：

| 主机 / 要求路径 | 通过 | 失败 |
| --- | ---: | ---: |
| A / Direct | 19 | 3 |
| A / Relay | 8 | 14 |
| B / Direct | 22 | 0 |
| B / Relay | 19 | 3 |

失败阶段为连接 14 项、场景操作 5 项、准备 1 项。68 个通过项已逐项离线核对命令
nonce、shell 基线、后台计数及实际路径，未发现审计不一致或首次错误后继续选择的
掩盖行为，见 `extended-final-03/acceptance-audit.json`。准备失败包含 daemon 停止时
网络关闭耗尽原有 5 秒期限；没有修改该产品期限。

20 项失败中另有一项明确的环境管理阻塞：模拟器重启后 `adb root` 返回连接关闭。
随后测试器改为在有界窗口内验证实际 UID 就绪；主动停止的 instrumentation 也不再
误报为意外崩溃。原报告中的 1 条 between-cases incident 正是这一夹具问题，详见
该行 `analysis.json`，没有前台服务崩溃证据。定向复测 `android-reboot-final-01`
为 1/4 通过、3 失败，无同类管理通道失败；其失败分别为 daemon 停止超时、首次
连接超时和 Android 恢复超时，不能计为重启场景通过。

## 已完成的独立检查

| 检查 | 实测结果 | 证据 |
| --- | --- | --- |
| macOS Rust workspace（已安装制品源码 `83a65`） | 746 通过，13 ignored，0 失败 | `checks/first-route-repro/workspace-01-summary.json`、`checks/first-route-repro/promotion.json` |
| Linux ARM64 Rust workspace（已安装制品源码 `83a65`） | 752 通过，9 ignored，0 失败 | `checks/linux-tests-16-summary.json` |
| Rust fmt / Clippy（已安装制品源码 `83a65`） | 两端全目标 Clippy、fmt 通过 | `checks/first-route-root-clippy.log`、`checks/first-route-root-fmt.log`、`checks/linux-clippy-16.log`、`checks/linux-fmt-16.log` |
| macOS Rust workspace（待安装候选） | 758 通过，13 ignored，0 失败；Clippy / fmt 通过 | `checks/detach-release-repro/workspace-02-summary.json`、`checks/detach-release-repro/clippy-02.log`、`checks/detach-release-repro/fmt-02.log` |
| 测试器与源码规则 | 安装阶段固定运行器 28 项通过；后续本地运行器 45 项通过，源码规则通过；旧核心运行未替换 | `checks/coordinator-49.log`、`checks/coordinator-64.log`、`checks/source-policy-42.log` |
| Welcome 延迟回归 | 真实双 daemon，回环延迟 60 ms，10/10 通过 | `checks/handshake-delay-5.log` |
| Android 构建 / lint / 单元测试（已安装制品源码 `83a65`） | debug 与 instrumentation APK 构建、lint 和单元测试通过 | `checks/first-route-android-build-checks.log` |
| Android 构建 / lint / JVM（待安装候选） | 两个 APK 构建与 lint 通过，JVM 11 项通过，尚未安装 | `checks/detach-release-android-prepared.json`、`checks/detach-release-android-build-01.log` |
| Android 基础 instrumentation | 当前 APK：41 通过，4 条件跳过，0 失败 | `checks/first-route-android-runtime.json` |
| Android 前台服务停启压力测试 | 中间制品：允许／拒绝通知各两轮，4/4 通过，但未覆盖实际崩溃条件 | `checks/android-service-race-after.json` |
| Android 待处理前台启动回归 | 当前 APK：允许／拒绝各两项，4/4 通过 | `checks/first-route-android-runtime.json` |
| Android 广泛 instrumentation | 早期制品：55 通过，3 失败，33 跳过 | `checks/android-instrumentation-4.log` |
| Android 指定测试主机复测 | 早期制品：1 通过，2 失败 | `checks/android-explicit-host-1.log` |
| 环境预检 | 当前 Linux 与实际安装 APK 摘要匹配、adb root、六方向 UDP 通过 | `checks/preflight-18.log`、`environment/preflight.json` |

Rust 数量是标准测试 harness 的汇总，不把自定义门禁输出和显式跳过算作通过。
Linux 附加的 attachment resync、terminal drain、PTY lifecycle 门禁通过；terminal
black-box 门禁明确跳过。Android 基础组的 4 个跳过仍保留为跳过。广泛测试中的两个
默认 `my-mac` 主机查找失败，经指定 `multi-device-A` 后一项通过，另一项在触摸注入时
失败；`NativeTerminalTest` 的冷连接超时仍复现。adb 返回码为 0 不等于 JUnit 全部通过。

前一轮 `1.0.3` 依赖制品的 Linux SHA-256 为
`c923a9f3a2fbc0478ca06a54339c16681d3d4c34fd844d1d51793d0d33382172`，
debug APK 为 `816ffd7192523bec4803f6d86fc5d22b8202525701a173a7a35b48bae189d290`，
instrumentation APK 为 `6cf99a747b2541133981d29ecbb90be6a6116ccaea10d2ea522737b04524ab31`。
源码归档 `environment/source-04d9c0eaf6cbcd40.tar.gz` 记录对应产品代码。
后续测试器改动由各 run 的 `runner-source.py` 和 manifest 中独立摘要固定，旧制品与旧结果未覆盖。

`1.3.0` 候选已安装并通过六方向 UDP、root 和制品摘要预检：Linux SHA-256 为
`41fc385abbb320108b25e0e1b635b7340470fb9d349f14a3eefd5b8acc9dfe8d`，debug APK 为
`83e0f460a8d30d9d03d435b09cc1edfc7829d1de9a4601a739281163f59ec44e`，instrumentation 为
`42913fa4cf7c9b3fc2e5740aaa14315883822aeef57ea6bebffcd7a3727cb509`。
对应源码归档为 `environment/source-9e4beac6db1242bc.tar.gz`，安装记录见
`checks/iroh-1.3-installed.json`。
该 APK 的基础 instrumentation 为 41 通过／4 条件跳过，前台服务回归在通知权限
允许／拒绝下各两项，合计 4/4 通过，见 `checks/iroh-1.3-android-runtime.json`。

独立的双 NAT / 受控 QAD 网络门禁在 A 内的隔离 namespace 执行，见
`checks/network-gate-final.log`。最初因夹具固定将 NAT 出口设为 `eth0` 而失败；本机
真实默认出口为 `lima0`。新增 `VOYTTY_GATE_EGRESS_INTERFACE`，保持 Docker 默认值，
修正后 A/B/C/D 四个网络门禁全部通过：官方发现保持 Relay、注入候选地址转 Direct、
双端阻断 UDP 使用 WSS/TCP Relay、受控 QAD 转 Direct。最终判定仍是
`GO_WITH_DEFERRED_ADDRESS_DISCOVERY`。这些字母是网络门禁用例名，不是三台设备；
结果不能代替真实跨物理 NAT 的官方地址发现验收。

NOQ 修复后同样的四项受控网络门禁再次通过，见
`checks/noq-idle-network-gate.log`；执行后 IPv4 forwarding 恢复为 0，namespace
清单前后均为空，见 `checks/noq-idle-network-gate-cleanup.json`。旧脚本输出中的
Colima / TUN 字样不是本轮 VZ 环境失败原因的证据。

## 验收状态

执行中。最终矩阵、十次重复、60 分钟稳定性结果和停止检查完成后在此记录。
真实跨物理网络、真实手机蜂窝切换、自建 Relay 部署和 Linux x64 不属于本轮虚拟环境
结论，已有受控 QAD / 双 NAT 检查单列为补充证据。
