# A / B / C 验收问题与解决方案清单

记录时间：2026-10-10（上海时间）。基线：`main` / `0.1.36` / `24654dc5da563877f2059f23e9c71ab26e49def7`。

**验收尚未通过。** 本文按问题原因合并重复出现的失败，记录截至本次整理已经发现的问题、
实际完成的修改、验证证据和剩余工作。各轮逐项失败仍保存在原运行目录，未由后续成功覆盖。
此前整理时暂停了产品修复及设备实验；2026-10-05 根据“继续修复”恢复工作，已并入 P25、P26。
2026-10-08 已正式实现 P27／P28，随后继续 R05，增加 P29 的关闭取消／并发所有权修正。
继续 R02 时新增 P30：Relay 拨号期间按实际入队时间清理过期待发包，避免误删新重传。
随后补充 D05 无内容传输诊断，捕获公共 Relay TCP 已连接、TLS 等待约五秒后拨号
失败的真实现场；尚未识别该拨号错误原因，不能将其等同于客户端拨号计时器超时。
该轮是定位证据。随后实现 P31：发现新路径后立即补发仍有效的 QUIC Initial，
修正等待下一次重传才开始新 Relay 拨号的延迟；R02 的其他公网超时仍需独立归因。
P27／P28 制品完整扩展矩阵为 72 通过／16 失败；P29、P30 的验证单列，历史结果不迁移到新制品。

“已修正”指对应分支有实现和回归证据，不表示所有相似现场故障都已解释，也不表示完整三端验收通过。
“待定位”只有症状或候选解释；尚未验证的方案明确写为下一步，不能算作已完成修复。
P01–P35 已提交到 `main`（`1f4b180`）；2026-10-11 新增的 P36 在分支
`investigate/r02-stale-relay-route`，尚未提交或发布。

原始证据根目录为 `target/verification/multi-device/`；下文 `checks/...` 和运行编号均相对于此目录。
2026-10-11 核对时本机该目录已不存在（`target/` 于 2026-10-10 17:08 重建，晚于 P01–P35
提交），此前各轮引用的原始证据无法在本机复查；文字记录保留，历史结论不因此改写。
此后实验根目录移到 `${XDG_STATE_HOME:-~/.local/state}/zterm-verification/multi-device/`
（可用 `ZTERM_MULTI_DEVICE_ROOT` 覆盖），新证据的 `checks/...` 相对该目录，P36 证据已移入；
重建过程见 [三端测试环境重建记录](multi-device-lab.md)。
详细时间线见 [验收报告](multi-device.md) 和 [历史过程](multi-device-history.md)，操作入口见
[运行说明](../../tests/e2e/README.md)。

## 当前续修状态

**2026-10-12 续修 R10，新增 P40：保留连接上的首个请求在窗口过半仍无确认时，后台提前建立替换连接。**
P38 的确认窗口（4×RTT，至多 2.5 秒）与其后的重新建连原先串行；旧路径 RTT 约 0.75 秒以上时，
建连、Hello 和请求接在 2.5 秒之后，超出五秒期限。现在窗口过半仍无任何 QUIC 确认时，在后台向
主机建立只到 TLS 认证、不发 Hello 的传输连接；窗口结束仍无确认时，重试让它与原有的新拨号竞争，
先连上者用于 Hello 和重试；否则关闭不用。窗口、期限、计时器、重试次数均未改，请求仍最多发两次。
本机加延迟的受控复现（路径 RTT 772–784 ms）修正前三次均 **5.00 秒 DeadlineExceeded**，修正后
三次 **4.04 秒**成功；晚到确认的存活主机不换连接、不重发。三端 A↔B 定向检查当天公共 Relay 路径
更慢（无延迟时旧路径 507 ms），旧路径 1159／1213 ms 时首个请求 **4.53／4.81 秒**成功，无延迟时
4.10 秒成功。剩余边界：窗口后仍需新路径两个 RTT，新路径 RTT 约 1.2 秒以上时仍会超时，见
[P40](#p40高-rtt-时-p38-的窗口与重新建连串行超出期限)。

### 上轮 R08／P39（保留历史）

**2026-10-12 续修 R08，新增 P39：诊断 flush 不再持写入锁做 fsync，R08 关闭。**
R08 的写入失败已定位：`Store::flush` 持 `writer.lock` 对日志文件和目录做 fsync（macOS 为
`F_FULLFSYNC`），磁盘有负载时一次要数百毫秒；该锁是 5 ms 轮询的非阻塞锁，刚释放的进程又能
立即拿到，其他写入进程等满一秒 `LOCK_WAIT` 后放弃，记录丢失，下一次 flush 返回 false。8 个
用例副本并行互为负载时，原用例 120 次失败 85 次，写入错误全部是等锁 `WouldBlock`。修正后锁内
只打开文件句柄，释放后再同步：同一负载 **120/120**、12 个副本 **120/120** 通过，最慢 append
261 ms。锁等待期限和测试断言未改。updater 交接前的持久记录屏障用的是同一 flush，失败会使更新报错，
见 [P39](#p39诊断-flush-持写入锁做-fsync其他写入进程等锁超时)。

### 上轮 R01／P38（保留历史）

**2026-10-12 续修 R01，新增 P38：主机换 Relay 重启后，保留连接上的首个请求不再耗尽期限。**
受控复现确认了 R01 的一种机制：主机被强杀后以同一身份在另一 Relay 上恢复时，控制端保留的
旧连接的包只到旧 Relay，P27 的重置无从送达，首个列表查询 **5.003 秒 DeadlineExceeded**；
在同一 Relay 或同一 Relay 加 Direct 新端口上恢复时，P27 已能在 0.33／0.86–1.43 秒内恢复。
允许重试的首次请求若在窗口内（4×RTT，至少 1 秒）收不到任何 QUIC 确认，即按传输失败交给
既有的闲置清理和一次逐字节重试，修正后 **1.33 秒**成功；存活但慢的主机仍在原连接上回应。
期限、计时器与重试规则未改。三端 `host-daemon-crash`、`host-reboot` 各一轮 **16/16 通过**、审计零问题，
但这些行不经过 P38；A↔B daemon broker 路径的定向检查在真实 Relay 上让主机换 Relay 重启，
保留连接上的首个请求四次 **2.66–4.08 秒**成功，旧连接均在 1.00 秒窗口后清理。人为抬高旧路径
RTT 后，557 ms 时仍 3.97 秒成功；754–760 ms 时窗口达到 2.5 秒上限，重试来不及，仍在 5.01 秒
失败（与修正前结果相同，另记 R10），见 [P38](#p38主机换-relay-重启后保留连接上的首个请求耗尽五秒期限)。

### 上轮 R02／P36（保留历史）

**2026-10-11 继续 R02，新增 P36：新连接不再被同一对端旧连接的选中路径钉住。**
受控复现确认 Iroh 的一个确定性缺陷：只要同一对端还有未关闭的旧连接，新出站连接的
握手包只发往旧连接的选中路径，并跳过地址查询和 P31 的 Initial 补发。主机被强杀后在
另一 Relay 上恢复时，旧连接要等 30 秒 Relay 路径 idle 才关闭，新拨号在原十五秒预算内
必然失败。修正前两种“旧连接仍开着”的组合均 **15.0 秒 DeadlineExceeded**，修正后
**0.31–0.33 秒**成功；旧连接已关闭的对照前后均约 0.33 秒成功。期限和 QUIC／Relay 计时器未改。

这只证明该缺陷存在，**不能据此归因历史 R02 失败**：Android 控制端只在关闭自己的旧连接后
重拨，冷启动新进程也没有旧连接，因此 Android 首次列表等多数冷启动超时不在 P36 范围内。
可能受影响的是 daemon 在仍有同一对端连接时的出站拨号（例如 A／B 互为控制端的行）。
本轮 Mac 上 `just check`（含一次网络失败后重跑的 relay 静态检查）和 `just android-check` 通过，
没有运行 A／B／C 或 Linux 完整工作区；
详见 [P36](#p36新连接被同一对端旧连接的选中路径钉住) 和
[本轮交接](#2026-10-11-r02-新连接选中路径修复交接)。

### 上轮 R03 画面帧窗口修复与定向验收（保留历史）

**R03 已按用户确认的有限标准定向验收通过，关闭。** 本轮实现 P35：远程终端实时画面帧
改为有界未回执窗口（4 帧或 32 KiB），使 resize 后的快照不再排在慢 Relay 路径上未送达的
大量过期增量之后。根因由现场逐次统计和受控复现共同确认：Relay 上请求 resize 至收到快照
之间各轮中位数 41–53 帧，失败现场读了 74 帧／560 KB 仍未见快照；受控复现修正前 5 秒内
收不到快照，修正后 0.828–0.850 秒收到，五次一致。期限、计时器和快照 ACK 规则均未修改。

同源候选 `2d86a806051a724c` 按冻结计划重新验收，不沿用旧制品结果：A／B→Android 的
Direct／Relay 四种组合均至少 **10 次有效恢复全部通过**（A Relay、B Relay、A Direct 各 10/10，
B Direct 9/10 后补充 10/10；唯一失败发生在注入断网前的初连，单列 R02，不计有效样本）。
同一 B→Android Relay 会话连续 **3601.2 秒、60/60**，原会话、Shell 和后台任务连续，含 6 次
精确中断 Relay TCP 后的自动重连，恢复 1.723–13.526 秒，每次都在原六十秒内显示请求尺寸、
Active 且可输入。五轮矩阵标准审计零问题；补充审计通过，首版补充审计把注入前的 R02 初连
失败也当作恢复失败，已按冻结的矩阵契约修正，原输出保留。

Linux 工作区 **817 通过／9 忽略**，Mac 定向测试、Mac／Linux Clippy、Android
构建／lint／JVM、格式、策略和秘密扫描通过。A／B／C 于 **07:28:27 UTC／15:28:27 上海时间**
停止，独立复核无故障规则残留、协调锁可获取。更早的历史 R03 停滞未保存同类积压观测，
不逐个宣称同因；R02 和完整项目验收（R09）仍开放，没有提交或发布。
详见 [P35](#p35远程终端画面帧无界积压resize-快照排在过期输出之后) 和
[本轮交接](#2026-10-10-r03-画面帧窗口修复与定向验收交接)。

### 上轮有限验收失败（保留历史）

**上轮按约定验收后，确认当时候选仍复现 R03，不能关闭。** 同源制品
`723cb5966555da18` 的四种普通短断网组合已补齐：A→C Direct **10/10**、
A→C Relay **10/10**、B→C Direct **10/10**、B→C Relay **20/20**，共 **50/50**。
其中本轮新增 26 项，既有同源结果 24 项，三个补充运行的审计均零问题。

计划一小时的 `r03-acceptance-relay-hour-04` 在同一 B→C Relay 会话中运行
**245.298 秒**后，第 4 次恢复失败：**3 通过／1 失败／56 未执行**，没有完成一小时。
每次使用八秒内 80 KiB 的有界输出、三秒双向丢包和 resize；前 3 次恢复为
7.355–12.938 秒，首次还成功覆盖了精确中断测试 UID 的 Relay TCP 连接及自动重连。
失败的第 4 次没有额外中断 Relay socket；Android 超过原六十秒仍为 synchronizing，
保持 **101×31**，没有显示请求的 **102×32**。会话未替换，没有重复发起 attach 或 resync。

主机在处理 resize 后 **6 ms** 写出 revision 375 的快照，未记录该快照的 ACK。
客户端未记录对应完整快照接收／应用，但同一流仍有字节和解码帧进展；同期有一次
ping 超时及两次 Relay 读错误断开。观察窗口存在诊断丢失／抑制，不能将缺失的包记录
当作永久未送达，也未获得代理出口／Relay 服务端的唯一归因。这次较强输出压力下的
实际超时已经足以否定 R03 的关闭条件；P34 的独立收发公平性修复仍保留。

454 项构建输入仍匹配候选，三端哈希及首尾预检通过，该轮没有修改产品代码或期限。
最终收集为 `collection-r03-acceptance-final`；A／B／C 于 **02:38:55 UTC／10:38:55
上海时间**停止，独立复核无故障规则残留、协调锁可获取。证据在
`checks/ordered-fixes/r03-acceptance/`，详见[本轮验收交接](#2026-10-10-r03-有限验收失败交接)。

2026-10-10 用户确认按有限验收标准检查并关闭 R03：同一制品的 A／B→Android、
Direct／Relay 四种组合各至少十次有效短断网恢复；B→Android Relay 同一会话连续
一小时，反复短断网、输出、resize，并覆盖输出积压和 Relay 重连。每次仍须在原
六十秒内恢复 Active、正确显示尺寸及可用输入，原会话／Shell／后台任务连续。
初连失败单列 R02，不计有效恢复样本；恢复失败不能由后续成功覆盖。满足以上条件和
证据审计后，可将 R03 标为“定向验收通过，关闭”，保留历史根因未唯一确认的说明。
不以完整项目矩阵作为 R03 的额外关闭条件。该轮未满足上述条件，冻结计划在
`checks/ordered-fixes/r03-acceptance/plan.json`。

### 上轮 P34 实现和首次验证（保留历史）

上轮继续 R03，新增 **P34：Relay 收发队列公平调度**。旧循环总是先选待发数据，
持续可写时会让已经就绪的接收数据和 pong 一直得不到处理。现在在原关闭、控制及
存活检查之后轮流优先选择收发方向；保留队列边界、批次、FIFO、重试和全部期限。
三个无 socket 的真实 actor 回归修正前两项失败、一项取消边界通过，修正后三项全通过。
该夹具证明队列饥饿缺陷，没有重现实际 TCP／TLS 或六十秒终端停滞。

旧 `95015f6a` 基线 **12/12**；新候选 `723cb5966555da18` 的 B→C Relay **20/20**、
Direct **4/4**，恢复分别为 **1.329–8.471 秒**、**0.420–0.916 秒**。
各轮结果和制品分别留证，36 项都有完整快照接收／应用／ACK 链，三轮审计零问题。
慢而成功继续计通过，不能用公网前后通过率证明 P34 的因果关系。

**R03 的历史两次超过 60 秒的停滞仍开放。** 当时没有接收就绪／队列占用证据，
本轮也未重新复现，不能将 `r03-sync-phases-03/0003`、`/0011` 唯一归因于 P34。
十五秒／五秒请求预算、六十秒恢复期限、QUIC／Relay 计时器及终端同步状态机未修改。

454 项构建输入及运行器与归档一致，三端安装哈希和六向预检通过。Linux 工作区
**812 通过／9 忽略**、Iroh **156 通过／1 忽略**，Mac Iroh 定向 **26 通过**；
Mac／Linux Clippy、Android 构建／lint／JVM、格式／策略和秘密扫描通过。
证据在 `checks/ordered-fixes/r03-relay-backpressure/`，详见
[本轮交接](#2026-10-10-r03-relay-收发饥饿修复交接)。最终收集、预检和停止完成，
A／B／C 于 **02:10:32 UTC／10:10:32 上海时间** 停止，无实验任务或故障规则残留。
完整核心／扩展矩阵及一小时尚未执行，没有提交或发布。

上轮流进度诊断曾捕获 **48.599 秒**成功恢复，等待主要在完整快照接收之前，收到后
**2 ms** 应用并写 ACK；其四轮不同制品结果和合成 TCP／QUIC 探针仍保留在
`checks/ordered-fixes/r03-stream-progress/` 和[上轮交接](#2026-10-09-r03-流进度续查交接)。
这些观察也不能唯一解释旧六十秒失败。

上轮 P33 已修复 Android 诊断文件描述符泄漏，三个资源回归修正前全失败、修正后
通过；T32 修复测试进程退出后仍读取旧状态，运行器及主机准备 50/50。其候选
`8d47006ce307211d` 的 `r03-relay-fd-fixed-02` 连续 **40/40**、恢复 0.422–4.536 秒，
同一 Android 进程的 80 次诊断句柄采样均为零。此前 `r03-relay-cause-01/0016`
已收到 ACK 后发生 EMFILE／测试器退出，不能作为旧快照未收到的第三次复现；
全部证据保留在 `checks/ordered-fixes/r03-relay-cause/`。

上轮 P32／T31 候选 `4366eab617aed7cc` 的 Relay 为 8 通过／12 失败（十次初连
失败、两次恢复失败），Direct 为 10/10；更早两轮为 14/20 和 10/10。旧失败、
诊断额度修复与原生同步回归全部保留在 `checks/ordered-fixes/r03-sync/` 及其交接。

上轮 R02 排查沿用 `0c99f1f61292d458`，没有新增产品改动或调整期限。逐包关联将
两类旧失败进一步定位：`r02-15s-matrix-02/0020` 中客户端写出的 10 个密文数据报均
见于主机接收日志，主机写出的 16 个仅有 5 个见于客户端接收日志；`0042` 在首次
Relay 拨号至注册完成之间已经耗费 **12,644 ms**，之后 QUIC 就绪时认证只剩约
432 ms。这说明十五秒仍可能耗在 Relay 建连和数据送达等待上，但尚不能唯一定位到
出口代理、运营商或 Relay；写出／flush 不等于送达，密文指纹也不能识别应用帧内容。

同源新一轮 `r02-timeout-path-01` 冷启动／首次打开 Relay **8/8 通过**，审计零问题，
成功 UI 耗时 **3.065–10.116 秒**；约十秒的成功仍计通过。独立 Relay 拨号探针
8/8 成功，516–2969 ms。启动后曾查到 DNS 返回旧 Relay、Pkarr 返回实际新 Relay，
后续结果收敛；本轮未由此失败，不能倒推上轮稳定性准备失败同因。新 TCP 头部捕获
覆盖的是这八项成功请求，不能代替旧失败的同次捕获。证据见
`checks/ordered-fixes/r02-timeout-path/`，详细边界见文末最新交接。
451 项构建输入与原归档仍一致，A／B／C 已于 **07:20:09 UTC／15:20:09 上海时间**
停止，无实验任务或故障规则残留。此前 45/52 及一小时准备失败仍保留，R02 尚未闭环。

2026-10-09 用户明确同意调整产品超时策略：首次远程请求（地址发现、连接认证及首次
响应）共享 **15 秒总预算**；连接认证／通道就绪后，普通查询或终端操作最多 **5 秒**，
并受总截止时间约束。新策略适用于桌面 CLI 经 daemon 的转发和 Android 的 Iroh 客户端；
本地操作、已连接终端的控制操作、配对和重连循环原有期限不变。历史“不延长期限”要求
用于保留旧基线，不能覆盖本次用户授权的新启动策略。

这项调整记为 D06，不记作公网慢响应的根因修复。代码在共享客户端中区分启动与操作预算；
unary 在确认连接认证后才启动操作计时，原请求的两次允许尝试仍共用同一截止时间。
终端首次连接也在总预算内将首帧操作限制为五秒。Android 连接画面保留进度提示，新增
取消按钮，返回首页会取消尚在等待的连接任务；CLI 继续保留进度及已提交结果的取消语义。

本轮明确按用户所在的中国大陆网络环境验收：**慢但成功的连接仍计通过**，不增加
低于产品期限的速度门槛；实际超时、响应错误和重启恢复失败分别留证，也不直接等同于
客户端缺陷。UI 从打开主机到终端可用可能包含多个请求，不能拿该累计耗时冒充单次协议期限。

续验第一版 D06 的 `r02-15s-matrix-01` 为 **47 通过／5 失败**。其中三个请求在
10.001 秒提前结束，源码确认正常连接仍套用了配对的十秒拨号上限；受控回归还发现
桌面 normal Hello/Welcome 仍被独立五秒期限截断。现已补齐：Android 正常连接传递
调用者的总截止时间，daemon 单次正常连接的地址发现、QUIC、Hello/Welcome 和候选
确认共享十五秒窗口；外层各请求仍各自受原绝对截止时间约束。配对、入站首帧准入、
就绪操作和重连循环的既有外层限制不变，没有修改 QUIC／Relay 内部计时器。

上轮 R02 同源候选 `0c99f1f61292d458`、451 项输入一致。六项新增回归中，修正前四项失败、
两项边界检查通过；修正后六项全通过，覆盖十一秒传输加一秒认证、六秒认证、多个阶段
合计最多十五秒及配对仍为十秒。Linux 完整工作区 **804 通过／9 忽略**，Mac／Linux
Clippy、Android 构建／lint／JVM、格式、策略和秘密扫描通过。

`r02-15s-matrix-02` 完成 **45 通过／7 失败／0 阻塞／0 跳过**，审计零不一致。
其中冷启动／首次打开为 **14/16**；其余重启／进程场景 **31/36**，三个失败发生在
故障注入前，不能算已覆盖后续重启断言。七项失败为六次启动／重启恢复的十五秒期限
耗尽，以及一次缓存连接查询的五秒超时。一个连接阶段约 **13.9 秒成功**，该行后续
自动重启恢复失败，两个阶段分别保留。普通冷启动／首次打开的成功 UI 耗时为
1.991–7.245 秒；没有因为成功连接较慢而改判失败。

`0020` 的传输在总启动约 7,993 ms 就绪，随后认证等待 7,007 ms 耗尽总期限；
路径 RTT 估计为 4,604 ms、丢包计数为 3。`0042` 的 QUIC 到 14,569 ms 才就绪，
normal 认证仅余约 432 ms；路径 RTT 估计为 4,446 ms、丢包计数为 7。
这些是同次运行的网络等待证据，尚不能唯一归因于客户端、出口代理、运营商或公共 Relay；
两轮公网通过率也不是受控的修复因果对照。**R02 的可用性验收仍未通过。**

一小时稳定性 `r02-15s-soak-03` 在准备阶段三次打开同一原会话均失败，顺序为
Linux B／Android C／Linux B；后台均记录接近十五秒的握手超时，最后一次前台呈现为
隧道在 Opened 前关闭。原会话经只读查询仍存在，没有创建替代会话。准备耗时
51.521 秒，稳定性计时 **0 秒、0 个随机动作**，不能算一小时验证。
此前编排脚本给 soak 传入 `scenario='all'` 的参数错误发生在启动前，已单列并改用
`scenario=None`，未覆盖已完成矩阵。两轮矩阵和这次失败准备的证据审计均无不一致。

上述上一轮于 **2026-10-09 06:54:38 UTC（14:54:38 上海时间）** 停止 A／B／C；协调锁已释放，
无故障规则或实验任务残留。完整核心 1460 项、完整扩展 88 项及完整一小时仍未通过。
本轮证据和阶段分类见 `checks/ordered-fixes/r02-15s-validation/`，预算回归／制品检查
见其 `budget-fix/`；后续需继续区分可接受延迟、期限策略和已证明的实现缺陷。

### 上轮 D06 第一版：保留原验证结果

同源候选 `8dd3c970a3276c18`、451 项构建输入已通过 Linux 工作区 **798 通过／9 忽略**，
Mac／Linux Clippy、Android 构建／lint／JVM、策略检查。新增五项 Rust 回归覆盖实际
TLS／Hello 的六秒认证、桌面隧道／unary 的六秒建立、十五秒总截止时间、就绪后的五秒
操作限制及取消后不发送请求。`startup-budget-cold-02` 冷启动／首次打开 **16/16 通过**，
审计无不一致；Android 取消／状态显示 **10/10**、A／B 主机首次终端 **2/2**。
A／B／C 已于 **2026-10-09 01:53:07 UTC（09:53:07 上海时间）** 停止。证据保存在
`checks/ordered-fixes/startup-budget/`；本轮不等于完整矩阵通过或公网慢握手原因已查明。

### 上轮 P31：保留原五秒基线

最新续修先在隔离 Linux 复现客户端可控的等待，再实现 P31。新地址在 100 ms 返回，
旧代码仍到 1.018 秒才完成首次请求；修复后为 121–123 ms。真实 TLS Relay 的受控
拨号延迟为 4.1 秒，旧代码到 1.007 秒才开始新 Relay 拨号，首次请求在 5.003 秒
超时；修复后约 109 ms 开始拨号、4.371 秒完成首次请求。生产 QUIC 计时器和原五秒
请求期限均未修改。新增八项回归通过（两项真实传输、六项边界）。同源候选
`27a35c53900b71ce`、451 项输入已安装 A／B／C；Linux 工作区 **793 通过／9 忽略**，
Iroh 完整库 **151 通过／1 忽略**，Mac 无监听 Iroh 门禁 22 项通过。Mac／Linux
Clippy、Android 构建／lint／JVM、格式／策略／秘密扫描通过。

`fresh-route-cold-01` 为 **31 通过／1 失败**；`fresh-route-reboot-02` 的主机／客户端
重启为 **16/16 通过**，均无阻塞、跳过或审计不一致。Android 全新进程
NativeTerminalTest 五次通过（A／B 交替，主机仅做本地就绪准备）。公网通过率不构成
修复因果对照；因果证据来自上述固定时序夹具。早先 `fresh-route-reboot-01` 的 core
过滤排除了重启场景，实际零项，已标记为未执行并由 `-02` 补齐，不计作成功验收。

冷启动唯一失败 `0024-cold-B-A-A-direct-2` 中，A 的新进程已在 1,755 ms 向新 Relay
补发同一 Initial，早于下一轮常规重传的 3,027 ms。新 Relay TLS 在 3,476 ms 完成，
升级／认证在 4,742 ms 完成，原五秒期限仅余约 258 ms；QUIC 仍未及时就绪。
这是 P31 已生效但期限仍耗尽的样本；没有本轮 TCP 捕获，不能据阶段日志唯一归因于
链路、代理或服务端。**R02 仍有未闭环失败，不能宣称完全解决。** 当前源码校验、
验证和交接证据见 `checks/ordered-fixes/fresh-route-initial/`，完整核心 1460 项、
扩展 88 项和 3600 秒稳定性仍未执行。

上一轮继续 R02，沿用 `b91e3cd14c19a8dd`，449 项构建输入未变。带 TCP 头部与套接字
队列记录的 `relay-tls-io-01` 为 **22 通过／10 失败**，`relay-tls-io-02` 为
**29 通过／3 失败**；共 64 项，51 通过／13 失败，无阻塞或跳过，两轮审计无不一致。
其中一项失败在主机重启准备阶段，另有首次快照／控制操作超时，不能把 13 项全部称为
同一种 TLS 失败。两轮 Linux 详细窗口均在首项前显式续开。三端共六份有限 TCP 捕获
有完整结束统计，28,959 个包全部解析，内核丢包为零；没有保存报文载荷或 TLS 密钥。

`relay-tls-io-02/0009` 已在**同次失败**中对应到 Linux 进程、Relay 阶段和 TCP
套接字：TLS 在 2,004 ms 完成，HTTP 升级在 2,623 ms 完成，Relay 认证确认直到
5,224 ms 才到达。后两个阶段之间 2,601 ms 内，该客户端套接字没有新 TCP 负载到达，
抽样接收／发送队列均为空；后续数据到达后认证立即推进。它定位了客户端以外的 I/O
等待边界，尚不能区分公共链路、透明转发层和服务端处理。

只读检查确认 Mac 的 Relay 出站路径经过 `utun5`／BettboxCore。独立出站探针中，
本地 TCP 0.693 ms 就绪、1.103 ms 已写入 TLS 数据，但相关外层连接在 1,870 ms
仍为 `SYN_SENT`：D05 的 `relay_tcp_ready` 只说明本地套接字状态。
另用单个套接字绑定物理网卡 `en5`，也复现了收到 2,856 字节 TLS 数据后等待到
5,005 ms、没有待发送／未处理 BIO 数据的超时。探针使用 OpenSSL，首包大小与产品
Rustls 不同，不能补作旧 Android 失败的同次证据，也不能将 Bettbox 或公共 Relay
判为唯一原因。五秒产品请求期限、十秒 Relay 拨号上限及生产协议均未改变。

该轮只有证据与文档更正，没有新增已证明的产品行为修复，**R02 当时仍未解决**。
收集、六向预检与停止通过，A／B／C 于 **12:04:33 UTC（20:04:33 上海时间）**
停止；随后 Mac 仅出站探针也已结束，无实验任务或故障规则残留。源码校验仍为
449 项一致，没有重新构建或把旧测试执行计作本次新结果。汇总及机器交接见
`checks/ordered-fixes/relay-tls-io/final-summary-01.json`、`checks/current-handoff.json`。

上一轮 R02 阶段诊断制品同为 `b91e3cd14c19a8dd`，A／B／C 同源安装。
`relay-trace-cold-02` 完成 **32 项：30 通过、2 失败、0 阻塞、0 跳过**，审计无不一致。
前一诊断制品 `393bbf2d` 的 `relay-trace-cold-01` 为 26/32；两轮不是行为修复前后对照，
不能据通过率差异声称改善。最新制品的指定主机 NativeTerminalTest 全新进程五次均通过，
不会覆盖矩阵失败。Mac／Linux Clippy、诊断回归、Android 构建／lint／JVM 和策略检查通过；
最终 Linux 完整工作区 793 通过／9 忽略，Iroh 完整库 143 通过／1 忽略；本轮没有重跑
Mac 完整工作区。收集／六向预检／停止成功，A／B／C 于 **11:20:01 UTC（19:20:01
上海时间）** 停止，无残留故障规则、运行或排队任务。证据见本轮交接和
`checks/ordered-fixes/relay-handshake-fix/`。

`relay-trace-cold-02/0003` 的 Android 同一进程显示：Relay TCP 在 7 ms 就绪，TLS
未完成，直到 5,010 ms 时记录拨号失败；内置下一次 Relay 拨号于
5,919 ms 就绪。该过程与首次请求部分重叠，QUIC 就绪时首次请求已耗时 4,989 ms，
Hello/Welcome 仅剩约 12 ms 后超时。期间运行时计时器和队列活动继续，足以解释这次
期限耗尽；尚不能在 TLS 服务端、链路、中间设备或 TCP 重传之间作唯一归因。
另一失败 `0000` 缺少当时 Linux 详细日志，仍未归因；续开之前的旧进程日志不能补作证据。
后续源码复核更正：`relay/actor.rs` 的 `CONNECT_TIMEOUT` 是 **10 秒**，D05 的
`relay_dial_failed` 不包含错误类型；先前“原五秒 Relay 拨号期限触发”的表述不成立。
五秒首次产品请求期限与 Relay 自身拨号计时器是两回事。

临时身份的 Relay 单独拨号 16 次为 471–2,257 ms；使用真实控制端的合成首次请求
20 次通过。这些探针及 TCP 元数据补查均未复现上述 TLS 卡顿，不能据此排除现场失败。
未更换 Relay、预热冷控制端、放宽五秒期限或改变生产 QUIC／Relay 计时器。
当前完整核心 1460 项、完整扩展 88 项和 3600 秒稳定性仍未执行；R02 保持未解决。

上轮 R02 续修已合入 P30。同源候选 `9593ab6bd1f16fb4` 曾安装到 A／B／C，447 项
构建输入匹配；Mac／Linux 完整工作区为 786／792 通过，Iroh 完整库 142 通过／1 忽略，
Clippy、Android 构建／lint／JVM、格式／源码／秘密扫描通过。四项新回归通过，原两项
受控失败保留；延迟 3.3 秒的 Relay 建连下，真实 TLS 首次请求由 5.003 秒超时变为
3.414 秒完成，原五秒请求期限及生产 QUIC 计时器不变。
`cold-start-01` 完成 **32 项：27 通过、5 初连失败、0 阻塞、0 跳过**，全部 27 项
通过证据及覆盖审计无不一致；A/Direct 7/8、A/Relay 6/8、B/Direct 8/8、B/Relay 6/8。
实际进入正常 detach 的 15 项通过，另 1 项在初连失败；不能把未到达的 detach 记作验证。

另修正 T30：独立 Android 检查只启动 VM，漏启主机产品服务。上轮带日志复测及本轮
首次 Android 批次的主机状态为 Stopped，原始失败保留并单列前提错误。新增本地主机
准备工具及三项回归，完整运行器检查 48 项通过；不会预热 Android 控制端。
补齐 A/B Running/Online 并以全新 C 进程复测后，基础检查仍为 **63 通过／1 失败／
35 条件跳过**：创建会话前首次列表，在 QUIC `handshaking_transport` 等待 4,937 ms
后超时，尚未进入 Hello。此次是真实的就绪主机连接失败；权限 4 项、指定主机 UI 2 项通过。
P29 首次 Android 失败没有同等离线前提证据，不一并重分类。P30 和 T30 均不等于 R02 全部解决。

最终收集／六向预检／停止成功，447 项构建输入仍与归档匹配。A/B/C 于
**07:54:51 UTC（15:54:51 上海时间）** 停止，无故障规则、运行或排队任务。
当前完整核心 1460 项、完整扩展 88 项及 3600 秒稳定性未执行，旧制品结果不迁移。
证据为 `checks/ordered-fixes/cold-start-fix/`，汇总 `final-summary-01.json`，就绪主机
上的 Android 阶段见 `android-host-ready-failure-analysis-02.json`。

上轮 R05 续修已合入 P29。当时同源候选 `61806d256b4ec6ed` 已安装到 A／B／C，
446 项构建输入匹配；Mac 786 通过／13 忽略、Linux 792 通过／9 忽略，完整 Iroh 库
138 通过／1 忽略，两端 Clippy、Android 构建／lint／JVM、格式／源码／秘密扫描通过。
三项关闭所有权回归在修复前全部失败；新增六项关闭回归及原五项重启回归现在全部通过。
受控约 2 秒 RTT 链路正常关闭约 2 秒；屏蔽关闭回应后，原 5 秒仍报超时，确实等待在
`wait_all_draining`，基线观察到 11.565 秒才触发协议关闭计时器。没有缩短该协议等待，
也没有把取消／强制退出改记正常关闭；原 Relay 现场缺少库内／报文证据，R05 尚未全部闭环。
Android 基础设备两轮均 63 通过／1 请求超时／35 条件跳过，带日志复测定位到创建会话
前的首次列表请求。通知权限每轮 4 项通过；指定主机 UI 第一轮 2 项通过、第二轮 2 项
请求超时，全部失败保留；第二轮主机未启动的前提错误见 T30，不能宣称设备检查全通过。
`endpoint-close-01` 完成 **32 项：28 通过、4 初连失败、0 阻塞、0 跳过**，覆盖及全部
28 项通过证据审计无不一致。15 项实际进入正常 detach 的场景全部通过；另 1 项在连接
阶段失败，未验证 detach。日志按设备／进程／操作关联到 22 次 Endpoint 关闭，均有 Closed，
最长 1,768 ms，没有未完成的 ClosingEndpoint；这只是本轮样本，不是慢 Relay 的保证上界。
日志收集、最终预检和停止成功，446 项输入仍与归档一致；A／B／C 于
**06:51:28 UTC（14:51:28 上海时间）** 停止，无残留故障规则或运行／排队任务。
证据目录为 `checks/ordered-fixes/endpoint-close-fix/`，汇总为 `final-summary-01.json`，
关闭观察为 `shutdown-observations-02.json`。当前完整核心 1460 项、完整扩展 88 项及
3600 秒稳定性均未执行；下面 `740376ef` 的完整扩展结果属于上一制品。

此前已合入 P27（Endpoint 重启后的重置策略）和 P28（半关闭隧道释放）。
P27 补齐“仅改 CID”失败对照；三个生产配置的首次查询回归均由约 5 秒超时转为
14.6–16.1 ms 成功，原期限不变；另有五项密钥／CID／默认配置／非零路径回归通过。
P28 的 daemon 单元测试为 204 通过／1 忽略；最终半关闭定向五项通过。
工作区 Clippy、源码／依赖策略、秘密扫描和 45 项运行器回归通过。
新同源制品 `740376ef812fb3e2` 已安装到 A／B／C，445 项构建输入与归档一致，六向预检通过。
Mac 完整工作区 786 通过／13 忽略，Linux 792 通过／9 忽略，两端 Clippy 通过。
Linux 完整 Iroh 库 132 通过／1 忽略。Android lint／JVM 通过，基础设备测试 64 通过／
35 条件跳过，通知权限允许／拒绝下共 4 项、指定主机 UI 2 项均通过。
Linux 首次链接耗尽 VM 磁盘的日志保留；清理生成缓存、关闭测试调试符号和增量缓存后
同源完整重跑通过，安装制品未更换。
新增证据目录为 `checks/ordered-fixes/endpoint-restart-fix/`。

`restart-extended-01` 已完成全部 **88 项：72 通过、16 失败、0 阻塞、0 跳过**。
A/Direct 21/22、A/Relay 11/22、B/Direct 22/22、B/Relay 18/22；72 项通过证据及覆盖
审计无不一致。12 项失败在初连，另外 4 项发生在场景检查阶段：`0029` 接管结果不确定、
`0033` 对已结束原会话的精确重开超时、`0043`／`0073` 第二控制端在接管前连接失败。
不能将未到达的接管／后台／上传断点记作已验证。

新增阶段证据：`0018` 是新 QUIC 连接及 Hello 等待超时；`0082` 查询超时期间接收字节仍
增加 168,432，Relay RTT 从 2,656 增至 4,215 ms，不能解释为完全没有收包，也不能据此
断言服务端限速或识别收到的数据内容。`0029` 主机已提交接管，客户端未收到完整结果；
其清理中的业务流已释放，随后 `Endpoint.close()` 未在原 5 秒内结束，具体内部等待点仍待定位。
该关闭阶段位于下一行 `0030` 收集的日志，关联分析位于
`checks/ordered-fixes/endpoint-restart-fix/extended-shutdown-observation-0029.json`。

最终日志收集、六向预检和停止均成功，445 项输入再次与归档一致。A／B／C 于
**2026-10-08 03:27:10 UTC（11:27:10 上海时间）** 停止，随后核实无残留故障规则、
运行或排队任务。完整汇总为 `checks/ordered-fixes/endpoint-restart-fix/final-summary-01.json`；
逐项阶段为 `restart-extended-01/stage-analysis-01.json`。当前核心 1460 项和 3600 秒
稳定性仍未执行，R01–R09 中未闭环部分继续保留。

本次 Mac Iroh 全库运行中，用户拒绝了测试程序的传入连接；只读核查确认对应程序被
系统防火墙阻止。该轮出现网络超时后已停止，原日志和 `firewall-incident-01.json` 保留，
不能算完整库通过，也不能将这些超时直接归因于补丁。防火墙未修改；完整网络检查改在
既有隔离 Linux VM 执行，Mac 内存传输回归的通过证据单独保留。

续修前同源候选为 `65dd801dbdd7d1c9`，已含 P25、P26，安装记录为
`checks/ordered-fixes/tunnel-installed.json`。363 个构建输入与源码归档逐项一致，
六向网络预检通过。Mac 完整工作区 780 通过／13 忽略，Clippy／格式／策略检查通过；
Linux 完整工作区 786 通过／9 忽略，Clippy 通过；Android 静态及基础设备检查通过，
通知权限 4 项、指定主机 UI 2 项均通过。
`tunnel-host-outage-01` 的 **80 项全部通过，无失败／阻塞／跳过**：A 主机／Direct，
20 项断网、60 项接管，八个方向各 10 轮。覆盖和全部 80 项通过证据审计无不一致。
该定向运行结束前预检通过，363 项输入仍匹配；当时 A / B / C 于 **14:00:48 UTC** 停止。
其汇总保留于 `checks/ordered-fixes/tunnel-final-summary.json`。

随后在相同 `65dd801d` 制品上完成 `ordered-extended-02`：**72 通过、16 失败、
0 阻塞、0 跳过**，88 行全部记录，72 项通过证据及覆盖审计无不一致。
15 项在初始连接失败，1 项在 Linux 控制端 VM 重启后的重连失败；不能把初连失败
记作已执行后续 resize、生命周期或上传故障。结束前预检、日志收集和清理均通过。
A / B / C 于 **2026-10-05 15:25:38 UTC（23:25:38 上海时间）** 停止。
2026-10-06 保存进展时再次确认两台 VM 为 Stopped、指定模拟器未运行、无故障规则残留，
363 个构建输入仍与已安装制品一致，没有运行或排队的测试。
汇总见 `checks/ordered-fixes/ordered-progress-20261006.json`。

2026-10-06 的隔离证据：半关闭后前端退出的隧道泄漏已复现并修正，隔离 daemon 单元测试
204 通过／1 忽略、Clippy 通过；Endpoint 完整重启后的旧连接首次查询超时也已复现，
同时稳定重置密钥并采用 16 字节随机连接 ID 的实验在原 5 秒内成功。
当时两者尚未并入主工作区；2026-10-08 已完成正式实现 P27／P28 并安装新候选。
旧扩展矩阵已经执行但未通过，当前制品的完整 1460 项核心与 3600 秒稳定性验收仍未执行。

P25 的上一同源候选 `43826c75` 已完成 75 通过／5 失败，审计无不一致。
其中三次 Linux 终端请求重复使用同一可观察路径数为 0 的旧连接，形成 P26 的受控失败回归；
另外仍有新握手超时和旧连接结束。不能将库级修复等同于 R01／R02 全部解决。

## 续修前的制品、验证与环境

续修前安装到 A / B / C 的源码归档为 `environment/source-722ac9935acd4aff.tar.gz`。
对应安装记录是 `checks/ordered-fixes/idle-cache-installed.json`，362 个编译源文件／资源已核对。
以下摘要标识已测试制品；本文新增文档不属于该旧归档。

| 对象 | SHA-256 |
| --- | --- |
| 源码归档 | `722ac9935acd4affbb1bbf62d5b4fcbb5211a0ca16b657c48673edd1e2b5d5b6` |
| A / B 同一 Linux 二进制 | `fa53ad08a639d7aa862a3a07d8c0b1199c420dc2b399aa13ff73304637374366` |
| Android debug APK | `d57622c64ea3dc33691ae51b25657e5ba379f428bf39600cac6d4e46aeddaa77` |
| Android instrumentation APK | `47efe28d00940d9ba0b89cc88bcaeb732fb175e8d921579fbd14911dbce7064e` |
| 固定运行器 | `4d99d0305ed5c904895fb48275c0e414ce5fc286cebf50ea65eea95a14a8db84` |

| 续修前完成的验证 | 结果与边界 |
| --- | --- |
| macOS Rust 全工作区 | 776 通过、0 失败、13 忽略；全目标／全特征 Clippy、格式通过 |
| Linux ARM64 Rust 全工作区 | 782 通过、0 失败、9 忽略；全目标／全特征 Clippy 通过 |
| Android | lint／JVM 通过；通用 instrumentation 64 通过、0 失败、35 条件跳过；通知允许／拒绝下另 4 项通过；指定隔离主机的 UI 补跑 2 项通过，不将重复执行算作新的独立覆盖 |
| 运行器与策略 | 45 项运行器／审计回归通过；源码策略、秘密扫描及扫描夹具通过 |
| 续修前真实三端定向回归 | `idle-cache-host-outage-01`：80 项，76 通过、4 失败、0 阻塞、0 跳过；通过证据审计无不一致 |
| 该回归的实际覆盖 | A 承载、计划 Direct，两种主机断网方向和六向接管各 10 次；60 项接管通过，20 项主机断网行中 16 通过、4 项在故障注入前首次查询失败 |
| NOQ 两项既有补丁 | 最近独立完整库回归 429 通过；此次客户端缓存修复没有更改产品 NOQ 源码，不能将独立重置探针算入已通过结果 |

检查汇总：`checks/ordered-fixes/idle-cache-check-counts-01.json`。
当时设备于 **2026-10-05 06:26:02 UTC（14:26:02 上海时间）** 完成停止；A / B / C 环境和原始证据保留，
故障规则记录无残留，没有排队运行。这是旧交接时的状态；最新扩展矩阵与剩余验收见页首。

## 已实施的产品修正

### P01：重复连接裁决可能丢失 Welcome

反向配对时，主机已经提交授权，控制端却等不到 normal confirmation。
`finish()` 只排队 FIN，紧接着裁决并关闭重复连接可能丢弃 Welcome。
现在在原握手期限内等待发送完成；接受 FIN 确认或正常读完后的 `STOP_SENDING(0)`，错误仍失败。
代码：[connection_broker.rs](../../crates/daemon/src/connection_broker.rs)。
真实双 daemon 加 60 ms 延迟的回归从失败转为通过，后续 10/10 通过，证据 `checks/handshake-delay-5.log`。

### P02：客户端重启后的新连接可能被旧 primary 淘汰

旧连接尚未超时，候选键排序却可能保留旧端、关闭新端。
新增可选 16 字节 `ConnectionIncarnation`：同代继续确定性裁决，经过身份及授权检查的新代退役旧候选；
旧代墓碑在完整握手窗口拒绝迟到 Hello，数量有界。daemon 和 Android 都在没有可复用连接时建立新代。
代码：[connection_broker.rs](../../crates/daemon/src/connection_broker.rs)、
[iroh_controller.rs](../../crates/client/src/iroh_controller.rs)、
[transport.proto](../../proto/zterm/v2/transport.proto)。
真实 Iroh 回归覆盖旧连接仍存活、新候选排序更大和迟到旧握手；见
[two_daemon_transport.rs](../../crates/daemon/tests/two_daemon_transport.rs)。字段缺省兼容旧端，完整隔离需要双方升级。
公网重启超时仍见 R02，不能据此宣称全部解决。

### P03：旧 Unix socket 所有权记录可能误删新 socket

Linux 快速 unlink / rebind 会复用 inode 和时间戳，单靠这两者识别旧 socket 不足。
现在以 `O_PATH | O_NOFOLLOW` 描述符保持原 inode 存活，恢复／清理继续校验文件身份。
代码：[local_unix.rs](../../crates/platform/src/local_unix.rs)。128 次快速替换回归通过，
证据 `checks/socket-pin-stress.log`。

### P04：VZ / user-v2 的 UDP 批量发送不兼容

现场复现 `UDP_SEGMENT` 发送失败及初始快照停滞。daemon 已关闭 GSO，使用与 Android 一致的普通 UDP 数据报发送。
代码：[network.rs](../../crates/daemon/src/network.rs)。相同 Direct 连接／接管子集恢复为 12/12 通过。
这是兼容性取舍，没有最大吞吐测量，也没有据此解释所有 Relay 超时。

### P05：短暂只读观察锁被误判为已有 daemon

状态／诊断检查也会短暂持有 `daemon.lock`；启动只尝试一次会错误报告 `daemon_already_running`。
现在在原 5 秒预算内等待锁，仍必须获得真实独占所有权。
代码：[lifecycle.rs](../../crates/daemon/src/lifecycle.rs)、
[single_instance.rs](../../crates/daemon/tests/single_instance.rs)。
保持观察锁 100 ms 的回归修正前失败、修正后通过；`startup-fix-smoke-01` 16/16 通过。
证据 `checks/startup-observation-before.log`、`checks/startup-observation-after.log`。

### P06：接管成功响应早于所需快照

持续输出下接管改变颜色／画面状态，成功响应却可能先于新快照发出，CLI 又过早预取历史，出现 `not_synchronized`。
终端流唯一 writer 现在先发送接管所需快照，再发送成功响应；CLI 到 Active 后才预取历史。
代码：[session_wire.rs](../../crates/daemon/src/session_wire.rs)、
[CLI session.rs](../../crates/cli/src/terminal_ui/session.rs)。
固定调度回归从错误顺序转为正确顺序，同时保留 ACK 和旧端不能输入的断言；证据
`checks/takeover-snapshot-before.log`、`checks/takeover-snapshot-after.log`。

### P07：合法重复快照 ACK 被拒绝

旧 ACK 与新快照交错会补发相同 revision，客户端第二次确认该 revision 被当作 `not_synchronized`。
现在记录最后确认 revision，合法重复 ACK 不关闭附件，也不能放行更新的同步屏障；未发送的 revision 和失权附件仍拒绝。
代码：[session.rs](../../crates/daemon/src/session.rs)。
证据 `checks/overlapping-snapshot-before.log`、`checks/overlapping-snapshot-after.log`、
`checks/overlapping-snapshot-boundaries.log`。R03 的间歇同步停滞仍未定位。

### P08：旧 Android 在他人接管后持续自动重试占用

旧端每秒重试 `session_occupied`，新控制端退出后可能被旧端自动接续。
现在“其他控制端占用”终止自动恢复；仅相同授权主体、相同 resume view 等待自身旧传输释放时返回可重试传输错误。
代码：[客户端 session.rs](../../crates/client/src/session.rs)、
[主机 session.rs](../../crates/daemon/src/session.rs)。两个分支均有定向回归，旧端不得自动抢回控制。

### P09：等待操作 lease 时明确的失权通知被 EOF 覆盖

等待租约期间暂存 `TerminalLeaseLost`，随后 EOF 使客户端误报 `cancelled`。
现在立即验证并处理明确通知，迟到租约回复也不会再触发接管。
代码：[客户端 session.rs](../../crates/client/src/session.rs)。
`checks/lease-wait-repro/` 保存首次失败和 5 项通过回归；v19 定向并发接管 16/16 通过。
现场未记录帧内容，因此没有把原失败的每个内部步骤都当作直接观测事实。

### P10：Android 前台服务启动尚未送达即取消导致崩溃

快速占用结果触发 `stopService`，服务尚未完成前台提升，系统报告 `ForegroundServiceDidNotStartInTimeException`。
仅把提升移至 `onStartCommand` 的中间方案仍出现 4/20 次崩溃，已保留失败。
最终实现保留已提交的启动，每次 `onStartCommand` 先提升，再按最新状态自行停止；销毁期间的新前台请求在销毁后重新评估，
失败或系统停止不触发后台重启。
代码：[BackgroundConnection.kt](../../apps/android/app/src/main/java/io/github/leonfox28/zterm/BackgroundConnection.kt)。
同一主线程轮次“启动后立即取消”的最小复现修正前崩溃；通知允许／拒绝共 4 项回归通过。
真实 `android-takeover-final-02` 的 Linux → Android 10/10 通过、无同类崩溃；另一方向有 1 项初连失败。
证据 `checks/android-pending-start-before.log`、`checks/android-pending-start-after.json`。

### P11：地址查询超时丢弃已验证的部分结果

一个来源已经返回目标 Relay，另一个不结束；整体 future 超时导致已收到结果一起丢失。
现在保留期限内与精确目标身份匹配的有效结果，期限、候选数量上限不变，其他身份仍拒绝。
代码：[route.rs](../../crates/client/src/route.rs)。
两项回归从 1 通过／1 失败变为 2/2；证据 `checks/partial-lookup-before.log`、`checks/partial-lookup-after.log`。

### P12：已有有效路由仍等待其他发现来源结束

即使不丢结果，继续等待其他来源也可能浪费 2 秒预算。
现在首个有效目标路由立即可用，无有效结果才继续按原预算等待，保留身份校验和 fresh / cache / ticket 规则。
代码：[route.rs](../../crates/client/src/route.rs)。
500 ms 受控断言修正前失败、修正后通过，证据 `checks/first-route-repro/`。

### P13：第一个不可达路由耗尽期限，饿死可用后备路由

原串行拨号在首个不可用地址耗尽 5 秒，已验证缓存或配对票据中的可用地址没有机会。
现在将有界、精确身份匹配的 Relay 候选交给同一次 Iroh 拨号并行探测，仅缓存实际选中路由。
代码：[iroh_controller.rs](../../crates/client/src/iroh_controller.rs)、
[daemon route.rs](../../crates/daemon/src/route.rs)。
本地真实 HTTPS Relay 复现从 5.003 秒失败变为 16.8 ms；证据 `checks/ordered-fixes/route-before-02.log`、
`checks/ordered-fixes/route-after-01.log`。真实 `route-fallback-startup-01` 23/24 通过，另 1 项 QUIC 初连超时仍失败。

### P14：冷端先等发现服务，延迟已有缓存／票据拨号

无响应发现服务使可用缓存仍等待约 2.019 秒。
现在没有既有远端状态的冷端先用已验证缓存／票据拨号，同一连接并行执行签名发现；
有旧远端状态的恢复仍显式刷新路由，避免沿用旧选中路径。
代码：[iroh_controller.rs](../../crates/client/src/iroh_controller.rs)、
[connection_broker.rs](../../crates/daemon/src/connection_broker.rs)。
受控耗时约 8 ms，7 项回归通过；`cold-lookup-startup-01` 45/48 通过，3 项 Android Relay 首次握手超时未解决。
证据 `checks/ordered-fixes/cold-lookup-before-01.log`、`checks/ordered-fixes/cold-lookup-after-01.log`。
P13 / P14 的毫秒耗时是本地实验数据，不能作为公网性能结论。

### P15：初始画面编码过大，慢链路无法在原期限发送完成

空白 80×25、65×48、131×41 画面原编码分别为 33626、51615、87611 字节。
真实 SessionWireServer 在受控 4 KiB/s 链路上无法在 5 秒内完成初始快照。
新增附件级显式启用的 zlib 画面帧编码，旧客户端仍使用原格式；保留 8 MiB 解压上限，拒绝截断、尾随数据和未知编码。
代码：[compression.rs](../../crates/proto/src/compression.rs)、
[session_wire.rs](../../crates/daemon/src/session_wire.rs)、
[wire.proto](../../proto/zterm/v2/wire.proto)。
相同实验约 0.342 秒完成，并核对原 Session 和尺寸，证据 `checks/snapshot-size-probe-01/`。
这不是公共 Relay 的限速测量。

### P16：无响应的新路径反复延长整个 QUIC 连接存活

完全停止收包后每 20 秒探测新路径，旧逻辑可在 130 秒后仍保持连接，因为每条新路径的首次发送都重置整连接 idle。
现在分开整连接和每路径的发送侧续期许可；只有收到并认证对端数据后，整连接才允许一次发送侧重置。
协商期限、心跳与恢复期限不变。原 30 秒设置下的回归要求不迟于 40 秒关闭，完整库 428 项通过。
代码与来源：[NOQ 补丁说明](../../vendor/noq-proto/ZTERM.md)。
证据 `checks/noq-idle-130s-before-2.log`、`checks/noq-idle-130s-after.log`。

### P17：已放弃的慢路径仍拉长存活连接的 idle

20 秒 RTT 旧路径被放弃、2 ms 新路径仍用作活动路径后，旧路径保留的 PTO 使完全无入站数据的连接直到 127.6 秒才关闭。
现在连接 idle 只计算未放弃路径，从原有效收发时点重算，放弃路径本身不重启时钟；
关闭、密钥丢弃和路径清理仍计算所有保留路径。
立即放弃／断网 20 秒后放弃均在 30 秒关闭；慢路径未放弃的对照仍为 127.6 秒。完整库 429/429 通过。
代码：[NOQ 补丁说明](../../vendor/noq-proto/ZTERM.md)；证据 `checks/noq-abandoned-idle-repro/`。
两项库复现不能倒推每次现场失败的实际 RTT 或唯一原因。

### P18：配对可能已提交，却被外层期限／EOF 改报为普通失败

后台已报告 `pair_outcome_unknown`，CLI 却收到 `deadline_exceeded`；本地同一请求两次回复丢失也可能报取消。
现在保留“有效授权证明已经发出、可能已提交”的不确定分类，明确拒绝和写入前失败仍准确区分；
原 15 秒期限、相同操作 ID 的一次重试不变。
代码：[pairing_service.rs](../../crates/daemon/src/pairing_service.rs)、
[IPC client](../../crates/daemon/src/client/ipc.rs)。
首次失败及针对性配对／IPC 回归见 `checks/pair-deadline-repro/`。这修正错误分类，没有消除所有配对超时或响应丢失。

### P19：主动 detach 只等待本地写端结束，主机尚未释放附件

Android 已回首页，Linux 普通连接仍收到占用，主机随后才释放旧 attachment。
共享客户端现在在原 5 秒控制期限内继续读取直到主机结束流或给出明确错误；事件接收端退出时立即关闭输入并取消排队命令，
随后等待网络清理。CLI 原有 2 秒外层期限不变。
代码：[客户端 session.rs](../../crates/client/src/session.rs)。
三项定向回归通过，v19 定向跨端 detach 16/16 通过；证据 `checks/detach-release-repro/`。
实际超限和初始快照失败仍保留，见 R05。

### P20：附件父任务取消后，读写子任务仍可继续持有流

丢弃普通 JoinHandle 不会取消子任务；明确 detach 也曾只发 abort 而不等待 writer 退出。
现在以 AbortOnDropHandle 绑定读写任务所有权；正常 detach 中止并等待 writer 结束后再确认附件任务退出。
代码：[session_wire.rs](../../crates/daemon/src/session_wire.rs)。
两个独立复现修正前失败、修正后通过；证据 `checks/ordered-fixes/attachment-owner-before-02.log`、
`attachment-owner-after-01.log`、`attachment-writer-before-02.log`、`attachment-writer-after-01.log`（后面三项同目录）。

### P21：daemon 停止仍等待无关空闲 IPC 或远程隧道

Stop 响应及会话关闭已完成，但监听器仍等待未发送首帧的 IPC 等任务，拖延退出。
现在取消并回收所属 IPC 任务，生命周期所有者继续完成配对、会话、网络和 Endpoint 清理。
代码：[local_ipc.rs](../../crates/daemon/src/local_ipc.rs)、
[lifecycle.rs](../../crates/daemon/src/lifecycle.rs)。
受控回归修正前失败、修正后通过，证据 `checks/ordered-fixes/idle-stop-before-02.log`、`idle-stop-after-01.log`（同目录）。
P20 / P21 的真实 `attachment-shutdown-regression-01` 为 39/40 通过；另 1 项在进入 detach 前初连失败。
10 次完整网络关闭中处理任务回收为 0 ms，Endpoint 关闭 0–3836 ms；样本不能证明所有历史关闭超时同因。

### P22：Android 会话列表查询错误污染终端或已退出页面

已复现三个分支：附着成功后的辅助列表查询失败写入终端错误；回首页／切换后旧查询迟到污染页面；
首次进入主机的必要查询即使屏蔽 UI 更新，仍通过旧调用者传播迟到错误。
现在分离 `sessionListError`，以连接 epoch、主机和查询序号限制结果归属；退役的必要查询以取消结束。
重试只读列表；已确认成功的重命名／删除先更新已知状态，后续读取失败不改报修改失败。
当前必要查询失败和精确 Session 缺失仍报错，不新建同名替代会话。
代码：[AppRepository.kt](../../apps/android/app/src/main/java/io/github/leonfox28/zterm/AppRepository.kt)、
[TerminalScreen.kt](../../apps/android/app/src/main/java/io/github/leonfox28/zterm/TerminalScreen.kt)。
最初 3 项中 2 失败，修正后 6 项通过；新增必要查询退役复现从 6 通过／1 失败到 7/7。
两项 UI 检查通过，证据 `checks/ordered-fixes/session-refresh-before/`、
`checks/ordered-fixes/retired-initial-before-01.log`、`checks/ordered-fixes/retired-initial-after-01.log`。

### P23：请求超时后反复复用同一失效闲置连接

真实 TLS / Hello / HTTPS Relay 夹具中，QUIC 仍开放但应用不再回复；连续两次独立查询均耗尽约 5 秒，始终复用连接 1。
客户端和 daemon 现在先释放失败请求，在能够独占全部准入许可时，仅退役发生写后传输错误／超时的精确闲置连接。
其他出站流或入站处理任务仍活跃时保留连接，迟到失败不能关闭替换连接，完整业务错误响应不触发清理。
清理不新增等待、不更换操作 ID、不自行重发已发应用字节，保留不确定结果语义。
代码：[iroh_controller.rs](../../crates/client/src/iroh_controller.rs)、
[remote_unary.rs](../../crates/client/src/remote_unary.rs)、
[connection_broker.rs](../../crates/daemon/src/connection_broker.rs)、
[remote_session.rs](../../crates/daemon/src/remote_session.rs)。
两个客户端回归修正前均失败，修正后通过；另 4 项 broker 真实传输回归覆盖双向活动流、迟到失败和业务错误。
同一探针源码修正后第一次仍约 5 秒失败，下一次独立查询经新认证连接约 4.1 ms 完成。
证据 `checks/ordered-fixes/cached-query-before-01.log`、`cached-query-after-01.log`、
`idle-cache-regression-before-01.log`、`idle-cache-broker-after-01.log`（后三项同目录）。
最新三端 4 次失败均有精确闲置清理及后续新连接证据，证明避免继续复用；首次失败仍未解决，见 R01。

### P24：同步诊断事件被汇总规则合并，缺少关键阶段

原每会话每秒汇总把新增快照阶段合并，缺少日志不能据此证明某一步停滞。
现在保留明确的快照写入、接收、应用、ACK 和 resize 阶段，维持原队列、全局速率及 15 分钟详细窗口。
同时补充无终端内容的路由、TLS、Hello/Welcome、缓存复用／退役、路径数量、RTT、收发计数和关闭阶段。
代码：[diagnostics](../../crates/diagnostics/src/recorder.rs)、
[event.rs](../../crates/diagnostics/src/event.rs)。
汇总回归修正前失败、修正后 8 项通过，实际 `transport-stages-relay-01` 24/24 通过并观察到阶段。
证据 `checks/ordered-fixes/snapshot-diagnostics-before-01.log`、`snapshot-diagnostics-after-01.log`（同目录）。
这些是可观测性修正，没有把 R03 自动标为解决。

### P25：多路径重置令牌漏登记、错误路由及退役后误用

补齐 R07 探针后，原服务端确认实际 PING 属于路径 1，重建的服务端 Endpoint 返回的重置令牌
也与该报文的目标 CID 完全一致；客户端只登记路径 0，确实丢弃有效重置。
仅补登记后，原两个场景继续失败于连接层：Endpoint 把重置路由到路径 0，连接也只核对初始令牌。
另复现了 CID 轮换／路径退役后，已排队的旧令牌仍错误关闭连接。

现在在路径启用或补到 CID 时登记当前令牌，Endpoint 保留其路径归属；连接按当前活动 CID、
精确来源地址及路径退役状态重新核对。CID 帧可以从另一条路径到达，登记仍使用目标路径地址；
迁移及回退同步地址，未启用的候选 CID 不提前生效。令牌比较使用现有常量时间实现。
没有延长 idle、请求或恢复期限，也没有增加应用操作重发。

代码：[NOQ 补丁说明](../../vendor/noq-proto/ZTERM.md)、
[stateless_reset.rs](../../vendor/noq-proto/src/tests/stateless_reset.rs)。
相同七项回归在旧安装源码上 0/7，在修正后 7/7；完整库 436/436。
覆盖两种端角色、初始路径已删除、轮换、迁移、错误来源／令牌、退役队列、已知 CID 前缀和重复事件。
首轮完整库发现补丁在连接已 drained 后仍发令牌事件，已修正；原失败及 proptest 种子保留。
证据：`checks/ordered-fixes/reset-token-probe/before-04.log`、`registration-only-01.log`、
`final-baseline-01.log`、`permanent-after-04.log`、`noq-full-03.log`（后四项同目录）。
这证实了传输库缺陷；R01 / R02 的全部现场失败仍不能据此判定同因。
同源三端 `43826c75` 的 `reset-host-outage-01` 为 75 通过、5 失败，审计无不一致。
失败中一次为 Android 新握手的原 5 秒超时，三次为 Linux 重复复用无可用路径的连接，
另一次为 Linux 旧连接结束。详细证据见该运行的 `stage-analysis.json`；09:26:44 UTC
测试实例已停止，无故障规则残留。

### P26：终端隧道超时后仍反复复用未响应的闲置连接

`reset-host-outage-01/0010`、`0011`、`0014` 中，Linux B 连续复用同一候选 8，
三次可观察路径数均为 0，接收计数保持 29,325 字节。前两次耗尽原 5 秒期限，
第三次直到连接本身结束才失败。P23 只覆盖 unary 查询，前端终端超时关闭本地 socket
时，透明隧道正常返回，原候选仍被缓存。

现在记录隧道是否发出数据、是否收到远端字节；隧道结束且已发送但完全未响应时，
先释放该流，再使用原有精确闲置清理：必须仍为同一主候选，且没有并发出站流或入站处理任务。
未发送数据的取消、有任意响应（包括业务错误）、正在使用的连接均保留。
适配器仍不解析 Session 内容，不延长期限，不重发创建或接管，不改变未知结果分类。
这只阻止后续独立请求再次使用同一未响应闲置连接，不消除原请求的首次失败。

代码：[remote_tunnel.rs](../../crates/daemon/src/remote_tunnel.rs)、
[cache_tests.rs](../../crates/daemon/src/remote_session/cache_tests.rs)。真实本地 TLS／Hello／Welcome
回归中，使用生产前端的原 5 秒连接期限，修复前 3 项全部失败；修复后通过。
已有并发流仍可往返数据，结束并释放后才清理；主机返回明确错误、发送前取消和迟到旧候选清理均有保护检查。
证据：`checks/ordered-fixes/tunnel-retirement-probe/before-01.log`、`after-01.log`。
最终 Mac／Linux 的查询与隧道缓存回归均为 9 项通过，包含发送前取消保护；
完整工作区分别 780／786 项通过。三端 `tunnel-host-outage-01` 为 80/80，审计通过。
已经收到部分响应后再停滞的情况不在此修复范围；路径数只作诊断，清理不依赖路径快照。

### P27：完整 Endpoint 重启改变重置密钥与 CID 校验策略

完整重启时，旧客户端仍认为缓存连接存活，但新 Endpoint 无法返回可识别的重置。
补齐“仅改 CID”对照后，原配置、仅稳定密钥、仅随机 CID 均首次查询超时，必须同时处理两处。

固定 `iroh` 1.3.0 到 [vendor/iroh](../../vendor/iroh/ZTERM.md)，增加显式启用的
`persistent_stateless_reset`；默认行为不变。启用后从秘密身份通过独立 BLAKE3 域派生稳定
重置密钥，使用 16 字节随机 CID，身份轮换也轮换重置密钥。daemon 的两个基础设施配置和
移动端配置均启用；前者由 daemon 锁和单 supervisor 持有 Endpoint，后者由 Application／
NativeRuntime 的 OnceCell 持有；必须完整关闭旧 Endpoint 后才能创建同身份替代实例。
不支持同身份并发副本。TLS ticket／地址验证密钥、协议期限和应用重试规则不变。

三个生产配置回归使用真实 QUIC／TLS、Hello／Welcome 和列表请求，保留旧缓存开放状态，
完整重建主机 Endpoint，要求第一次查询在原五秒内成功，仅有一次新认证连接及一次实际查询。
`profiles-before-01.log`／`daemon-profiles-before-01.log` 三项全部失败，
`profiles-after-01.log` 三项全部通过。另五项永久回归覆盖默认关闭、身份／用途隔离、
随机密钥、两代生成器 8192 个 CID，以及初始路径已退役后的非零路径真实重置。
`just ci-iroh` 纳入检查入口；证据均在 `checks/ordered-fixes/endpoint-restart-fix/`。
旧随机密钥签发的连接不能因单端升级自动恢复；全部历史 R01／R02 尚未逐项归因。

### P28：半关闭后前端完全退出，静默隧道无法释放

将此前隔离候选合入 [remote_tunnel.rs](../../crates/daemon/src/remote_tunnel.rs) 及永久回归。
本地读端 EOF 后，在原有一秒路径观察周期写入路径状态，以写入错误发现前端完全退出；
合法半关闭继续允许迟到响应。隧道退出后由 P26 原有精确闲置检查退役无响应连接，
不解析 Session 内容、不增加期限、不重发操作。

原 Unix／真实 TLS 失败证据保留于 `checks/ordered-fixes/half-close-probe/`。
合入后的 `endpoint-restart-fix/daemon-lib-01.log` 为 204 通过／1 忽略，
`half-close-final-01.log` 五项通过；同时确认子任务没有以 panic 冒充完成。
不能据此将所有历史 detach／daemon 停止超时归为同因。同源平台检查通过，完整扩展
72/88；本轮另有业务流已释放、等待 Endpoint 关闭超过期限的证据，尚未修正该分支。

### P29：取消或并发关闭 Endpoint 时提前返回，最后一个所有者遗漏任务取消

Iroh 原实现将 `is_closing()` 与 `is_closed()` 一同用于提前返回。第一个关闭 future 被
调用方期限取消后，再次 `close()` 直接返回，但 `is_closed()` 仍为 false；第二个并发调用
也可在第一个尚未完成时返回。最后一个 Endpoint 释放时，`abort()` 同样因 closing 状态
提前返回，遗漏剩余 actor／运行时任务的取消。这是独立于网络耗时的确定缺陷。

[socket.rs](../../vendor/iroh/src/socket.rs) 现在通过持有 actor 句柄的异步互斥锁串行化关闭。
取消等待会释放锁并保留句柄，下一次调用继续已有关闭过程；actor 完成或被取消后须 join，
最后一个所有者即使已开始关闭，也会取消剩余任务。公共关闭接口保持不变，补充了
[取消／并发契约](../../vendor/iroh/src/endpoint.rs)；没有增加 daemon 的五秒期限、跳过
`wait_all_draining`、更改协议关闭计时器或把强制退出当正常完成。

[永久回归](../../vendor/iroh/src/socket/close_tests.rs) 使用真实 QUIC／TLS 和纯内存传输，
不需要 Mac 接收入站网络连接。三项原始所有权检查修复前全部失败；最终六项通过，覆盖
取消后重入、并发等待、最后所有者取消任务并释放其资源、actor 等待被取消时保留句柄、
慢链路关闭码送达，以及无回应时继续如实报告原五秒超时。与原五项重启回归合计 11 项。

另做了严格区分的慢链路对照：约 2 秒 RTT 下，关闭回应可达时约 2 秒正常结束；屏蔽回应
后，五秒时 `wait_all_draining` 仍 Pending，尚未开始 actor 退出，基线协议关闭计时器于
11.565 秒触发。该对照解释了一个可复现的超时机制，但原 `restart-extended-01/0029`
没有库内或报文证据，不能据此认定它必然是丢失回应，更不能把全部 R05 标成已解决。

证据位于 `checks/ordered-fixes/endpoint-close-fix/`：`ownership-before-01.log`、
`slow-peer-before-01.log`、`slow-peer-no-reply-before-01.log`、`iroh-focused-03.log`、
`reproduction-summary-01.json`。完整 Mac／Linux 工作区分别 786／792 项通过；Linux
完整 Iroh 库 138 通过／1 忽略，Clippy／静态检查通过。源码 `61806d256b4ec6ed` 的
446 项构建输入匹配，已同源安装三端；Android 设备和定向矩阵的最终结果见页首。

### P30：Relay 拨号时整批清空仍有效的 QUIC 待发包

继续 R02 时，先将旧 Android 首次列表失败关联到同一进程的阶段日志：地址发现约 54 ms，
随后在 `handshaking_transport` 等待 4,948 ms 超时，尚未创建会话。旧定向 `0000`
安全连接耗时 4,025 ms，随后 Hello 未在整体五秒内完成；`0023`／`0030` 则已完成
Hello、在首次列表超时。这些阶段不能合并为同一种握手故障。
随后核查发现，上述旧 Android 带日志复测的主机服务实际上未启动，见 T30；其阶段
仍是原始观察，但不能继续作为“主机就绪时握手失败”的证据。补齐前提后的新失败另列。

确定的库缺陷是：Relay 尚未连通时，actor 每三秒直接清空整个发送队列，刚入队的
QUIC Initial 重传也被删除。连接稍后就绪时没有 Initial 可发，只能等指数退避后的下一次
PTO。无网络的回归确认仅等待 200 ms 的包被误删；真实 Relay／TLS 对照在原五秒请求
期限下失败，修复后约 3.414 秒完成。该夹具把 TCP 转发延迟设为 3.3 秒，并仅在夹具中
将初始 RTT 设为 300 ms，以将重传稳定放在清理之前；生产 RTT、Relay 建连期限和
请求期限均未修改。

[Relay 发送端](../../vendor/iroh/src/socket/transports/relay.rs) 现在记录原始入队时间；
[队列处理](../../vendor/iroh/src/socket/transports/relay/actor.rs) 只清理实际过期的包，
在退避结束、发送前再次检查年龄，保留顺序及批量发送。使用原有有界通道及一个前置暂存
项，没有逐包任务或无界缓冲。三秒包寿命不变，不重放应用请求，不修改保存的 Relay 提示。
新增[四项回归](../../vendor/iroh/src/socket/transports/relay/actor/cold_start_tests.rs)，覆盖
跨清理时刻的新包、不同年龄包的顺序、退避后的过期检查，以及真实 TLS 首次请求。

证据目录为 `checks/ordered-fixes/cold-start-fix/`，包括 `queue-before-02.log`、`tls-regression-before-01.log` 和
`tls-regression-after-01.log`；完整关联说明为 `reproduction-summary-01.json`。
首个 DNS 夹具自身三秒超时，未达到目标清理点，其失败单列保留，未当作产品回归。
旧公网日志没有 Relay 队列／包级证据，受控因果成立不代表每个现场失败都由此引起。

### P31：发现新路径后仍等 QUIC 重传，延迟首次 Relay 拨号

冷端已用旧缓存地址发出 Initial，100 ms 后发现精确目标的新地址。原远端 actor 只更新
路径表，直到约一秒的下一次 PTO 才向新路径发包并启动 Relay 拨号。内存真实 QUIC
夹具首次请求耗时 1.018 秒；修复后约 123 ms。另一夹具以本地真实 TLS Relay 延迟
TCP 转发 4.1 秒，保持生产默认 RTT 和五秒请求期限，旧代码 5.003 秒超时，修复后
4.371 秒成功；新 Relay 拨号时刻从 1.007 秒提前到 109 ms。

[远端 actor](../../vendor/iroh/src/socket/remote_map/remote_state.rs) 现在在签名发现尚未
结束、尚无选中路径时，短暂保留已发送的 QUIC v1 Initial 数据报；新地址到达后只向
新增且非本机的路径扩展原有发送。重复来源、其他身份、已过期包和已选中路径均不会
触发补发。[缓存](../../vendor/iroh/src/socket/remote_map/remote_state/initial_transmits.rs)
最多 16 个 transmit／64 KiB、寿命三秒；发现结束或选中路径时清除，发送任务仍受原
16 项并发上限约束。沿用同一连接、密文和包号，不新建应用请求，不重发不确定变更。
旧路径的 QUIC 重传周期、Hello/Welcome 屏障、Relay 和产品期限不变。

新增真实传输及边界回归覆盖新地址立即可用、慢 Relay 首次请求、重复来源／错误身份、
本机地址、过期、发现结束／路径选中、数量／字节边界及分段类型检查。证据见
`checks/ordered-fixes/fresh-route-initial/` 的 `baseline-04.log`、`baseline-tls-01.log`、
`fixed-tls-01.log` 和 `regression-01.log`。最初空地址夹具及错误构建命令单独保留，
不当作产品缺陷证据。P31 证明一个客户端等待缺陷，不能替代前轮 TLS／认证无入站数据
的独立链路问题归因，也不能把所有历史 R02 超时统一解释为此原因。

### P32：逐包诊断耗尽详细日志额度，遮蔽同步阶段

`r03-sync-viewport-02` 的十项恢复均通过，但七项没有保存客户端快照接收／应用阶段。
其中 `0001` 的主机已收到 revision 55 的 ACK，Android 也已显示 101×31；同期详细
日志出现一秒内 100 条逐包事件，关键日志报告 105 条丢失或抑制。不能把缺少阶段日志
解释为没有处理快照。独立确定时钟回归证实：原全局额度被包突发占满后，后续快照
阶段被拒绝，与 P24 已修正的每会话汇总是两条不同路径。

[记录器](../../crates/diagnostics/src/recorder.rs) 现在将逐包入队、写出、接收、交付、
过期和批次 flush 限制在原每秒 100 条总额中的 80 条，为同步和连接阶段保留空间；
非逐包事件仍可使用完整总额。没有增加队列、阻塞等待或延长 15 分钟详细窗口。
这仍是有界、尽力而为的诊断，其他事件突发或存储故障仍可能丢记录。

三项新增额度回归修正前两项失败、一项边界通过，修正后全部通过；诊断库 12/12。
证据为 `checks/ordered-fixes/r03-sync/diagnostic-starvation-evidence.json`、
`rate-before-02.log` 和 `diagnostics-after-01.log`。P32 修复可观测性缺陷，
**没有修改终端同步状态机，也不代表历史 R03 停滞已修复**。
安装后 8 次 Relay 和 10 次 Direct 成功恢复均保留完整快照阶段；两次 R03 失败则
均停在客户端快照接收之前，失败窗口没有丢失／抑制报告，详见本轮交接。

### P33：Android 诊断文件描述符泄漏，耗尽进程资源

续查 R03 的 `r03-relay-cause-01/0016` 表面仍为 65×48／synchronizing，但主机
已收到 revision 60 的快照 ACK。Android 诊断记录随后停止，测试器在写状态文件时
抛出 `EMFILE`，保留的旧状态被运行器继续读取，直到 60 秒恢复超时。这次现场不能
归类为“快照未送达”：它暴露了产品资源泄漏和 T32 的失效观察问题。

根因在 [AppDiagnostics.kt](../../apps/android/app/src/main/java/io/github/leonfox28/zterm/AppDiagnostics.kt)：
通过 `Os.open` 获得的描述符被传给普通 Java 文件流，`.use` 只关闭借用流，没有释放
原始描述符。Android 的 [输入流说明](https://android.googlesource.com/platform/prebuilts/fullsdk/sources/+/refs/heads/androidx-constraintlayout-release/android-35/java/io/FileInputStream.java)
和 [输出流说明](https://android.googlesource.com/platform/prebuilts/fullsdk/sources/+/refs/heads/androidx-transition-release/android-34/java/io/FileOutputStream.java)
明确规定了这一所有权差异。日志追加前的规范化、控制读取、flush 和导出均受影响；
即使详细日志关闭，控制轮询也可能缓慢累积泄漏。

现在保留原 `O_NOFOLLOW`／权限检查，以 `ParcelFileDescriptor` 的自关闭流持有
复制后的描述符，并在 `finally` 中关闭原描述符。正常返回、规范化的提前返回、
导出失败及快照收集失败都释放句柄；存储额度、显式十五分钟窗口和网络期限不变。

三项新增 [Android 回归](../../apps/android/app/src/androidTest/java/io/github/leonfox28/zterm/DiagnosticsTest.kt)
修正前全部失败：128 轮存储操作遗留 **775** 个句柄，导出分支额外遗留 **32** 个，
收集失败额外遗留 **8** 个；修正后全部通过，完整诊断测试 **7/7**。测试只统计随机
夹具目录的描述符，避免后台网络活动影响计数；首版夹具未考虑 Android 路径别名，
误判为零，原日志保留并由 `fd-before-02.log` 更正。

证据在 `checks/ordered-fixes/r03-relay-cause/`。P33 是已证明的产品缺陷；历史
`r03-sync-phases-03/0003`、`/0011` 未保留文件描述符计数，也没有本次 `EMFILE`
异常，不能据此把两次传输送达缺口统一归因于 P33。

### P34：Relay 持续发送时饿死已经就绪的接收数据和 pong

继续 R03 的代码审查发现：已连接 Relay 的 `biased select` 总是先处理待发批次，
只有发送 future 阻塞时，内部循环才顺带处理接收。因此发送队列持续有数据、本地
TCP 又可写时，已经到达的 QUIC 数据和 pong 可以一直得不到处理。线程调度让出
不等于收发方向公平；下一次轮询仍会先选发送。

[Relay actor](../../vendor/iroh/src/socket/transports/relay/actor.rs) 现在在原关闭、
优先控制消息及存活检查之后，轮流优先选择收发方向；一侧未就绪时另一侧继续。
保留批次大小、FIFO、有界队列、发送背压时的接收、三秒过期和原计时器，没有增加
任务或重放请求，也没有调整十五秒／五秒预算、六十秒恢复期限或终端同步状态机。

三个[无 socket 回归](../../vendor/iroh/src/socket/transports/relay/actor/fairness_tests.rs)
驱动真实 actor 和类型化 Relay 消息：十批持续待发数据期间，接收数据必须推进、
匹配健康检查的 pong 必须被处理，预先取消仍必须优先。修正前前两项失败、取消项
通过；修正后全部通过。夹具还拒绝把接收改为绝对优先而饿死发送的替代方案。
这不是 TCP／TLS 或真实终端场景的复现，不能由此给历史两次六十秒失败作唯一归因。

证据在 `checks/ordered-fixes/r03-relay-backpressure/`，主要前后对照为
`fairness-before-04.log`、`fairness-after-03.log`。最初提取 I/O 夹具时的类型错误，
以及未显式触发健康检查、依赖首个定时 tick 的 pong 夹具失败，均保留，不计作
产品失败或修复后通过。旧 `95015f6a` 的本轮定向 **12/12**，恢复 1.669–5.505 秒，
全部有完整同步链；这说明本轮基线没有复现历史停滞，不能拿公网前后通过率证明 P34。
后续同源构建、设备验证和剩余边界见文末交接。

### P35：远程终端画面帧无界积压，resize 快照排在过期输出之后

`r03-acceptance-relay-hour-04/0003` 中主机 6 ms 写出 revision 375 的快照，客户端在请求
resize 后又读取 **74 帧／560,075 字节**，仍未读到该快照。新增的逐次 resize 统计只用
客户端时钟，统计请求 resize 至收到对应完整快照之间读到的帧：Relay 各轮中位数为
**41–53 帧**，普通矩阵最多 70 帧／129 KB，80 KiB 压力下 48–60 帧／375–454 KB；
Direct 中位数为 3–4 帧。慢而成功的恢复和超时的这次失败，等待都在快照之前的有序流上。

根因在主机 [session_wire.rs](../../crates/daemon/src/session_wire.rs) 的唯一 writer：只要
QUIC 发送缓冲接受，每个 PTY revision 就写一帧增量。QUIC 流量控制额度（固定 noq
默认单流窗口约 1.25 MB）远大于慢 Relay 路径的实际送达能力，写入几乎不阻塞，
原有的 latest-only 合并因此不生效；resize 后的快照只能排在全部未送达的过期增量之后。
这不是快照 ACK 状态机缺陷，也不等于 Relay 本身限速。

现在附件可显式请求 `acknowledge_surface_frames`，远程控制端（Android 和经 daemon
隧道的 CLI）在初连和重连时启用，本地附件不启用。主机为实时快照／增量编号
`frame_sequence`；已写出但未收到累计回执的帧达到 **4 帧或 32 KiB** 时，暂停由 revision
触发的可选更新，且总是允许一帧以免大帧卡死。暂停期间的输出合并进下一次最新增量，
不丢失，也不新增队列。请求响应、生命周期、接管所需快照和会话结束的最终更新不受限制。
回执 `TerminalSurfaceFramesReceived`（wire kind 326）只表示本流已读到，不是快照 ACK，
不跨流重放；超出已写范围、序号跳跃或未协商时按协议错误处理。旧主机忽略该字段且不编号，
客户端只回执带编号的帧，因此双向兼容。客户端在可取消读取之外发送回执，避免写入被中途放弃。
十五秒／五秒、六十秒期限，QUIC／Relay 计时器，快照 ACK 和重连规则均未修改。

受控复现 `resize_snapshot_is_not_queued_behind_an_undelivered_surface_backlog` 使用真实
SessionWireServer、PTY 每 20 ms 输出 24 行随机文本、zlib 帧、512 KiB 可写缓冲和
32 KiB/s 送达速率。修正前 resize 后 5 秒内又收到 21 帧（解码后 1,805,034 字节）仍无
快照；修正后 **0.828–0.850 秒**收到请求尺寸的快照，五次一致，其间 5 帧。首版夹具使用
重复文本、64 KiB/s 和 3 秒期限，同样失败，日志保留但不作为最终对照。另有三项主机回归
覆盖回执范围、窗口边界、真实会话中暂停后合并且连续编号、旧附件不编号并拒绝回执；
一项客户端驱动回归覆盖只回执带编号的帧和拒绝序号跳跃。客户端回归首版夹具的增量缺少
主屏滚动度量，被解码器正确拒绝，属于夹具错误，原日志保留。

证据在 `checks/ordered-fixes/r03-frame-window/`：`root-cause-summary.json`、
`backlog-analysis.json`、`baseline-02.log`、`fixed-01.log`、`fixed-repeat-01.log`、
`host-receipts-01.log`、`client-receipts-02.log`。

同源候选 `2d86a806051a724c` 仍是同一组 454 项构建输入，只有上述 9 个文件变化，三端安装哈希
及首尾预检通过。Linux 工作区 **817 通过／0 失败／9 忽略**，Mac 的 proto／client／daemon
库／Android Rust／CLI、socket-free daemon 集成门禁、Mac／Linux Clippy、Android
assemble／lint／JVM、格式、策略和秘密扫描通过。按冻结计划重新验收：A Relay、B Relay、
A Direct 各 **10/10**；B Direct 首轮 9/10，唯一失败在注入断网前的初连（R02），补充一轮
**10/10**。同一 B→Android Relay 会话一小时 **3601.2 秒、60/60**，含 6 次 Relay TCP 精确
中断重连，恢复 **1.723–13.526 秒**，60 次都有完整快照链。修复后 Relay 上 resize 快照前的
过期帧中位数降为 **6 帧**（最多 11 帧／17 KB），详见文末交接。

### P36：新连接被同一对端旧连接的选中路径钉住

继续 R02 时先怀疑 [fresh_relay_hints](../../crates/client/src/route.rs) 取第一个返回的
地址服务结果：DNS 曾短暂返回旧 `aps1-1`，而 Pkarr 已返回 `usw1-1`。受控对照否定了它是
主因：旧连接已关闭时，Iroh 自身的地址查询会合并全部服务结果，即使 DNS 先返回旧 Relay
也在约 0.33 秒连上。真正的失败条件是**同一对端还有未关闭的旧连接**。

Iroh 每个对端只有一个选中路径。[remote_state.rs](../../vendor/iroh/src/socket/remote_map/remote_state.rs)
原先把 mixed 地址数据报（只承载新出站连接的握手）全部只发往该路径；有选中路径时还
跳过地址查询和 P31 的 Initial 补发。主机被强杀或 VM 重启、没有 CONNECTION_CLOSE 送达时，
旧连接要等 idle（Relay 路径 30 秒）才关闭并清空选中路径。若主机换了 Relay 或端口，
期间每次新拨号都发不到主机，即使调用方已提供正确的新 Relay。上游 `main` 行为相同，
已报告为 [n0-computer/iroh#4600](https://github.com/n0-computer/iroh/issues/4600)。

现在握手数据报先发选中路径，再发其他已知的非本机路径，与上游无选中路径时一致；
新连接在没有查询进行时启动地址查询，查询新增路径时补发仍有效的 Initial；Initial 缓存
改为在新连接加入、查询结束或过期时清空。已建立连接的路径、路径选择、打洞、QUIC 计时器、
十五秒／五秒期限均未修改，也不重放应用请求。

客户端 `moved_host_` 三项回归使用两个本机 Relay：仅 Relay 的主机在独立运行时中被丢弃，
不发送 QUIC 关闭，再用同一身份在另一 Relay 上恢复；拨号沿用十五秒启动预算。修正前
“旧连接仍开着 + 正确新 Relay 提示”和“旧连接仍开着 + 旧 DNS 先返回”均 **15.0 秒
DeadlineExceeded**，修正后分别约 **0.31 秒**和 **0.33 秒**成功，连跑三次一致；旧连接
已关闭的对照前后都约 0.33 秒成功。首版夹具在第一次连接的 300 ms 查询结束前就重启主机，
旧查询仍在进行，按设计不会重复启动，这是夹具时序错误：现场主机重启需要数秒。夹具改为
先等第一次查询结束，原失败输出不计作产品行为。另有三项 `zterm_` vendor 单元回归：
新握手不只发选中路径、有选中路径仍启动查询、另一连接的选中路径不终止 Initial 补发；
修正前三项失败，修正后 `just ci-iroh` **28/28** 通过。

这些测试证明钉住缺陷存在，不证明哪些历史 R02 失败由它造成。按当前代码阅读，Android
控制端只在关闭自己的旧连接后重拨，冷启动新进程也没有旧连接；daemon 只在仍有同一对端的
primary（例如对方的入站连接）而尚未获得远端接受时才另行出站拨号。因此 Android
冷启动和首次列表超时不在 P36 范围内，需要现场证据才能判断哪些行受影响。

### P37：Relay 首次 TLS 无进展时等满十秒拨号期限

重建后的三端环境中，`lab-rebuild-cold-02` 的两行 Linux 冷启动（A←B Direct 12.293 秒、
B←A Direct 12.639 秒）各有一次所需 Relay 的拨号：TCP 由本机透明代理 1 ms 内确认，TLS 随后
十秒没有任何进展，以 `dial_deadline`（10,005／10,002 ms）结束；立即重拨约 0.47 秒连上。
这十秒占去十五秒启动预算的大部分，期间排队的 QUIC Initial 每三秒过期一批。其余拨号 TLS
0.09–2.1 秒完成。这与历史 `0042` 首次 TLS 失败后重拨 649 ms 即完成的形态一致，现在有了
类型化的失败类别。上游 `main` 同样只做一次尝试。

[Relay actor](../../vendor/iroh/src/socket/transports/relay/actor.rs) 现在在拨号 3 秒仍未连上时
对同一 Relay 并发再发起一次；先成功者胜出并丢弃另一个，一个失败则等待另一个，3 秒前的错误
照旧立即返回，慢但有进展的原尝试不会被取消。十秒拨号期限仍约束两次尝试，退避、数据报过期、
QUIC 计时器和产品十五秒／五秒期限均未修改。落败但已完成认证的尝试会短暂以同一身份注册；
固定的 iroh-relay 1.3.0 服务端保留新连接、降级旧连接，新连接断开时把旧连接恢复为 active，
客户端只记录健康通知。

五项暂停时钟的 `zterm_relay_dial_` 单元回归覆盖首次尝试停滞、快速尝试不对冲、早期错误不对冲、
慢但有进展的首次尝试仍胜出、一次失败等待另一次。真实 TLS Relay 的 socket 回归让代理接受
首个 TCP 连接但永不转发：单次尝试时首个请求 **10.09 秒**，对冲后四次 **3.15–3.16 秒**。
`just ci-iroh` 33/33，P30 的 socket 回归仍通过（3.49 秒）。证据在
`checks/ordered-fixes/hedged-relay-dial/` 及 `checks/lab-rebuild/relay-dials-cold-02.json`。
这证明了停滞拨号的等待机制及其修正，不等于历史全部 R02 超时同因；三端同源复测见文末交接。

### P38：主机换 Relay 重启后，保留连接上的首个请求耗尽五秒期限

R01 的受控复现在 [restart_tests.rs](../../crates/client/src/iroh_controller/restart_tests.rs)：
两个本机 Relay，仅 Relay 的主机在独立运行时中被丢弃、不发 QUIC 关闭，2 秒后以同一身份
分别在 ① 同一 Relay、② 另一 Relay、③ 同一 Relay 但 Direct 换新端口上恢复。控制端保留原
normal 连接，首个列表查询沿用原五秒期限。修正前 ① 约 0.33 秒、③ 0.86–1.43 秒成功
（P27 的重置经 Relay 路径送达）；② **5.003 秒 DeadlineExceeded**，新主机没有收到这次查询，
下一次独立查询约 0.33 秒经新连接成功。这证实了交接中的候选解释。

旧连接的包只发往旧 Relay；Relay 服务端对未连接的目标静默丢包，只在对端注销时向曾收到
其包的一方发 `EndpointGone`，Iroh 客户端对此只更新在线集合。新实例收不到任何包，P27 的
重置无从送达；Iroh 也不为已建立的连接重新查询地址。保活 5 秒、Relay 路径 idle 30 秒，都远超
五秒期限，而 P23 只在期限耗尽后才清理闲置连接，现有的一次逐字节重试因此没有剩余时间。

存活的主机即使服务还在处理，也会在约一个 RTT 内由 QUIC 确认请求；已经换址的旧连接则收不到
任何确认。[remote_unary.rs](../../crates/client/src/remote_unary.rs) 的单一重试所有者现在只在
“之后允许一次逐字节重试”的首次尝试上，写前采样连接收到的 ACK／PATH_ACK 帧计数：窗口为
连接最大路径 RTT 的 4 倍，至少 1 秒，至多剩余期限的一半。窗口结束时仍无回应且没有任何新
确认，才按写后传输失败处理，复用 P23 的精确闲置清理和既有重试，在原期限内经新连接完成；
有新确认则继续等回应到原期限。操作 lease 分配和第二次尝试仍等满期限。控制端和 daemon broker
（[remote_session.rs](../../crates/daemon/src/remote_session.rs)）共用同一实现。期限、QUIC／Relay
计时器、重试类别与次数、操作 ID 均未修改；文档契约见
[remote-cli.md](../remote-cli.md#ambiguous-operations)。

四项回归：①②③ 首查须在五秒内成功，新主机只接受一个新连接且下一次查询复用它；存活但回应
延迟 2.5 秒（超过 1 秒窗口）的主机须在原连接上成功，不新建连接、不重发。修正后 ② 四次
**1.326–1.336 秒**、① 0.33 秒、③ 1.19–1.33 秒，慢主机 2.505 秒。两项对照：关闭提前结束时 ② 仍 5.003 秒失败；
把 ACK 判断换成纯计时时，慢主机被换连接且请求重发（accepted=3、queries=4）而失败。daemon
测试替身记录各次尝试是否允许提前结束：列表与变更为 `[true, false]`，lease 为 `[false]`。
证据在 `checks/ordered-fixes/r01-kept-connection/`（`before-01.log`、`control-plain-timer-01.log`、
`after-01.log`–`after-03.log`）。本机 `just check` 1290 通过、`just android-check` 通过。

范围：只有连接上没有其他流时才能替换（P23 规则），终端附件仍占用同一连接时首个请求照旧
在期限后失败，附件自身的恢复不在本修正内。daemon broker 路径没有本机可运行的真实 Relay
回归（两 daemon 夹具仅 Linux 且禁用 Relay），改在三端验证。

**三端验证（2026-10-12 01:38–02:16 UTC／09:38–10:16）**：候选为 `40837c6` 加上述未提交改动
（`modified_sources` 7 个文件，源码归档 `source-11790be7b3527e9b.tar.gz`，A／B 二进制
`1de7a31a…278b28`），preflight 通过。

- 矩阵 `host-daemon-crash` ×1（`p38-host-daemon-crash-01`）8/8、`host-reboot` ×1（按控制端分为
  `p38-host-reboot-a-01`／`-b-01`／`-c-01`）8/8 通过，审计零问题，各行均无 `not acknowledged`。
  这些行不经过 P38：被重启主机的控制端有终端附件占用连接；daemon 强杀后主机仍在同一 Relay
  或地址上，保活包 3–5 秒内换回 P27 重置；VM 关机期间主机 daemon 没有记录停止事件，附件约在
  新实例启动 24 秒后（空闲超时量级）才发现。运行器每行切换路径时还会重启 daemon，前一行留下
  的空闲连接也就不会被下一行用到。因此这些行只说明 P38 没有引入回归。
- 定向检查 `kept_relay_switch.py`：只对 A 的测试账户屏蔽 UDP（同运行器 Relay 行），B 查询 A
  一次留下空闲保留连接（复用查询 160–193 ms）；然后强杀 A 的 daemon，只对 A 屏蔽其当前
  home relay `aps1-1` 的 TCP 后重启，迫使 A 在另一 Relay 上恢复。B 的下一次 `session list`
  就是保留连接上的首个请求：

  | 运行 | 首个请求发出 | 结果 | 旧连接清理 | 新连接建立 |
  | --- | --- | --- | --- | --- |
  | `relay-switch-02` | 强杀后 12.2 秒 | 4.084 秒成功 | 1.00 秒后（`retry`） | 再 2.55 秒 |
  | `relay-switch-03` | 强杀后 12.3 秒 | 2.658 秒成功 | 1.00 秒后 | 再 1.08 秒 |
  | `relay-switch-04` | A 刚 Online（4.2 秒） | 2.740 秒成功 | 1.00 秒后 | 再 1.24 秒 |
  | `relay-switch-05` | 强杀后 12.3 秒 | 2.792 秒成功 | 1.00 秒后 | 再 1.28 秒 |

  旧连接 RTT 155–185 ms，窗口取下限 1 秒；窗口内旧连接接收字节不变（只多出请求的发送字节）。
  `-02` 中新连接的首包先发往旧 Relay，0.79 秒后经地址发现得到 A 的新 Relay，再拨号 0.65 秒
  （TLS 0.40 秒）。防火墙计数确认发往 `aps1-1` 的包被丢弃。`-01` 因脚本解析地址出错，
  强杀前即中止并清理，作废；`-05` 是下述高 RTT 检查的同时段基线。三端没有未修正版本的对照，
  “修正前耗尽期限”依据本机复现 ②。
- 高 RTT 余量（02:09–02:16 UTC）：`high_rtt.py` 只对 B 测试账户的 TCP 443 打标记，在 B 的
  `lima0` 上用 netem 加出站延迟，再运行同一检查，结束时删除。netem 作用在 Relay 的 TCP 流上，
  放大并不线性（+50 ms 得 557 ms，+150／+350 ms 都约 760 ms），下表按实测 RTT 记录：

  | 运行 | 旧连接 RTT | 窗口 | 新连接建立 | 新连接 RTT | 首个请求 |
  | --- | --- | --- | --- | --- | --- |
  | `high-rtt-050-01` | 557 ms | 2.23 秒 | 再 1.14 秒 | 525 ms | 3.967 秒成功 |
  | `high-rtt-150-01` | 754 ms | 2.50 秒（上限） | 再 1.42 秒 | 638 ms | 5.015 秒 `deadline_exceeded` |
  | `high-rtt-350-01` | 760 ms | 2.50 秒（上限） | 再 1.86 秒 | 844 ms | 5.012 秒 `deadline_exceeded` |

  总耗时约为窗口、新连接建立和约一个新路径 RTT 之和。旧路径 RTT 达到 625 ms 后窗口固定为
  2.5 秒，重试只剩 2.5 秒；两次失败都是新连接建立后剩余时间（1.08／0.64 秒）不足以完成请求。
  紧接着的第二次请求均成功（3.13／2.76 秒）。失败时用户看到的结果与修正前相同，但期限耗尽后
  P23 还会清理刚建立的重试连接，下一次请求须再建一次连接。不加延迟时，A 的新 Relay 路径 RTT
  就有 490–686 ms：若旧路径 RTT 达到 557 ms 量级，再遇上 `-02` 那样需重新发现并拨号的 3.08 秒
  重试，总耗时约 5.3 秒，同样会失败。

边界：P38 在旧路径 RTT 不超过约 0.55 秒时实测有约 1 秒余量；约 0.75 秒及以上时，窗口与重试
串行相加超出五秒期限，首个请求仍然失败（R10，由 P40 处理）。证据在
`checks/ordered-fixes/r01-kept-connection/`：`build-01.log`、`run-*.log`、`audit-*.log`、
`relay-switch-01`–`-05`、`high-rtt-*`（含 A／B 日志、防火墙与 netem 计数）、
`relay-switch-summary-01.json`、`high-rtt-margin-01.txt` 及 `high-rtt-summary-01.json`。

### P39：诊断 flush 持写入锁做 fsync，其他写入进程等锁超时

R08 的用例 `cooperating_processes_rotate_complete_records_on_one_stable_lock` 由三个子进程
共用一个日志目录，各写 40 条记录、每条后 `flush(3 s)`。`Recorder::flush` 只在写入线程报告
失败时提前返回 false：上次屏障后有 append 失败，或 sink 自身 flush 出错。
[diagnostics.rs](../../crates/platform/src/diagnostics.rs) 的 append 与 `Store::flush` 都先取
`writer.lock`；取锁是非阻塞尝试加 5 ms 轮询，最多等 `LOCK_WAIT` 一秒，超时返回
`WouldBlock "diagnostic writer busy"`。`Store::flush` 原先持锁对现有日志文件和目录依次 fsync，
Rust 在 macOS 上用 `F_FULLFSYNC`。锁不排队：持有者释放后立即开始下一条记录就能再次拿到，
轮询方要正好落在空隙里才能进入。

临时探针（不提交）包装子进程的 sink，记录失败及超过 200 ms 的 append／flush。无负载单次运行
已有一个子进程 0.29 秒内完成全部 40 轮，另一个的 append 等锁 259 ms。随后用 8 个用例副本并行
互为磁盘负载（各自独立目录和锁，只共享磁盘），每个副本 15 次：120 次失败 85 次；94 个 sink
错误全是等满约 1.00 秒的 `WouldBlock`，append 66 个、`Store::flush` 自身取锁 28 个，没有其他
错误类型。成功的 flush（含等锁）中位 422 ms、p90 797 ms。2026-10-12 `just check` 中的失败发生在
本机同时运行三端 VM 和模拟器时，与这一机制吻合；那次运行没有探针，无法逐条对应。

修正：`Store::flush` 只在持锁期间打开现有日志文件，释放锁后再同步这些句柄和目录。持锁期间
不会轮转，取得的就是当时存放记录的 inode；之后的轮转只改名，同步的仍是同一份数据，归档
被下一次轮转删除属于既有保留策略。这与 `snapshots()` 锁内只取句柄的做法一致。锁等待期限、
轮询间隔、记录上限和该用例的断言均未改。修正后同一 8 副本负载 **120/120**，没有 sink 错误或
超过 200 ms 的 append，最慢 flush 218 ms；12 副本、每个 10 次 **120/120**，最慢 append 261 ms、
flush 312 ms。

产品影响：updater 记录 `AwaitingHandoff`、`Accepted` 阶段后 `flush(2 s)`
（[handoff.rs](../../crates/daemon/src/handoff.rs)），返回 false 时更新报 “Unable to record the
update outcome.”；daemon、前台进程和 updater 同时写日志时，原实现可能因磁盘负载使这一屏障
失败。剩余边界：锁仍不排队，持锁时间本身因负载超过一秒时仍会丢记录；这属于文档声明的尽力而为
日志语义，本轮没有复现。证据在
`checks/diagnostic-flush-r08/`：`probe-instrumentation.patch`、`fix.diff`、`run-probe.sh`、
`probe-01`（修正前）、`probe-02`／`probe-03`（修正后）及 `summary.json`。本机无额外负载的
`just check` 1290 通过（`just-check-01.log`）；Android 不依赖 `zterm-platform`，未运行
`just android-check`。

### P40：高 RTT 时 P38 的窗口与重新建连串行超出期限

R10 的受控复现沿用 P38 的 [restart_tests.rs](../../crates/client/src/iroh_controller/restart_tests.rs)：
两个本机 Relay，仅 Relay 的主机被丢弃、不发 QUIC 关闭，再以同一身份在另一 Relay 上恢复。新增
的是主机经一个 TCP 转发连接 Relay，转发在每个方向延迟 380 ms，相当于三端 netem 只作用于一侧的
Relay 流量；Relay 按端点 ID 转发，控制端仍直连。保留连接的路径 RTT 为 772–784 ms。修正前三次
均 **5.001–5.004 秒 DeadlineExceeded**，新主机 accepted=2：期限耗尽后 P23 又清理了刚建立的重试
连接，与三端 `high-rtt-150`／`-350` 相同。窗口达到 2.5 秒上限后才开始地址查询和 QUIC 握手，
之后的 Hello／Welcome 与请求各需约一个 RTT。

窗口不宜缩短：那会把慢但存活的主机判为断开并重发请求。修正让建连与窗口并行。窗口过半（未达
上限时为两个 RTT）仍没有任何新的 QUIC 确认时，在后台用同样的路由向主机建立传输连接，只到 TLS
认证为止、不发 Hello：控制端每次 Hello 都带新的连接代次，存活主机收到后会退役仍在处理请求的
旧连接；主机在收到 Hello 前也不登记该连接。窗口结束仍无确认、旧连接按 P38 退役时，这条替换
连接交给下一次拨号，与原有的新拨号竞争，先连上者用于 Hello 和重试；替换连接已连上时不再发起
新拨号。保留新拨号是因为 Iroh 只在连接开始时查询一次地址：替换连接若在主机发布新 Relay 前查询，
会一直发往旧 Relay，只等它会比 P38 更差。窗口内收到确认、请求得到回应或旧连接未能退役时，替换
连接不发 Hello 直接关闭；它也不晚于原请求期限被采用，主机等 Hello 的五秒期限因此不会先到。

实现在 [remote_unary.rs](../../crates/client/src/remote_unary.rs)，控制端
（[iroh_controller.rs](../../crates/client/src/iroh_controller.rs)）与 daemon broker 共用。broker
（[connection_broker.rs](../../crates/daemon/src/connection_broker.rs)）把替换连接交给拨号 worker 的
`dial_once` 采用，不另设第二个 Hello 拨号者；替换连接不向 CLI 报告进度，进度仍只来自实际拨号。
`dial_once` 中解析路由与建立传输合为一步以便竞争；路由解析始终只返回一条路由，原先“握手失败后
试下一条路由”的循环没有实际作用。窗口、五秒／十五秒期限、QUIC／Relay 计时器、重试类别与次数、
操作 ID 均未修改，请求仍最多发送两次。文档契约见
[remote-cli.md](../remote-cli.md#ambiguous-operations)。

本机回归（修正后串行各三轮，均通过）：

- 高 RTT 换 Relay：路径 RTT 778–780 ms，首个请求 **4.038–4.043 秒**成功；新主机只接受一个连接
  （即替换连接），下一次请求复用它，queries=2。
- 晚到确认的存活主机：保留连接建立后把主机侧单向延迟从 0 调到 350 ms，窗口仍为 1 秒下限，确认在
  窗口中点之后、结束之前到达，服务 2.5 秒后回应。请求 3.21 秒在原连接上成功；此时主机
  accepted=2（替换连接已建立），但只有原连接发过 Hello（greeted=1），下一次请求仍复用原连接，
  queries=3，没有重发。修正前代码 accepted=1。RTT 估计尚未跟上变慢的路径时，下一次请求也会
  建立并丢弃自己的替换连接：Linux 上其前的估计只有 347 ms，中点约 0.69 秒，主机最终 accepted=3，
  仍 greeted=1。首版用例在最后断言 accepted=2，因此 PR CI 的两个 Linux 任务失败，macOS 通过；
  改为在首个请求后计数。
- 替换连接只见旧地址：主机已在新 Relay 上恢复，替换连接发出的两次地址查询（控制端路由查询和
  Iroh 自身查询）仍返回旧 Relay，之后（0.81 秒）才改为新 Relay。首个请求 **1.33 秒**成功，由窗口
  后的新拨号胜出，与 P38 相同。对照：把竞争改为先等替换连接，同一用例 **5.003 秒
  DeadlineExceeded**。修正前代码上该用例也失败，但那里前两次查询来自窗口后的新拨号本身，不是
  可比对照。
- P38 原有四项：同一 Relay 0.33 秒；另一 Relay 1.32–1.33 → **1.01 秒**；Direct 新端口
  1.17–1.33 → **0.83–1.01 秒**；慢主机 2.50 秒，不换连接、不重发（accepted=1、queries=2）。
- broker（真实 TLS／Hello 夹具）：替换连接拨号完成后退役原 primary 并交出替换连接，下一次查询经
  拨号 worker 采用它，主机始终只有 2 个连接；对照让拨号 worker 忽略替换连接，出现第 3 个连接。

**三端验证（2026-10-12 05:46–05:51 UTC）**：候选为 `6ff3e16` 加上述未提交改动（源码归档
`source-bd3e5830c6343f88.tar.gz`，A／B 二进制 `23ea513d…b09036`），preflight 通过。沿用 P38 的
`kept_relay_switch.py`／`high_rtt.py`（主机 A 换 Relay，防火墙计数确认发往 `aps1-1` 的包被丢弃；
DNS 在 12 秒内仍返回旧 Relay，与 P38 同）。当天公共 Relay 路径明显比 P38 当天慢：

| 运行 | 旧连接 RTT | 窗口 | 替换连接 | 新连接 RTT | 首个请求 |
| --- | --- | --- | --- | --- | --- |
| `relay-switch-01`（无延迟） | 507 ms | 2.03 秒 | +1.03 秒开始，窗口后 0.52 秒连上；窗口后的新拨号未完成即被放弃 | 1133 ms | 4.102 秒成功 |
| `high-rtt-150-01` | 1159 ms | 2.50 秒 | +1.26 秒开始，窗口前 0.60 秒连上；未发起新拨号 | 648 ms | 4.529 秒成功 |
| `high-rtt-350-01` | 1213 ms | 2.50 秒 | +1.26 秒开始，窗口前 0.08 秒连上；未发起新拨号 | 1159 ms | 4.811 秒成功 |

两次高 RTT 运行在旧连接退役后 2 ms 内即在替换连接上发出 Hello，无延迟那次在替换连接连上的同一
毫秒发出；随后的 Hello／Welcome 0.65–1.37 秒，请求约 0.5–1.4 秒。日志用 `operation_id` 区分两次拨号：`relay-switch-01` 中替换连接（4）连上，窗口后
的新拨号（5）没有完成。三端没有同日的修正前对照；按各次实测的拨号时间推算，若拨号在窗口结束后
才开始，三次约为 5.1、5.2、5.9 秒，都会超时。P38 当天 754–760 ms 时的失败见 P38。

边界：窗口达到 2.5 秒上限后，仍需新路径上 Hello 和请求两个 RTT；新路径 RTT 约 1.2 秒以上时，
首个请求仍会超时（`high-rtt-350-01` 新路径 1159 ms 只余 0.19 秒）。这需要改窗口或期限，本轮不做。
终端附件占用旧连接时不替换，与 P38 相同。证据在 `checks/ordered-fixes/r10-replacement-dial/`：
本机 `before-01`–`03.log`（主线代码加新夹具）、`after-01`–`03.log`、`after-broker-01.log`、
`control-no-race-01.log`、`control-no-adopt-01.log`；三端 `build-01.log`、`relay-switch-01`、
`high-rtt-150-01`、`high-rtt-350-01`（含 A／B 日志、防火墙与 netem 计数）及所用脚本，汇总见
`summary-01.json`。本机 `just check` 1294 通过（`just-check-01.log`）、`just android-check` 通过
（`android-check-01.log`）；三端 A 虚拟机（Linux arm64）上 restart 七项三轮及 broker 十一项通过
（`linux-a-01.log`）。

## 隔离修复候选的首次证据（2026-10-06 历史）

以下保留当时尚未合入的证据与边界；2026-10-08 的正式实现见 P27／P28。

### 半关闭后前端退出，静默远端使隧道持续保留（关联 R05）

真实 Unix 前端发送 HalfClose 并关闭写端后，隧道读到 EOF，就永久停止读取本地流。
如果前端随后完全退出，而远端保持静默、路径又不变，原实现不会再写本地 socket，
因此无法发现前端消失，继续占有远端流与准入许可。P26 需要隧道先结束，无法清理这个分支。

隔离副本中的修正：本地读端已结束后，在既有的 1 秒路径观察周期发送当前路径状态，
由写入失败发现前端退出。正常半关闭仍可接收迟到响应；没有增加操作期限、解析 Session
内容或重发操作。隧道结束后继续由 P26 的精确闲置检查决定是否清理连接。

证据均位于 `checks/ordered-fixes/half-close-probe/`：

- `before-01.log`：前端退出检查失败，正常半关闭的迟到响应检查通过。
- `before-integration-01.log`：真实 TLS／生产前端原 2 秒关闭期限结束后，隧道仍不释放。
- `after-01.log`：5 项隧道回归通过；`after-full-01.log`：daemon 单元测试 204 通过、1 忽略，
  包含真实 TLS 清理及后续独立查询成功，也保留已有并发流保护检查。
- `clippy-01.log`：daemon 全目标／全特征 Clippy 通过；`change-formatted.patch`、
  `promotion-hashes.json` 保存待合入差异与文件摘要。
- `assessment-02.json`：保存本次检查汇总、待合入状态和适用边界；旧评估保留。

实际修改只在该目录的 `source/crates/daemon/src/remote_tunnel.rs` 和
`source/crates/daemon/src/remote_session/cache_tests.rs`。主工作区及安装制品仍为 P25／P26。
尚不能把所有历史 detach／daemon 停止超时归为此原因。

### Endpoint 重启后，旧连接的首次查询不能识别重置（关联 R01、R02）

受控网络屏蔽旧主机的关闭通知，确认客户端缓存仍视旧连接为打开状态，再执行一次生产
IrohController 列表请求，保持原 5 秒总期限：

| 实验 | 结果及证据（`checks/ordered-fixes/` 下） |
| --- | --- |
| 同一个 Endpoint 仅丢弃连接状态 | 首次查询约 11.199 ms 内重连成功；`reset-query-run-01.log` |
| 相同设备身份完整重建 Endpoint，使用原配置 | 首次查询 5.003 秒超时，下一次独立查询 6.285 ms 成功；`reset-query-endpoint-restart-01.log`。探针退出码 0 表示成功复现预期失败，不表示首次查询通过 |
| 只从身份秘密派生稳定的重置密钥 | 首次查询仍超时；`reset-query-stable-key-01.log` |
| 稳定重置密钥，同时采用 16 字节随机 CID | 首次查询约 1.943 秒完成重连，后续查询约 0.229 ms；`reset-query-stable-key-cid-01.log` |

Iroh 1.3.0 每次绑定产生新的重置密钥；NOQ 默认 HashedConnectionIdGenerator 也产生新的
CID 校验密钥。前者使旧重置令牌无法匹配，后者会先丢弃不再被识别的旧 CID；实验确认只固定
重置密钥不足以恢复。新 CID 实验使用 128 位随机值，避免把默认生成器的短随机部分长期复用在固定密钥下。
QUIC 对持久重置密钥、CID 不复用及重置 oracle 的约束见
[RFC 9000 §10.3.2](https://www.rfc-editor.org/rfc/rfc9000.html#section-10.3.2) 和
[§21.11](https://www.rfc-editor.org/rfc/rfc9000.html#section-21.11)。

当前实现仅在 `reset-query-probe/iroh-key-probe/`，使用明确的测试派生上下文；
不是正式 API 或产品补丁。尚需确定产品配置、同身份单 Endpoint 生命周期约束，补齐默认行为、
错误密钥、身份更换及非零路径等永久回归，再同源构建验证。不要直接把探针副本当成可安装候选。
`reset-query-run-02.log`、`reset-query-run-03.log` 的两条自定义路径夹具未建立第二路径，
超时发生在故障准备阶段；它们不是多路径请求失败证据。更早两次编译错误同样保留，不计行为回归。
完整实验汇总和探针源码摘要见 `checks/ordered-fixes/reset-query-assessment-02.json`。

最新现场 `ordered-extended-02/0035` 在前一行 daemon 崩溃重启后复用 Android 连接 39，
首次列表查询超时；本行计划的主机重启尚未注入。该事实支持继续验证重启机制，但接收计数
增加 30 字节不能证明实际收到重置包，也不能把其余新握手失败全部归因于此。

## 已完成的依赖、兼容性与策略调整

| 编号 | 调整及原因 | 已有证据与限制 |
| --- | --- | --- |
| D01 | `iroh` / `iroh-relay` 从 1.0.3 固定到 1.3.0，包含上游 Relay 发送队列拥塞时阻塞 remote actor 的修正 | 完成 Rust／Android 检查、60 ms 反向配对 10/10、独立网络门禁；`checks/iroh-1.3-*`。这是已实施升级，不能以相关上游缺陷断言全部现场超时同因；升级后真实 Relay 初连失败仍存在 |
| D02 | Android DNS 继续使用接受 SocketAddr 的兼容 API，保留 IPv6 接口 scope，避免直接换到只接受 IpAddr 的新构造器 | [dns.rs](../../crates/android/src/dns.rs)；兼容性检查通过。这是迁移约束，没有把未发生的 scope 丢失记作现场故障 |
| D03 | 将 NOQ 修正固定到 vendor，Cargo patch 和 Android Gradle 构建输入统一引用，保留来源／许可证，加入 `just ci-noq` | [ZTERM.md](../../vendor/noq-proto/ZTERM.md)、[justfile](../../justfile)；两项旧 idle 修正为 429 项通过；本轮加入 P25 重置修正后完整库 436 项通过 |
| D04 | 将 Iroh 1.3.0 固定到 vendor，增加 P27 重启重置策略、P29 关闭所有权修正及 `just ci-iroh` | [ZTERM.md](../../vendor/iroh/ZTERM.md) 记录来源、身份单实例约束、取消契约和升级边界；五项重启与六项关闭回归通过，三个生产配置使用同一实现；当前 Linux 完整库 138 通过／1 忽略 |
| D05 | 有限详细窗口内的 Relay 阶段和 QUIC 流进度；daemon 和 Android 使用白名单事件 | [transport_trace.rs](../../crates/diagnostics/src/transport_trace.rs)、[Iroh 适配](../../vendor/iroh/src/transport_trace.rs)、[流进度](../../crates/client/src/iroh_controller.rs)。匿名标签、密文数据报指纹、进程内单调时钟；不转发地址、身份、载荷或任意依赖消息。拨号／断开使用封闭错误类别。R03 追加每流最多 1 Hz 的有序字节／解码／部分帧计数，连接级 STREAM 计数不等于送达或 ACK；真实 QUIC 回归覆盖取消读、分片及 EOF 保持累计计数，嵌套字段拒绝内容注入。这是诊断能力，不计作 R02／R03 传输行为修复 |
| D06 | 用户授权首次远程请求总预算 15 秒，就绪后普通操作最多 5 秒；补齐 Android 取消及正常建连子阶段的期限传递 | [共享期限](../../crates/client/src/protocol.rs)、[Android 共享连接](../../crates/client/src/iroh_controller.rs)、[daemon 正常连接](../../crates/daemon/src/connection_broker.rs)。第一版五项回归保留；本轮另六项回归全通过，修正前四项失败，消除正常拨号十秒／出站认证五秒的提前截断。配对、入站准入和就绪操作上限保留。当前 804 项 Linux 测试通过，三端矩阵 45/52；容忍慢连接不等于已证明全部公网等待的原因 |

## 尚未解决或不能宣称解决的问题

| 编号 | 已知事实 | 已做的处理及下一步 |
| --- | --- | --- |
| R01：旧连接上的首次查询仍有失败未归因 | 旧失败保留；新 `restart-extended-01/0034`／`0082` 仍在复用连接的查询上超时，随后精确闲置清理生效。`0082` 超时期间接收计数增长，不能认为连接完全静默 | P27 已处理完整 Endpoint 重启的密钥／CID 配置问题，三个生产配置首次查询在原期限通过，CID 单独修改失败对照也已补齐。`740376ef` 三端扩展仍有失败；P27 不等于全部 R01 闭环。2026-10-12 受控复现确认主机换 Relay 重启时旧连接收不到重置、首查 5.003 秒超时，P38 改为窗口内无 QUIC 确认即交给既有重试，1.33 秒成功；附件占用连接时不替换。三端 restart 行 16/16 通过但不经过 P38；A↔B daemon broker 定向换 Relay 检查首个请求四次 2.66–4.08 秒成功；旧路径 RTT 557 ms 仍成功，754–760 ms 时窗口与重试串行超出期限，仍失败，另记 R10 |
| R02：冷启动、进程／VM 重启后的握手与首次操作仍间歇超时 | 当前 `0c99f1f6` 已补齐正常连接共享十五秒期限，六项受控回归通过。`r02-15s-matrix-02` 45 通过／7 失败，一小时准备另有三次握手失败。同源新捕获 `r02-timeout-path-01` 8/8，最慢 10.116 秒成功，未覆盖旧失败 | 旧 `0020` 的客户端 10 个数据报全到主机，主机 16 个写出仅 5 个见于客户端；`0042` Relay 建连耗 12,644 ms。新查到短暂 DNS／Pkarr 地址不一致，但未证明造成旧失败。链路等待已进一步定位，代理／运营商／Relay 的唯一归因仍缺少同次边界证据。见 `checks/ordered-fixes/r02-timeout-path/` 和文末交接；R02、完整核心／扩展／稳定性仍未通过。2026-10-11 的 P36 修复同一对端旧连接未关闭时新拨号被旧选中路径钉住的缺陷（受控 15.0 秒失败→约 0.33 秒成功），但 Android 冷启动不具备该前提，不能据此关闭 R02。2026-10-11 重建三端后，P37 对冲救回单连接 TLS 卡死；剩余失败与经 Mac 代理到公共 Relay 的路径质量同时刻吻合（成簇不可达、只建连不转发、主机重启后 home relay 被改选），见 [环境重建记录](multi-device-lab.md) |
| R03：短断网后偶发停留 synchronizing、尺寸没有更新（**定向验收通过，关闭**） | 上一候选 `723cb596` 在 `r03-acceptance-relay-hour-04/0003` 超过六十秒仍为 101×31、synchronizing：主机 6 ms 写出新快照，客户端在请求后又读 74 帧／560 KB 仍未读到。已定位为 P35：Relay 上快照排在每轮中位数 41–53 帧的过期增量之后 | P35 修复后，同源 `2d86a806` 按冻结计划：四组合 A／B Direct／Relay 各至少 10 次有效恢复全部通过（1 次注入前初连失败单列 R02），同会话一小时 60/60、含 6 次 Relay 重连，审计通过。更早的 `r03-sync-phases-03/0003`、`/0011` 未保存同类积压观测，不逐个宣称同因；完整项目验收仍见 R09 |
| R04：创建、接管或配对的结果不确定仍出现 | 新 `restart-extended-01/0029` 主机已提交接管，客户端结果不完整；`0030` 初始 Session 请求仍返回未知结果。旧晚提交证据保留，单次空查询不能证明未来不提交 | P18 修正配对分类；测试器保存只读核对、实施跨行清理屏障，不换操作 ID 重发。新故障说明剩余确认链仍需定位；保留历史 `checks/pair-deadline-repro/`、`checks/v19-failure-analysis.json`、`core-final-11/` |
| R05：CLI 正常 detach／daemon 关闭超时尚未全部对应根因 | `0029` 业务流已 quiesce，随后 ClosingEndpoint 超过五秒；受控约 2 秒 RTT 且无关闭回应时，已确认可卡在 `wait_all_draining` 直到 11.565 秒的协议计时器。现场没有唯一归因证据 | P28 修复半关闭隧道；P29 修复关闭取消／并发时提前返回及 Drop 遗漏任务取消，六项回归通过。慢链路无回应仍须如实保留五秒失败，不放宽期限，不跳过协议等待，不把强制结束算正常完成；继续捕获现场具体关闭等待与回应路径 |
| R06：公共 Relay 慢传输及跨 NAT 地址发现限制 | 独立临时身份探针的两次 128 KiB 传输为 16.805／46.774 秒，第一轮 5.022 秒只收到 24,214 字节；普通双 NAT 的 official-n0 自动发现仍选 Relay。不能把慢传输直接判为服务端限速 | P15 减少画面传输量；注入候选 Direct、强制 TCP Relay、受控 QAD Direct 有各自补充通过证据。保留 official-n0，未部署替代 Relay；真实跨物理网络自动 Direct 不在本轮已证明范围。证据 `checks/public-relay-probe-01/`、`checks/pair-deadline-network-gate-analysis.json` |
| R07：重置库缺陷已修正，现场关联仍待验证 | P25 已确认活动路径令牌登记、路由及校验缺陷，七项前失败后通过，完整库 436 项通过；原不充分探针保留 | P25／P26 同源断网／接管 80/80，随后完整扩展 72/88；P27 已处理完整 Endpoint 重启的密钥／CID 配置，补齐非零路径真实重置回归。接收字节增长仍不能识别包类型，不能把全部 R01／R02 认定同因 |
| R08：诊断并发 flush 测试失败没有唯一解释（**已定位，P39 修正，关闭**） | 历史 `checks/rust-tests-8.log` 中日志并发轮转用例首次失败，单独及 10 次复测通过，串行调度完整 workspace 通过。2026-10-12 P38 推送前的 `just check` 再次失败于同一用例：写入子进程在 1.15 秒内 `flush(3 s)` 返回 false。该函数提前返回 false 只能来自写入线程报告的结果（上次屏障后有写入失败或 sink flush 出错），所以是一次写入失败，不是等待超时。当时本机同时运行三端 VM 与模拟器；单独复跑 6 次通过（各约 0.5 秒） | 原失败保留。临时探针在 8 个用例副本并行互为磁盘负载时复现 85/120 次失败，sink 错误全部是等满一秒 `LOCK_WAIT` 的 `WouldBlock`：`Store::flush` 持 `writer.lock` 做 fsync，非排队的轮询锁让其他写入进程等锁超时。P39 改为锁内只取句柄、释放后同步，同负载 120/120、12 副本 120/120 通过；锁等待期限和断言未改。旧失败那次没有探针，按同一机制解释。证据 `checks/diagnostic-flush-repeat.json`、`checks/diagnostic-flush-2026-10-12/just-check-01.log`、`checks/diagnostic-flush-r08/`，见 [P39](#p39诊断-flush-持写入锁做-fsync其他写入进程等锁超时) |
| R09：当前完整验收尚未通过 | 当前 `2d86a806`（含 P35）Linux 工作区 817 通过／9 忽略，R03 定向验收通过；此前 R02 45/52 及稳定性准备失败仍保留，本轮 B Direct 又有 1 次注入前初连超时 | 当前完整核心 1460 项和完整扩展 88 项未执行，R03 定向一小时不能替代完整三端稳定性。A／B／C 已停止，无故障规则残留；最新证据见 `checks/ordered-fixes/r03-frame-window/` 和文末交接 |
| R10：旧路径高 RTT 时，P38 的窗口与重试串行相加超出五秒期限（**P40 修正，关闭**） | 三端 A↔B 定向检查中，旧连接 RTT 557 ms 时首个请求 3.97 秒成功；754–760 ms 时窗口达 2.5 秒上限，新连接再用 1.42–1.86 秒，请求在 5.01 秒 `deadline_exceeded`，下一次请求成功。不加延迟时主机新 Relay 路径 RTT 已有 490–686 ms | 原失败保留。本机加延迟受控复现（RTT 772–784 ms）修正前 3/3 在 5.00 秒失败。P40 在窗口过半仍无确认时后台建立不发 Hello 的替换连接，旧连接退役后与新拨号竞争；窗口、期限、计时器与重试次数不变。修正后本机 3/3 在 4.04 秒成功，晚到确认的存活主机不换连接、不重发；三端旧路径 507／1159／1213 ms 时首个请求 4.10／4.53／4.81 秒成功。剩余边界：新路径 RTT 约 1.2 秒以上仍会超时。证据 `checks/ordered-fixes/r10-replacement-dial/`，见 [P40](#p40高-rtt-时-p38-的窗口与重新建连串行超出期限) |

## 测试器、证据与测试夹具的问题和修正

这些条目不计作产品行为修复。入口为 [multi-device.py](../../tests/e2e/multi-device.py)、
[审计脚本](../../tests/e2e/multi-device-audit.py)、[45 项回归](../../tests/e2e/multi_device_test.py)；
各次运行固定自己的源码和摘要，早期不充分的证据没有事后升级为通过。

| 编号 | 问题与已实施方案 | 证据／范围 |
| --- | --- | --- |
| T01 | PTY 扩展 CSI 解析污染画面、重复关闭描述符、VM 重启后仅凭复用 PID 判进程；分别修正解析、所有权清理与进程身份核对 | 早期 `smoke-01` 至 `smoke-06`；`test_split_extension_queries_do_not_corrupt_terminal_cells`，历史过程保留夹具失败 |
| T02 | Android mailbox 发布顺序与读取时点竞争；修正阶段信号发布，使用私有目录和明确 serial | [MultiDeviceTest.kt](../../apps/android/app/src/androidTest/java/io/github/leonfox28/zterm/MultiDeviceTest.kt)、历史过程 |
| T03 | 仅 TIOCSWINSZ 不触发 SSH 远端 resize；底层 Active 也可能早于终端输入栅栏开放 | 显式 SIGWINCH 通知所属 SSH 子进程，并等待真实 PTY 输入呈现；恢复后仍在原绝对期限只发送一次 nonce；`checks/coordinator-19.log` |
| T04 | 重启夹具混淆正常 guest reboot 与强杀模拟器；命名会话恢复缺少错误回退对照，上传故障时机不准确 | 区分故障类型，加入独立 `main` 对照，修正上传故障阶段；原 `extended-01` 39 通过／41 失败保留 |
| T05 | Android 打开主机失败后仍继续 select，可能掩盖第一次错误 | 首次失败立即保留并结束该行，不以额外选择请求替代；`checks/android-open-before.log`、`checks/coordinator-13.log` |
| T06 | 已自动恢复到目标的 synchronizing / reconnecting 会话被重复 select，造成占用或结果不确定 | 保留原帧并等待同一次连接；仅不同会话或明确接管走选择流程，贴合真实界面；`checks/android-retained-selection-before-01.log`、`checks/coordinator-59.log` |
| T07 | 并发接管未等第二请求结束就判胜者，明确占用后漏做用户已请求的接管，CLI lease_lost 正常失权又被判异常 | 等待双方终态，处理明确占用／显式接管和明确失权；未知结果仍不重发；`checks/coordinator-25.log`、历史 `iroh-1.3-smoke-01` 与 `iroh-1.3-race-01` |
| T08 | detach 超时后强制 SIGTERM 或非零退出仍可能算正常退出 | 正常 detach 必须无强制结束且退出码 0，metrics 保存清理方式与退出码；`checks/normal-detach-before.log`、`checks/coordinator-31.log` |
| T09 | 原 offline 仅阻断出站，旧端仍可能收到接管通知，未验证完全断网 | 测试 UID 出站 + 该 UID socket 端口双栈入站，保留 SSH／adb；规则日志与 EOF guard 异常恢复；双向实际 DROP 任一缺失不得通过；`checks/bidirectional-fault-before.log`、`checks/coordinator-34.log`、`checks/coordinator-39.log` |
| T10 | 短时丢包期间只有主机输出，控制端可能没有出站，导致证据不足 | 故障前有界 PTY 输出，故障中仅一次真实 resize，恢复后核对原尺寸／连续性，不离线输入 shell；`checks/short-outage-traffic-repro/`。原 `1068` 的单向计数不足仍为 blocked |
| T11 | 客户端检测和主机释放各拿 120 秒，累计超出上限；慢查询超时返回成功仍被接受 | 共享从故障开始的绝对期限，期限后观察也拒绝；`checks/shared-deadline-before.log`、`checks/coordinator-42.log` |
| T12 | Linux 冷启动先 restart daemon 并等 Online，漏测 CLI 自动启动时序 | 只 stop 旧 daemon，直接由真实 CLI connect 启动，连接耗时含启动；`checks/cold-cli-bootstrap-before.log`、`checks/coordinator-44.log` |
| T13 | 前一场景恢复后主机仍 Degraded，污染下一独立行；只在路径模式改变时等 Online | 每个独立远程场景前只读检查主机本地就绪，不向 peer 预热，不预启冷控制端；本地连接不要求 Online；`checks/host-readiness-before-01.log`、`checks/host-readiness-after-01.log` |
| T14 | 配对失败清理仅查一次授权，可能清理后才提交；确认退出失败也可能继续矩阵 | 对未成功返回的临时配对先停止测试主机并确认旧 daemon 退出，再只读核对／撤销精确授权；无法确认则保留原错误并停止后续矩阵；`checks/pair-cleanup-barrier-before-01.log`、`checks/pair-barrier-stop-repro/` |
| T15 | 创建结果不确定后的空查询被当成隔离完成，晚提交污染下一行 | 保存原失败和只读结果，再确认该测试主机旧进程退出；审计要求清理屏障，无法确认停止矩阵；仅用于独立矩阵行，不重置连续性稳定性会话；`checks/session-isolation-repro/` |
| T16 | 配对证据被日志收集错误覆盖，或用会话列表冒充授权核对；配对成功后会话结果不确定又只查授权 | 授权清理前后各保存快照，收集失败不覆盖原错误；按操作类型分别只读核对授权与会话，审计强制对应证据；`checks/pair-reconciliation-before-01.log`、`checks/pair-audit-before-01.log`、`checks/post-pair-session-reconciliation-before-01.log` |
| T17 | 用显示名识别 Android 授权，误匹配旧临时 Linux 身份 | instrumentation 暴露本地公钥，撤销严格核对精确公钥；旧残留只在有明确来源时清理；`checks/orphan-pair-authorization-analysis.json`、`checks/coordinator-51.log` |
| T18 | 稳定性重开没有完整独立 PTY，失败 metrics 被后续接续污染，单端连续重开不利继续采样 | 唯一 PTY 文件名、失败时快照、独立 continuation.metrics；只读确认原 ID 后最多三次显式跨端接续，每次失败保留，未知结果／原会话消失即停，不新建替代；`soak-final-04/pty-evidence-note.json`、`checks/soak-alternate-reopen-before.log` |
| T19 | soak 把故障条件不足 Blocked 写成 Failed，或含非通过动作仍可能声称整体通过 | blocked 单列，整轮仍未通过；审计要求完整时长、原会话和所有动作状态；`checks/soak-status-repro/`、`checks/coordinator-64.log` |
| T20 | 并发命令结束时使用相同最新序号，可能覆盖证据文件 | 命令开始前分配独立编号并串行追加索引；`checks/concurrent-command-evidence-before-01.log`、`checks/coordinator-57.log` |
| T21 | matrix 预览忽略筛选，显示 2340 行，而实际 core run 为 1460 行 | 预览与运行共用筛选，拒绝未知场景；`checks/matrix-preview-filters-before-01.log`、`checks/coordinator-58.log` |
| T22 | 私有配对命令失败时完全隐藏错误，无法分辨公开错误分类 | 仅透出白名单公开错误码，私有票据／内容仍隐藏，未知字符串不输出；`checks/private-error-kind-before-01.log`、`checks/coordinator-56.log` |
| T23 | Android UI 测试默认寻找 my-mac，且完整标题匹配不适应 Linux OSC 标题追加 | 要求显式测试 hostName，并用唯一会话名匹配；保留几何、选择、恢复断言。通用 64 通过／35 条件跳过；指定 A 两项 UI 通过；`checks/ordered-fixes/ui-fixture-android-runtime.json` |
| T24 | SettingsUiTest 将返回首页清理也塞入 1 秒设置断言窗口，保存会话恢复遇阻时失败 | 分开清理等待和设置断言，设置断言原 1 秒不变；空状态／受阻恢复对照各 10 次，20/20 通过并还原应用状态；`checks/settings-restore-before-01/`、`checks/settings-restore-after-01/results.json` |
| T25 | 长运行只续开 Linux 详细日志，Android 15 分钟到期遗漏失败时证据 | 新增 [diagnostics helper](../../tests/e2e/multi-device-diagnostics.py) 显式续开三端有限窗口，以 Android 自身时钟／应用 UID 写控制标记，不启动实例、不重启应用、不请求 peer；停止状态拒绝和运行中续开均验证。已缺失日志不能补回 |
| T26 | vendor 协议 token 计算表达式被扫描器当成硬编码凭据 | 区分字符串字面量与运行时表达式，继续扫描 vendor、真实凭据格式、配置／shell 未引号赋值，新增拒绝夹具；`checks/noq-vendor-secret-*.log` |
| T27 | 并行 tracing 全局 callsite 竞争导致既有配对日志断言偶发计数 0 | 将日志断言隔离到子进程，保留恰好一次及票据不泄露断言；[local_pair_ipc.rs](../../crates/daemon/tests/local_pair_ipc.rs)。不同于 R08 的等锁失败（P39） |
| T28 | 模拟器重启后 adb root 短暂连接关闭、主动停止 instrumentation 被误报崩溃 | 在有界窗口确认实际 UID 就绪，区分主动停止与意外退出；`android-reboot-final-01` 无同类管理失败，其 3 项产品失败仍保留 |
| T29 | 重启后 Relay 规则恢复、双栈部分失败和清理错误可能漏清其他设备／地址族 | 恢复范围仅所属测试链；一项清理失败仍尝试其他设备和地址族，管理规则检查、故障日志及 EOF guard 有对应回归；原 45 项运行器／审计检查通过 |
| T30 | 独立 Android 验收在 VM 重启后直接开始，产品 daemon 仍为 Stopped；基础网络预检不保证产品主机已启动 | 新增 [本地主机准备工具](../../tests/e2e/multi-device-hosts.py)，通过显式 local 查询启动已配置 daemon，并记录前后状态、等待 Online；不重启活跃服务、不创建会话、不预热控制端。三项回归及完整运行器 48 项通过。`cold-start-fix/host-readiness-analysis-02.json` 区分前提错误与修正后仍存在的真实握手失败，原日志不改写 |
| T31 | 短断网恢复只断言 Active／inputReady 和主机尺寸，未要求 Android 显示尺寸同步 | [运行器](../../tests/e2e/multi-device.py) 在原 60 秒期限内等待 Android 实际尺寸匹配，再发送连续性输入；[审计](../../tests/e2e/multi-device-audit.py) 按 `host-and-android-surface-v1` 检查尺寸和输入状态，旧证据保留原契约。两种尺寸滞后回归修正前均失败、修正后通过，另验证审计拒绝缺失／错误尺寸和不可输入状态；运行器及主机准备测试 49/49。旧制品 B→C 十轮通过；证据 `checks/ordered-fixes/r03-sync/` |
| T32 | Android instrumentation 已退出，恢复轮询仍读取遗留状态文件，将观察器故障误报为同步超时 | [运行器](../../tests/e2e/multi-device.py) 的状态断言先确认测试进程存活，退出立即报错；证据采集仍显式保留最后状态。退出码 0／1 两种回归修正前均失败、修正后通过，运行器及主机准备 50/50；`checks/ordered-fixes/r03-relay-cause/coordinator-before.log` 和 `coordinator-after.log`。原 `cause-01/0016` 失败不改写，结合 P33 的 EMFILE 与已收到的快照 ACK 单列归因 |
| T33 | 重建环境后首次运行，C 的配对票据写入 `files/multi-device-ticket.txt` 失败，四个 Android 行被配对前提阻塞：新装应用在 08:56:07 才首次写入 `files/`，而写票据发生在 08:56:04–05，运行器又不创建目录。旧模拟器的 `files/` 早已存在，因此此前未暴露。改为先 `mkdir -p files` 再写入；新增回归在无 `files/` 的临时应用目录执行设备端脚本，修正前以 `No such file or directory` 失败、修正后通过，协调回归 48/48 | `$LAB/lab-rebuild-cold-01/pairing.json`、`checks/lab-rebuild/ticket-dir-before-01.log`、`ticket-dir-after-01.log`；见 [环境重建记录](multi-device-lab.md) |
| T34 | `p37-cold-01` 第 2 轮 A←C Direct 以 `'hosts'` 失败：Android 桥在发布首个 `status.json` 之前就写入 `ready`，运行器看到 `ready` 后立即读状态得到空值，`snapshot()` 返回 `{}`，`connect()` 取 `hosts` 时 KeyError。状态文件本身以临时文件加 rename 原子发布，不是读到半个文件。改为先发布状态再写 `ready`，需重建测试 APK | `$LAB/p37-cold-01/0009-cold-A-C-C-direct-2/`；见 [环境重建记录](multi-device-lab.md) |

## 环境与构建问题

| 编号 | 发现及处理 | 仍需保留的限制 |
| --- | --- | --- |
| E01 | 仅 vzNAT 时 A ↔ B 实测不可达；保留 VZ / vzNAT 并添加共享 user-v2 网卡，六向 ping／UDP 预检通过 | 实际网络与最初仅 vzNAT 计划不同，未安装额外特权网络组件 |
| E02 | 独立网络门禁硬编码 eth0，而 VM 默认出口为 lima0；新增 ZTERM_GATE_EGRESS_INTERFACE，保留 Docker 默认值 | 补充门禁与三设备产品链路分别统计；历史原始 UDP 反射超时及 v18 结束单向 UDP 失败保留，后续六向通过不覆盖它们 |
| E03 | 临时启动器解析 Python 虚拟环境符号链接后丢失 pyte | 使用虚拟环境入口并重跑；`partial-lookup-smoke-01` 保留为夹具失败／中断，不能算产品通过 |
| E04 | Linux VM 多次因旧测试可执行文件和增量缓存耗尽磁盘，链接未完成 | 仅清理已识别生成缓存，保留源码、制品及证据后完整重跑。历史 `checks/linux-old-test-cache-prune-02.json`、`checks/linux-cache-cleanup-04.json` 保留；本轮 `endpoint-restart-fix/linux-debug-cleanup-02.json` 核对安装二进制未变，并关闭测试调试符号／增量缓存，Linux 完整 792 项通过 |
| E05 | Android 本地包缓存引用临时检出；Mac 特征组合引用旧依赖制品，造成编译失败 | 定点清理本仓库本地包缓存并核对来源后重建。Android 对照 v19 的 18 个文件一致；编译错误不算行为复现。`checks/ordered-fixes/progress.json` 的 environment_incidents 与 `mac-cache-before.json` |
| E06 | Linux 检查忘记既定 umask 077，002 下身份重置夹具触发 PathUnsafe | 002 失败、022／077 通过；恢复 077 后完整重跑，没有放宽产品目录权限检查；`checks/ordered-fixes/linux-lib-01.log` 保留 |
| E07 | 早期 core-final-03 的协调进程退出，未留完整结束记录，原因不明 | 保存原 568 行，剩余明确跳过：363 通过、205 失败、892 跳过；不能拼接成十轮完成。后续运行使用固定运行器、独立证据、审计及清理记录 |
| E08 | 本轮 Mac Iroh 全库测试时用户拒绝传入连接，只读核查确认该测试可执行文件被系统防火墙阻止 | 已停止受影响轮次并保留 `endpoint-restart-fix/iroh-full-01.log`、`firewall-incident-01.json`，不修改防火墙。Mac 内存传输回归通过；完整网络库检查转到隔离 Linux VM，不能把受限轮次算作通过 |
| E09 | 2026-10-11 发现 `target/` 已于 2026-10-10 17:08 重建：运行器的 `environment.json`、venv、日志和全部 `checks/` 证据丢失；C 的 AVD 由 `create` 建在 `environment/avd` 下，也一并删除，仅剩指向不存在路径的 `~/.android/avd/zterm-md-20261003t024103z-c.ini`。A／B 在 `~/.lima`，仍在 | 重新进行三端实验前必须重建 C（含配对）并重建环境清单；不能把旧 VM 当作已核对的环境。实验环境放在 `target/` 下会被清理命令一并删除，后续应考虑移出 |
| E10 | 2026-10-11 C 的 Direct 行在 Relay 走代理与否时都只剩 Relay 路径。原因是 herdr 后台服务丢失 macOS“本地网络”权限：从它派生的会话与模拟器向任何局域网地址发包都返回 `No route to host`。重启 herdr 服务后新标签页恢复，模拟器需从新标签页重启 | 已在运行的进程不会随 herdr 重启恢复；失效原因未确认。之前因此失败的行属于环境问题，不计作产品或代理规则结论；见 [环境重建记录](multi-device-lab.md) 第 11 节 |

## 续修前的交接点（保留历史）

1. 产品停在已安装的 `722ac993` 候选；本次没有继续修改产品或升级依赖。
2. 最新确定修复是 P23：超时后的精确闲置连接清理。R01 的首次超时仍失败。
3. R07 独立副本的第二版探针已结束，0 通过／2 失败，均未达到“连接层识别重置并关闭”的断言；没有正在运行的探针。
4. 后续若继续修复，先补齐 R07 的报文／令牌登记证据；若假设不成立，应保留否定结果并继续查 R01 / R02。
5. 任何新修复先保存首次失败、做针对性回归，再同源构建和真实三端复测；不延长期限、不预热冷端、
   不以新操作 ID 重发变更、不新建恢复替代 Session。最终十轮及一小时验收仍未完成。

机器可读交接入口：`checks/current-handoff.json`。本次额外核对及勘误：
`checks/ordered-fixes/issues-record-01.json`、`checks/ordered-fixes/idle-cache-path-evidence-correction-01.json`、
`checks/ordered-fixes/reset-token-probe/before-02-assessment.json`。


## 上轮交接（P25／P26，保留历史）

P25 的七项 NOQ 回归、P26 的三项真实 TLS 失败复现均已修正，原始失败保留。
当前制品为 `65dd801d`，平台检查及十轮 80 项定向回归完成，实验环境已停止。
下一步仍是同源完整核心／扩展／稳定性验证及 R01／R02 等历史间歇问题定位，
不能把本轮全通过写成全部验收通过。查询／创建／接管期限及操作身份约束未改变；
代码仍未提交或发布。机器交接入口为 `checks/current-handoff.json`。

## 2026-10-06 进展交接

1. 已安装且主工作区保持的仍是 `65dd801d`（P01–P26）；363 项构建输入核对一致。
   新的两个候选只在 `target/verification/multi-device/checks/ordered-fixes/` 的隔离副本中。
2. 完整扩展矩阵为 72 通过／16 失败。A/Direct 22/22、A/Relay 13/22、
   B/Direct 15/22、B/Relay 22/22；全部失败及两端阶段保留，审计、收集、最终预检和停止均完成。
   首次启动曾因 B→A UDP 预检超时终止，没有进入产品矩阵；失败记录为
   `ordered-extended-validation-01.json`，第二次运行使用新编号且预检通过。
3. 下一步先完成 Endpoint 重启恢复的正式配置和永久回归，检查身份／密钥隔离、CID 唯一性、
   非零路径与单 Endpoint 所有权；当前只有“原配置失败、只固定重置密钥仍失败、两处同时修改通过”，
   尚无“仅改 CID”对照。再合入半关闭候选并完成同源全平台验证。
4. 最新新握手／Hello/Welcome 失败仍需继续定位；R03、R04、R06、R08 没有新增闭环证据。
   核心十轮与 3600 秒验收仍待后续候选完成，不扩大期限、不重发新 ID 的变更、不替换恢复 Session。
5. 实例已停止，无残留故障规则、运行任务或排队测试。没有提交、发布或更换已安装制品。
   详细机器交接见 `checks/current-handoff.json`，本次快照见 `checks/ordered-fixes/ordered-progress-20261006.json`。

## 2026-10-08 进展交接

1. P27／P28 已正式实现、安装并完成平台检查，当前制品 `740376ef`、445 项输入匹配；
   不再从旧隔离副本重复合入。最新完整扩展 `restart-extended-01` 为 72/88，全部失败保留。
2. R05 已新增明确的 ClosingEndpoint 等待证据。下一步需要在受控链路复现并区分
   QUIC 关闭等待与库内任务退出；不要延长原 5 秒或将超时强制结束改记成功。
3. R01／R02／R04 继续按新连接、旧连接查询及变更结果确认分别定位；重点对照
   `0018`、`0029`、`0030`、`0034`、`0082` 的两端阶段，不能据接收计数判定包类型或数据内容。
4. R03／R06／R08 的未闭环边界及当前核心十轮、3600 秒稳定性验收仍保留。
   A／B／C 已停止、无故障规则和排队测试，没有提交或发布；汇总和后续入口见
   `checks/ordered-fixes/endpoint-restart-fix/final-summary-01.json`、`checks/current-handoff.json`。

## 2026-10-08 R05 续修结束交接

1. P29 已实现并同源安装，当前源码归档 `61806d256b4ec6ed`，446 项构建输入匹配。
   三项原关闭所有权回归先失败后通过；最终六项关闭与五项重启回归、Mac／Linux 完整
   workspace、Linux Iroh 完整库、Clippy／静态检查通过。没有提交或发布。
2. P29 修复取消／并发时的提前返回与 Drop 遗漏取消，不能使高 RTT 且无回应的关闭在
   原五秒内正常完成。受控 `wait_all_draining` 超时证据已保存，原现场仍无唯一归因。
3. 当前三端定向 28/32，四项都在初连失败；实际 detach 15 项通过，22 次可关联的
   Endpoint 关闭均完成，最长 1,768 ms。Android 基础两轮各一次请求超时，其中复测
   定位到首次列表；复测 UI 两次请求超时也保留。下一步优先定位 R02，不能重发不确定变更。
4. 最终预检／收集／停止成功，A／B／C 于 06:51:28 UTC 停止，无残留故障规则或任务。
   当前完整核心／扩展／稳定性未执行。汇总为 `checks/ordered-fixes/endpoint-close-fix/final-summary-01.json`，
   机器交接为 `checks/current-handoff.json`；旧 `740376ef` 全部结果独立保留。

## 2026-10-08 R02 续修结束交接

1. P30 Relay 队列按包年龄过期已实现并安装，当前 `9593ab6bd1f16fb4`、447 项构建输入
   与归档一致。三项队列回归及真实 Relay／TLS 首次请求回归通过，受控首次请求从原五秒
   超时变为 3.414 秒成功。Mac／Linux 完整工作区、Linux Iroh 全库、Clippy／Android
   静态检查通过，生产期限与 QUIC 计时器未改。
2. T30 修正独立 Android 验收前遗漏本地主机启动的问题，新增三项回归、完整运行器 48 项通过。
   P29 第二轮和 P30 首批离线主机结果保留；P29 首轮与矩阵失败没有被一并重分类。
   主机正确就绪后的 Android 仍 63 通过／1 首次握手失败／35 条件跳过，权限 4 项和 UI 2 项通过。
3. 真实定向矩阵 `cold-start-01` 为 27/32，五项都在初连失败，15 项实际 detach 通过。
   下一步按 Relay 连接、首包发送、QUIC 握手及 Hello 阶段补充无内容关联证据，优先定位
   主机就绪后的 Android 首次列表握手 4,937 ms 超时，及 `0012`／`0016`／`0022`／`0023`／`0028`。
   当前没有证据将这些失败唯一归因于同一机制；不延长期限、不预热冷端、不重发不确定变更。
4. A／B／C 于 07:54:51 UTC 停止，无残留故障规则、运行或排队任务；没有提交或发布。
   当前完整核心 1460 项、完整扩展 88 项、3600 秒稳定性未执行，R02／R05 等剩余边界保留。
   汇总为 `checks/ordered-fixes/cold-start-fix/final-summary-01.json`，机器交接为
   `checks/current-handoff.json`；旧版本结果独立保留，T30 更正见 `host-readiness-analysis-02.json`。

## 2026-10-08 R02 阶段定位交接

1. 当前同源制品 `b91e3cd14c19a8dd`、449 项构建输入匹配；保留 P01–P30。
   本轮只增加 D05 诊断能力，未新增已证明的握手行为修复。有限日志关闭后不计算包指纹，
   不启用无关依赖 trace，不屏蔽原业务事件；匿名关联、白名单、敏感字段拒绝回归通过。
   固定依赖版本的 TLS 阶段标记及升级约束见 `vendor/iroh/ZTERM.md`。
2. 当前 `relay-trace-cold-02` 为 30/32，Android `0003` 已捕获完整 Relay TLS 等待链，
   Linux `0000` 缺少失败时的详细日志，仍未归因。前一诊断制品的 26/32 独立保留；
   两轮通过率不可当作修复效果。两轮审计均无不一致。
3. 最新指定 A 的 NativeTerminalTest 全新 C 进程五次通过；仅记录 TCP 方向／长度的
   补查没有再次捕获 TLS 卡顿，因此不能为旧失败补足报文证据。临时身份探针只作诊断，
   其中合成主机未包含真实 broker 的 Welcome 送达等待，20 次通过不等于完整产品回归。
   中断前没有产出新的 Native 测试结果，空 TCP 捕获也没有被算作通过或失败。
4. 最终 Linux 完整工作区 793 通过／9 忽略（另有一次独立子进程辅助输出，不重复计数）；
   Iroh 完整库 143 通过／1 忽略，Mac／Linux Clippy、Mac／Linux 诊断各 9 项、Android
   构建／lint／JVM、策略检查通过。本轮未重跑 Mac 完整工作区，也未执行完整核心
   1460 项、完整扩展 88 项或 3600 秒稳定性，旧制品结果不迁移。
5. 下一步应在同一次失败中同时保留 Relay TCP 收发元数据及 TLS 阶段，明确等待来自
   何处，再决定修复。当前证据不足以更换握手协议、删除 Welcome 送达屏障，或把失败
   判为公共 Relay 限速；原五秒期限、QUIC／Relay 计时器和不确定变更不重放规则仍保留。
6. A／B／C 于 **11:20:01 UTC** 停止，故障规则清理、最终六向预检和日志收集成功，
   没有运行或排队任务、没有提交或发布。证据为
   `checks/ordered-fixes/relay-handshake-fix/final-summary-01.json`、
   `matrix-analysis-01.json`／`matrix-analysis-02.json`、`android-first-request-analysis-02.json`；
   机器交接为 `checks/current-handoff.json`。R02 保持未解决，不能将 D05 记作 P31 修复。

## 2026-10-08 R02 TCP 与出站链路定位交接

1. 沿用 `b91e3cd14c19a8dd`，449 项构建输入一致，没有新增产品代码补丁。
   `relay-tls-io-01` 为 22/32，`relay-tls-io-02` 为 29/32，两轮审计无不一致；
   失败阶段原始记录见各行 `result.json`，按当时进程提取的阶段见
   `checks/ordered-fixes/relay-tls-io/failure-stages-01.json`。原失败均保留。
2. 有限捕获在首项前确认就绪，在同一实验结束时按所记录 PID 停止并保留统计。
   第一轮只覆盖 `usw1` 的当前 IPv4，第二轮覆盖四个默认 Relay 的当前 IPv4；
   没有将未覆盖地址的缺包当作无收包。六份捕获共 28,959 包，解析数量与结束统计
   一致、内核丢包均为零；TCP 源端口重用按新的 SYN 序列拆开，不混合两次连接。
3. `relay-tls-io-02/0009`：Linux A 的 PID 10054、Relay 匿名标签及端口 37544
   由 TLS 起始时间和 `ss` 的 PID／fd 记录共同对应。TLS 完成 2,004 ms，HTTP
   升级完成 2,623 ms，认证确认 5,224 ms；请求在此前已经超时。2,600.901 ms 的
   入站 TCP 负载空档内，五次套接字队列抽样均为空。认证数据到达后立即推进，
   不能解释为数据持续积压在客户端接收队列。详见 `failed-auth-io-01.json`。
   报文没有解密，内容阶段来自库内标记；该证据仍无法区分公共链路、代理转发和服务端。
4. 出站只读检查识别出 `utun5`／BettboxCore。独立探针第 11 次的本地 TCP 在
   0.693 ms 就绪，相关外层连接至少到 1,870 ms 仍为 `SYN_SENT`，2,153 ms 的
   抽样才成为 `ESTABLISHED`；TLS 在 3,475 ms 完成。关联依据是同目标唯一新增
   外层连接、时序和发送字节数一致，不是代理内部连接 ID，见 `tun-boundary-01.json`。
   这证明本地 `relay_tcp_ready` 不能当作公共对端 TCP 已就绪；没有替旧失败补作归因。
5. Mac 探针仅主动出站，不启动入站监听，也没有修改路由、防火墙或 Bettbox 配置。
   `curl` 的 24 次对照有 23 次收到 HTTP 404、1 次物理出口 TLS 超时：404 只用于
   响应时序，不能记为 Relay 健康检查通过。OpenSSL 的 24 次五秒观察中，21 次
   TLS 完成、2 次到达期限、1 次物理出口 TCP 超时。物理出口一例在 400 ms 收到
   2,856 字节后直到 5,005 ms 没有后续数据；TUN 一例在约 5,003 ms 读到 EOF。
   后续八秒诊断观察的 12 次均在约 0.38–3.48 秒完成，没有再次复现 EOF。
   所有探针都不是产品请求；OpenSSL 的首包大小和 TLS 实现也不同，原产品期限未改。
6. 更正上一轮表述：Iroh `CONNECT_TIMEOUT` 为十秒，五秒的是首次产品请求期限。
   D05 当前不含拨号错误类型，不能凭约五秒断言该 Relay 计时器触发。固定 1.3.0
   的服务端代码先发送认证确认，再同步登记客户端；没有证据支持“等待旧注册释放”
   作为此处原因，公共服务的实际实现另需验证。未见显式 `RateLimited` 消息，也不
   等于排除了所有服务端限制。R02 保持未解决，下一步需要独立出口或服务端时序对照。
7. 收集、六向预检、停止通过，A／B／C 于 **12:04:33 UTC（20:04:33 上海时间）**
   停止，后续 Mac 只读探针也已退出。没有实验任务、故障规则残留、提交或发布。
   汇总为 `checks/ordered-fixes/relay-tls-io/final-summary-01.json`，当前机器交接为
   `checks/current-handoff.json`。完整核心／扩展／3600 秒稳定性仍未执行。

## 2026-10-08 R02 新路径补发修复交接

1. P31 已实现，同源源码归档 `27a35c53900b71ce`、451 项构建输入一致，A／B／C
   均已安装。改动限于 Iroh 远端路径处理、Initial 缓存及回归；保留 P01–P30、
   五秒产品期限、生产 QUIC 计时器及 Hello/Welcome 交付屏障。
2. 受控新地址在 100 ms 返回，内存真实 QUIC 首次请求由 1.018 秒降至约 123 ms；
   4.1 秒 TLS Relay 启动夹具由 5.003 秒超时变为 4.371 秒成功。八项新增回归通过，
   Linux 工作区 793 通过／9 忽略（子进程辅助测试不重复计数），Iroh 151 通过／
   1 忽略；Mac 无监听 Iroh 门禁 22 项通过。Mac／Linux Clippy、Android 构建／
   lint／JVM、策略、格式、秘密扫描通过。Linux 最初额外测试 lint 的命令遗漏
   `metrics` 特性，其配置失败保留；补齐命令后的 `platform-validation-02.json`
   通过，没有为此改产品代码或删除测试。
3. `fresh-route-cold-01`：31 通过／1 失败；`fresh-route-reboot-02`：16/16 通过。
   两轮均审计无不一致，有限诊断窗口在首项前显式续开。首次重启筛选误用了 core
   分组、实际零项，已标为未执行，不计成功；完整 16 项在 `-02` 覆盖。
   Android 全新进程 NativeTerminalTest 五次通过，目标依次 A／B／A／B／A；
   主机仅做本地状态和 Session 就绪检查，没有预热控制端到主机的连接。
4. 保留失败 `fresh-route-cold-01/0024`：新路径 Initial 已在 1,755 ms 补发，
   TLS 3,476 ms、认证 4,742 ms 完成；五秒内 QUIC 未就绪。新路径补发修复
   已生效，但没有消除该样本的后续等待。见 `remaining-cold-failure-01.json`。
   本轮没有 TCP 捕获，不能唯一归因于链路、透明代理或公共服务端；前轮无入站
   数据的证据仍保留，不把所有历史故障合并为 P31，也不按公网通过率宣称因果。
5. 收集、六向预检、停止均通过，A／B／C 于 **14:17:51 UTC（22:17:51 上海时间）**
   停止；无故障规则残留，协调锁已释放。没有提交或发布。汇总为
   `checks/ordered-fixes/fresh-route-initial/final-summary-01.json`，机器交接为
   `checks/current-handoff.json`。R02 尚未全部解决，当前完整核心 1460 项、
   完整扩展 88 项和 3600 秒稳定性仍未执行。


## 2026-10-09 R02 启动期限调整交接（D06）

1. 用户明确要求按讨论方案实施：远程初次请求的建连与首次响应共用 15 秒绝对截止时间。
   连接就绪后普通操作最多五秒，且不能跨过总截止时间。共享 unary 在操作开始前完成
   认证；允许的字节相同重试共用一次操作期限。终端附着在通道打开后将剩余期限收紧为
   最多五秒。本地操作、活动终端控制、配对、重连循环、关闭及 QUIC／Relay 内部计时器
   保持原策略。P27 的完整 Endpoint 重启夹具仍显式使用原五秒总期限，以保留更严格回归。
2. CLI 进度与取消结果保留回归通过。Android 连接中显示明确的取消按钮，返回首页会取消
   等待中的 native 调用，结束 busy 状态；取消后可以再次发起连接。已提交变更没有增加
   重发策略，操作结果不确定的分类不变。
3. 源码归档 `source-8dd3c970a3276c18.tar.gz`，451 项构建输入一致。Linux 工作区
   **798 通过／9 忽略**（已排除日志中单独打印的 panic 子进程辅助项重复计数）；新增
   五项 Rust 回归通过，包括真实 TLS／Hello 六秒认证成功后，同一连接无响应查询仍在
   五到六秒内失败、且不获得新的启动预算。Mac 无 socket 预算回归两项、CLI 进度与取消
   一项通过；Mac／Linux Clippy、Android assemble／lint／JVM、策略和秘密扫描通过。
4. `startup-budget-cold-02` **16 通过／0 失败／0 阻塞／0 跳过**，覆盖两台 Linux 主机、
   桌面和 Android 控制端、Direct／Relay、冷启动及首次打开已有 Session；覆盖审计零问题。
   Android `SessionRefreshTest` 与 `ConnectionStatusUiTest` 合计 **10/10**；两个全新
   进程的 `NativeTerminalTest` 分别指定 A、B，**2/2**。公网通过率只描述这轮结果，不能
   唯一归因于超时调整；受控时序回归验证新旧预算行为。完整 1460 核心、88 扩展和
   3600 秒稳定性尚未在此制品执行，R02 不宣告全部闭环。
5. `startup-budget-cold-01` 最初误用系统 Python，缺少 `pyte`，九项均为依赖缺失；原始
   结果 **3 通过／9 失败／4 跳过** 保留。发现后中断、收集并停止环境，使用已有 venv
   完整重跑为 `-02`；首轮不计作完成验收。说明见 `runner-environment-01.json`。
6. 最终日志为 `collection-startup-budget-02`，A／B／C 于 **01:53:07 UTC / 09:53:07
   上海时间** 停止；故障规则已清理。制品 SHA、完整校验和验证汇总见
   `checks/ordered-fixes/startup-budget/final-summary.json`；机器交接已更新为
   `checks/current-handoff.json`。没有提交或发布，未修改真实用户身份或系统网络配置。

## 2026-10-09 R02 大陆网络十五秒续验交接

1. 验收明确区分延迟与错误：成功的慢连接计通过，不另设旧五秒速度门槛。就绪后的
   普通查询仍受五秒限制；UI 多步骤总耗时、单次请求期限、VM 启动及恢复窗口分别记录。
   两台 Linux VM 和 Android 模拟器沿用同一台 Mac 的现有公网出口及 official-n0，
   不代表多个独立运营商或跨地域物理设备的覆盖。
2. 第一版 D06 的 `r02-15s-matrix-01` 完成 47 通过／5 失败，审计零问题。三个 Android
   请求在 10.001 秒被正常拨号内部上限截断；另一次认证耗尽十五秒，一次在认证后
   的五秒终端响应窗口返回结果不确定。后者经只读核对，主机会话已经创建，未自动重发。
   证据为 `baseline-classification.json`、`response-timeout-0025.json`。
3. 已补齐正常连接 setup 的预算传递。共享 Iroh 客户端不再套用配对十秒上限；daemon
   的地址发现、传输、出站 Hello/Welcome 和候选确认使用同一个十五秒截止时间。
   共享 dial worker 的窗口不替代请求所有者自己的绝对期限。配对、入站首帧准入、
   已连接操作、取消语义和底层 QUIC／Relay 计时器保留原行为。
4. 六项新增回归覆盖十一秒传输加一秒认证成功、六秒认证成功、启动合计最多十五秒，
   以及配对仍在十秒结束。修正前四项失败、两项边界检查通过，修正后六项全通过；
   Linux 工作区 804 通过／9 忽略（不重复计入 panic 子进程辅助项）。Mac／Linux
   Clippy、Android assemble／lint／JVM、格式、策略和秘密扫描通过。首次 Mac
   Clippy 捕获测试中的 `unwrap`，改为有上下文的 `expect` 后重跑通过，原日志保留。
5. 同源归档 `source-0c99f1f61292d458.tar.gz`，451 项构建输入与本地／归档一致，
   A／B／C 安装哈希均通过预检。`r02-15s-matrix-02` 为 45 通过／7 失败、无阻塞
   或跳过，审计零问题。失败细分和 RTT／丢包证据见 `after-classification.json`；
   其中五项在场景连接阶段失败，两项在客户端重启后失败，未执行的后续断言不算覆盖。
   冷启动／首次打开成功 UI 耗时 1.991–7.245 秒；`0023` 的 13.919 秒连接步骤成功，
   但后续自动恢复超时使该整项失败。两轮公网结果不作为预算修正的因果比较。
6. 编排脚本为独立 soak 错填 `scenario='all'`，矩阵已经完成，soak 尚未启动；
   该配置失败保存在 `soak-setup-error.json`。改用 `scenario=None` 后，独立
   `r02-15s-soak-03`（种子 20261009）仍在准备阶段 B／C／B 三次初连失败，
   后台分别记录约十五秒握手等待，第三次前台显示隧道在 Opened 前关闭。
   原会话每次只读核对均仍存在，没有替代或重建；准备 51.521 秒，正式计时 0 秒、
   随机动作 0 项，审计无不一致但稳定性结果失败。不能声称完成一小时。
7. 最终收集为 `collection-r02-15s-soak-03`。A／B／C 于 **06:54:38 UTC／
   14:54:38 上海时间** 停止，无故障规则、运行或排队任务，协调锁可获取；451 项
   构建输入再次匹配归档。汇总为 `checks/ordered-fixes/r02-15s-validation/final-summary.json`，
   机器交接为 `checks/current-handoff.json`。没有提交或发布。R02 仍有实际失败，
   但新的公网超时不等于已证明另一处客户端缺陷；下一步应分别评估当前网络条件下
   的期限策略、独立出口／Relay 对照及剩余完整验收。

## 2026-10-09 R02 十五秒超时链路续查交接

1. 沿用同源 `0c99f1f61292d458`；451 项本地／归档输入匹配。本轮仅增加临时诊断
   脚本、派生证据和文档，没有修改产品行为、安装制品或十五秒／五秒期限。诊断 helper
   在隔离 Linux 编译，只读本地状态和两种地址服务；独立 Relay 拨号使用新生成身份，
   不读取产品私钥。临时 example 已从 guest 构建源码移除。
2. 对 `r02-15s-matrix-02/0020` 逐个密文数据报关联：C→A 的 10 个写出均见于 A
   接收；A→C 的 16 个写出仅 5 个见于 C 接收，未匹配的 11 个中有 10 个短头包。
   A 收到首个 Initial 后约 1 ms 写出回复，后续收包／发包仍在活动，随后入站首帧
   等待五秒结束。C 的 normal 认证在总预算内等待 7,007 ms 后失败。不能把未匹配包
   称为永久丢包或 Welcome：日志能证明的是送达链路的缺口，不是加密帧内容或中间哪一跳。
3. `0042` 的首次 Relay 拨号在 TLS 阶段 5,003 ms 后失败，15 ms 后再次拨号；
   第二次 TLS 649 ms，TLS 完成至 WebSocket 升级／Relay 认证完成又等待 6,977 ms。
   从首次拨号至 Relay 就绪共 **12,644 ms**；QUIC 在请求 14,569 ms 就绪，normal
   认证仅余约 432 ms。第一项错误无类型，仍不能断言是五秒计时器触发。此拆分解释
   为什么共享十五秒生效后该请求仍会耗尽预算，没有证明应进一步延长期限。
4. `0024` 缓存查询五秒内接收计数不变，A 的三个关联写出包均未见 C 接收；稳定性
   准备 B／C／B 三次失败时，A 已报告 Online，但未见目标数据报接收。第一尝试 B
   的两个 Relay 分别约 831／2704 ms 注册完成，仍等不到目标回复。原现场未保存实际
   Home Relay URL 和同期地址查询结果，无法确定是旧地址还是已注册路径没有交付数据。
5. 新 `r02-timeout-path-01` 固定八项冷启动／首次打开 Relay 全通过、无阻塞／跳过，
   审计零不一致；成功 UI 耗时 3.065–10.116 秒。额外八次独立 Relay 拨号全成功，
   516–2969 ms。不得由这一轮通过宣布旧七项或一小时准备已修复；没有重做完整矩阵
   或一小时。观测请求可能预热解析缓存，因此这轮也不充当无诊断干预的验收基线。
6. 只读对照实测 A 已连接 `usw1-1`，Pkarr 同样返回 `usw1-1`，而 DNS 最初仍返回
   `aps1-1`，之后收敛。暂态旧 DNS 已证实；其导致旧三次准备失败的假设尚未证实。
   三端 TCP 443 头部／socket 队列捕获完整，均零内核丢包，但这次产品请求均成功。
   同期健康探测、地址查询或其他 TLS 流不能仅凭时间／包长归为产品 Relay 握手。
7. 证据在 `checks/ordered-fixes/r02-timeout-path/`：`0020-packet-joins.json`、
   `0024-packet-joins.json`、`0042-packet-joins.json` 保留跨端关联及原文件哈希；
   `run-state.json` 保留逐轮实际 Relay／DNS／Pkarr 对照；`final-summary.json`
   汇总结论与边界。日志收集为 `collection-r02-timeout-path-01`。A／B／C 于
   **07:20:09 UTC／15:20:09 上海时间** 停止，协调锁已释放，无故障规则或活动任务。
8. 下一步要把“链路未送达”定位到具体边界，需要失败同次的 Relay 错误类型，以及
   guest／出口代理外层／服务端时序对照；无主机收包的失败还需同期实际 Relay 与两种
   地址来源。继续保持大陆延迟前提、慢而成功计通过。R02 和完整验收仍开放。


## 2026-10-09 R03 同步停滞续修交接

1. 本轮运行时改动只有 P32 的诊断额度分配：原每秒 100 条总额中，逐包事件最多
   80 条，其余阶段可使用全部总额；队列、非阻塞生产者和显式 15 分钟窗口保留。
   T31 要求短断网后的 Android 显示尺寸与请求匹配、Active 且 inputReady，再执行
   后续输入断言，所有检查仍共用原 60 秒期限。产品 15 秒首次请求／5 秒就绪操作、
   QUIC 计时器和终端同步状态机未修改。
2. 新增 [原生同步回归](../../crates/android/src/terminal/sync_tests.rs)，通过真实
   SessionClient、共享 TerminalView 驱动和 Android 原生状态处理，覆盖 17 个排队
   快照、同步中合并第二次 resize、最终尺寸、原 input epoch 和恢复后的实际输入发送。
   传输为注入的解码帧，不能替代真实 QUIC 或 Android UI；原主机真实流背压、输出、
   延迟重复 ACK／连续 resize 的八轮回归也通过。这两项没有复现产品状态机缺陷。
3. P32 三项额度回归旧逻辑两项失败、一项边界通过；新逻辑全部通过。T31 两种尺寸
   滞后分支旧逻辑均失败，修正后通过，审计拒绝缺失或错误显示尺寸、非 Active 和
   inputReady=false。Linux 诊断库 12 项、Android Rust 23 项、主机同步八轮通过；
   Python 运行器及主机准备 49 项通过。Mac 定向和 Linux 工作区 Clippy、Android
   assemble／lint／JVM、格式、源码策略及秘密扫描通过。首次测试夹具调用不存在的
   builder、系统 Python 缺少 pyte、一次单测筛选误传给独立黑盒程序的失败均保留；
   纠正测试入口后通过，不计为产品行为失败。
4. 同源归档 `source-4366eab617aed7cc.tar.gz`，452 项本地／归档构建输入一致，
   A／B 二进制和 C APK 的安装哈希通过预检。定向结果如下；所有失败保留，审计均
   零不一致，未执行完整核心／扩展矩阵或一小时稳定性：

   | 运行 | 制品 | 结果 | 有效覆盖及限制 |
   | --- | --- | --- | --- |
   | `r03-sync-baseline-01` | `0c99f1f6` | 14 通过／6 失败 | A／B→C Relay；六项初连失败均未注入断网；成功恢复 3.715–37.600 秒 |
   | `r03-sync-viewport-02` | `0c99f1f6` | 10/10 通过 | B→C Relay，新增显示尺寸契约；恢复 2.590–5.858 秒，但只有三项保留完整阶段，触发 P32 |
   | `r03-sync-phases-03` | `4366eab6` | 8 通过／12 失败 | B→C Relay；十项初连失败、两项 R03；成功恢复 1.705–23.656 秒，八项均有完整快照链 |
   | `r03-sync-direct-04` | `4366eab6` | 10/10 通过 | B→C Direct，同样的三秒双向丢包、真实输出和 resize；恢复 0.245–4.393 秒，十项均有完整链 |

5. `phases-03/0003` 和 `/0011` 均再次出现主机 101×31、Android 65×48，
   synchronizing、inputReady=true，超过 60 秒，未发送后续验证输入。主机分别在
   resize 后 15／14 ms 写出 revision 68／92 的快照，客户端未记录对应 SnapshotReceived／
   SnapshotApplied，主机也无对应 ACK。两个失败窗口都没有记录丢失／抑制报告。
   同期成功 Relay 样本中，客户端收到快照到 ACK 写入仅 2–11 ms；这不证明任意网络
   情况下均能恢复，但没有依据将此次停滞归为 Android 已收到完整快照后的应用死锁。
6. 按主机进程内序号选取快照 flush 之后的传输写出：`0003` 的 33 个数据报在客户端
   均无匹配接收；`0011` 的 164 个只有 78 个有匹配接收。部分早先写出的包仍继续到达，
   `0003` 还记录了多次 Relay 重拨及一次拨号失败。两端都有恢复后的传输活动，不能
   将移除三秒故障规则等同于公共链路已恢复可用。指纹只关联同一密文数据报，不能
   识别其为快照、ACK 或哪一应用帧；未匹配也不等于已证明永久丢包。没有跨设备时钟
   相减，没有同次 TCP 边界捕获，尚不能唯一归因客户端传输、代理／运营商或 Relay。
7. 原始矩阵、截图、双向丢包计数和日志保持不变。派生证据在
   `checks/ordered-fixes/r03-sync/`：`matrix-analysis.json`、`0003-packet-joins.json`、
   `0011-packet-joins.json`、`retained-packet-summary.json`、`post-source-guard.json`，
   均保留来源或哈希；`final-summary.json` 和 `checks/current-handoff.json` 汇总交接。
   最终收集为 `collection-r03-sync-final`，六向预检通过。A／B／C 于
   **09:33:44 UTC／17:33:44 上海时间** 停止，无故障规则残留或运行中的实验任务。
8. **R03 仍开放，但已获得两次新的失败阶段证据。** 下一步应捕获同次 Relay 断开／
   拨号错误的封闭类型、当前 Relay 路由及 TCP 送达边界，并构造对应的受控失败，
   再决定是否存在需要修改的客户端恢复逻辑。不能用盲目 resync、重建会话或延长期限
   把尚未定位的传输等待掩盖为通过。R02 及完整验收同样保持开放；没有提交或发布。


## 2026-10-09 R03 文件句柄续修交接

1. 先追加 D05 的封闭 Relay 错误类别，区分拨号总期限、连接器超时、TCP／DNS／
   代理、TLS、升级、认证，以及 ping／发送超时、关闭和协议错误；区分收到首个
   pong 前后的断开，不记录错误原文、地址或内容。Linux 诊断 13 项、Android Rust
   23 项、主机同步八轮和 vendor 错误映射回归通过；Mac 定向／Linux 工作区／vendor
   Clippy 通过。没有改变重试、队列、同步状态机或网络期限。
2. 同源 `0e513630208f8983` 的 `r03-relay-cause-01` 为 **19/20**，审计零不一致。
   19 次恢复为 1.147–3.339 秒，均有完整同步链；其中一次经历 ping 超时、连接器
   拨号超时仍成功恢复，不能将一次 ping 超时直接等同于 R03。B／C 同次 TCP 443
   头部与 socket 队列捕获完整，均零内核捕获丢包；不包含载荷／pcap，也不代表出口
   代理外层或 Relay 服务端的观测。
3. 唯一失败 `0016`：主机在 resize 后 7 ms 写出 revision 60，随后收到 ACK；
   Android 详细日志停止、写状态文件报 `EMFILE`，instrumentation 退出，截图显示
   桌面。原运行器一直读取最后的 65×48／synchronizing 状态，最终报恢复超时。
   `fd-cause-evidence.json` 保留 ACK、异常、原记录哈希及分类；原失败不改写。
4. P33 修复 `Os.open` 描述符被 Java 流借用而未关闭的所有权错误，覆盖追加、
   规范化、控制读取、flush、导出和部分收集失败。三个新增 Android 回归旧逻辑
   分别遗留 775／32／8 个句柄，修正后无增长，完整诊断 **7/7**。首版计数夹具
   没有处理 `/data/user/0` 与 `/data/data` 别名，误判通过；修正后的真实失败在
   `fd-before-02.log`，全部原日志保留。T32 对退出码 0／1 均拒绝旧状态，修正前
   两个子例失败，修正后运行器及本地主机准备 **50/50**。
5. 最终同源 `8d47006ce307211d` 的 452 项本地／归档构建输入一致，运行器、
   测试和审计脚本也与归档一致，三端安装哈希及六向预检通过。相对上一诊断候选
   只新增 Android 诊断实现及其测试两个构建输入，Linux 二进制哈希不变；前项
   Rust 检查可据同源输入沿用，Android assemble／lint／JVM 和设备回归重新通过。
   格式、源码策略和秘密扫描通过，未执行完整工作区测试或全矩阵。
6. `r03-relay-fd-fixed-02` **40/40 通过**，无阻塞、跳过或运行事件，审计零问题；
   恢复 0.422–4.536 秒，40 项均有完整快照接收／应用／ACK 链。路径配置启动时
   会按原流程重启一次 Android，之后全部 40 项为同一进程；80 次描述符观察中
   诊断文件句柄均为零，总数 156–166，没有随日志增长。两端 TCP 捕获完整且零
   内核捕获丢包。公网前后通过率不充当因果证明，资源泄漏的因果证据来自第 4 项。
7. `matrix-analysis.json`、`fd-cause-evidence.json`、两轮 `experiment.json`、
   `post-source-guard.json` 和 `final-summary.json` 保留结果和来源；机器交接为
   `checks/current-handoff.json`。收集 `collection-r03-relay-cause-final` 后再次预检，
   A／B／C 于 **11:36:16 UTC／19:36:16 上海时间** 停止，锁可获取、故障日志为空，
   无后台实验。启动后的只读状态 helper 一度不存在，前几次观察保留错误，随后
   从已验证制品恢复；这轮没有额外 DNS／Relay 拨号探针。
8. **P33、T32 已修正，历史 R03 送达缺口仍未闭环。** 旧 `0003`、`0011` 的真实
   终端截图、持续传输记录与本次测试器退出不同，缺少同期描述符数量，不能宣称
   同因。下次若在已修 P33 的版本重现，应把封闭 Relay 错误、稳定句柄数、同次
   TCP 和 QUIC 流进度对齐，再决定客户端恢复修改。R02、完整核心／扩展矩阵及
   一小时保持开放，没有提交或发布。


## 2026-10-09 R03 流进度续查交接

1. 本轮新增 D05 流进度，未修改产品恢复行为。[IrohSessionIo](../../crates/client/src/iroh_controller.rs)
   在原读取循环和一秒静默唤醒中，最多每秒记录一次已读有序字节、解码帧数、队列
   长度、未完整帧字节数，以及连接级 QUIC STREAM 计数／收发量／RTT。只在原显式
   900 秒详细窗口生效，没有额外任务、探测请求或内容字段。连接级 STREAM 计数包含
   其他流和重传，不能当作本流送达／ACK 数；没有样本也不能推断流一定停止。
2. 新增[真实 QUIC 回归](../../crates/client/src/iroh_controller/progress_tests.rs)，
   使用内存网络和真实 TLS／QUIC 传送 36,894 字节合法帧，分别停在两字节长度前缀、
   1,026 字节部分帧，取消两次读取后继续发送命令和接收，验证解码与最终帧完整。
   EOF 检查发现本轮诊断初稿依赖 QUIC 已回收的接收状态，累计读量会归零；
   `counter-before-01.log` 失败，改为适配器自有单调计数后通过。它是新增诊断的
   实现修正，不是历史 R03 根因。另有诊断嵌套未知字段／文本计数字段拒绝回归。
3. 四轮均保留原期限和结果，审计零不一致；成功项都有完整快照接收／应用／ACK 链。
   以下为不同制品的定向证据，不能合并成同一候选的验收通过率：

   | 运行 | 制品 | 结果 | 有效恢复耗时 |
   | --- | --- | --- | --- |
   | `r03-stream-baseline-01`，B→C Relay | `8d47006c`，上轮 P33 | 20 通过 | 0.966–4.313 秒 |
   | `r03-stream-observed-02`，A／B→C Relay | `e3d8803b`，流进度初版 | 15 通过／4 初连失败／1 阻塞 | 2.199–23.543 秒 |
   | `r03-stream-warm-03`，B→C Relay | `e3d8803b` | 4 通过／5 初连失败／1 阻塞 | 24.046–48.599 秒 |
   | `r03-stream-final-04`，B→C Relay | `95015f6a`，含 EOF 计数修正 | 2 通过 | 2.647–3.890 秒 |

   九次失败全在 connection 阶段、尚未注入断网，不是九次恢复失败。`observed-02/0001`
   的 QUIC 到 10,825 ms 才就绪，认证剩 4,175 ms，最终用尽十五秒预算；R02 仍开放。
   两次阻塞入口丢包均为零、出口分别 96／41，保留原 blocked，不能计作有效双向覆盖。
   `warm-03/0001` 的结果冻结后另做 15 秒只读观察，仍见旧尺寸／synchronizing；
   这段补充日志没有延长产品期限或改写结果，不能称为新的 60 秒恢复失败。
4. `observed-02/0012` 恢复 23.543 秒：主机 resize→flush 为 7 ms、flush→ACK
   22,285 ms，客户端完整接收→应用并写 ACK 为 2 ms；窗口内无记录丢失或 Relay
   错误。`warm-03/0005` 恢复 48.599 秒，三个同进程耗时分别为 **9／45,606／2 ms**。
   后者原连接／流的 47 次采样中有序读量 2,173→120,160 字节、解码 1→64 帧；
   一段约八秒保持在 105,121 字节／54 帧／629 字节部分帧，随后继续。期间两次
   `ping_timeout`、没有 records_lost，路径 RTT 估计峰值 11.171 秒。该样本主要
   等待位于完整快照接收之前，不支持“完整快照已收到后应用处理卡住”的解释。
   没有跨设备时钟相减，也不能由字节数推断每个密文包具体承载哪一应用帧。
5. 同次 Android TCP 443 观察仅筛选目标进程所属套接字，385 次样本 RTT 为
   2.095–436.580 ms，接收队列最大零、发送队列最大 239 字节。这是 guest 的
   本地 TCP 段，可能终止于透明转发层，不能当作出口外层或 Relay 服务端送达证据。
   四轮九份 TCP 头部捕获都有完整结束统计、零内核捕获丢包；不保存载荷／pcap。
   104 次 FD 采样诊断句柄均为零，各轮总量分别 157–175、160–165、159–167、
   162–164。A／B 交替轮会按既有路径配置重启进程；固定 B 轮在配置后连续使用
   同一 Android 进程，不能将全部 104 次说成一次进程的连续资源观察。
6. Linux 隔离探针覆盖真实 TLS Relay 的三秒 TCP 双向暂停及断连重拨，两者都在
   原流完成字节／ACK 校验，重拨时 TCP 连接数 2→3。另以真实 QUIC 的三秒单向
   延迟（约六秒 RTT）叠加三秒双向丢包，24 KiB／128 KiB 积压分别约 21.883／
   35.844 秒恢复，仍在原六十秒内。这些是合成字节的传输实验，不是终端 UI
   复现；暂存测试模块每轮均恢复原文件，未改生产 QUIC／Relay 计时器。它们说明
   这些条件下原流能恢复，不能据此排除所有客户端问题或唯一解释旧公网失败。
7. 最终归档 `source-95015f6ac5408a9b.tar.gz`，453 项构建输入／运行器一致，三端
   安装哈希和预检通过。Linux client 114 通过／1 忽略、proto 25＋17、diagnostics
   14、Android Rust 23，共 193 通过／1 忽略；Mac 定向回归、Mac 定向／Linux
   工作区 Clippy、Android assemble／lint／JVM、实际 DiagnosticsTest 7/7、格式、
   策略和秘密扫描通过。最初新回归的 unwrap lint 失败及 EOF 失败日志均保留；
   修正后检查通过。运行器源码未变，本轮未重跑此前 50 项，也未运行完整工作区
   单测、核心 1460 项、扩展 88 项或一小时稳定性。
8. 证据根 `checks/ordered-fixes/r03-stream-progress/` 的 `matrix-analysis.json`、
   `stream-analysis.json`、`investigation.json`、`slow-case-tcp.json` 保留观察和来源，
   `controlled*.json` 保留隔离探针及源码恢复状态，`post-source-guard.json` 和
   `final-summary.json` 汇总最终制品与边界。最终收集 `collection-r03-stream-progress-final`
   后六向预检通过，A／B／C 于 **13:58:21 UTC／21:58:21 上海时间** 停止，锁可获取、
   故障日志为空、无后台实验。**历史 R03 两次超过六十秒的停滞仍开放**；下一步需要
   同类失败窗口的流进度和出口外层／Relay 边界对照，才能决定具体客户端修正。
   本轮保留十五秒／五秒及六十秒期限，没有新增已证明的传输行为修复，没有提交或发布。

## 2026-10-10 R03 Relay 收发饥饿修复交接

1. P34 修复 [Relay actor](../../vendor/iroh/src/socket/transports/relay/actor.rs) 的就绪方向
   饥饿：外层 `biased select` 先选发送，内层又仅在写入阻塞时接收，持续可写会令
   已就绪数据和 pong 一直等待。现让收发方向轮流优先；关闭、优先／普通控制和
   存活检查仍在它们之前。没有额外队列、任务、探测、请求重放或期限修改。
2. 三项[回归](../../vendor/iroh/src/socket/transports/relay/actor/fairness_tests.rs) 驱动
   真实 actor、类型化 Relay 消息和即时就绪 I/O。十批发送期间必须消费接收数据和
   匹配的健康检查 pong；接收持续就绪也必须允许发送，预先取消必须保持最高优先。
   修正前两项失败、一项取消边界通过；修正后三项通过。pong 检查在末批注入发送
   错误来验证已确认存活的错误分类，没有真的等待 ping 计时器超时。最初提取夹具的
   类型错误，以及未显式触发健康检查的首版 pong 夹具失败都保留；最终前后对照为
   `fairness-before-04.log`／`fairness-after-03.log`，原循环对照后源码已恢复。
3. 同源 `source-723cb5966555da18.tar.gz`，454 项构建输入／运行器一致；相对上轮只改
   Relay actor 和新增回归两个构建输入，三端安装哈希和预检通过。Linux 完整
   工作区 **812 通过／9 忽略**、Iroh **156 通过／1 忽略**，Mac 无 socket Iroh
   **26 通过**；Mac／Linux 工作区及 vendor Clippy、Android assemble／lint／JVM、
   格式、源码策略和秘密扫描通过。独立终端 black-box 门禁仍为显式跳过，运行器
   源码未变，没有重跑此前 50 项 Python 测试，也没有重做 Android DiagnosticsTest。
4. 定向结果单列，三轮审计零问题；所有通过项都有完整 resize／快照写出／收到／
   应用／ACK 链。没有修改慢而成功计通过的规则：

   | 运行 | 制品 | 结果 | 有效恢复耗时 |
   | --- | --- | --- | --- |
   | `r03-backpressure-baseline-01`，B→C Relay | `95015f6a` | 12/12 | 1.669–5.505 秒 |
   | `r03-backpressure-fixed-02`，B→C Relay | `723cb596` | 20/20 | 1.329–8.471 秒 |
   | `r03-backpressure-direct-03`，B→C Direct | `723cb596` | 4/4 | 0.420–0.916 秒 |

5. 新 Relay 轮 Android 的 40 次句柄采样中，诊断文件句柄始终为零，总句柄为
   157–172。其中 39 次属于配置后的同一进程，另一次发生在配置重启前；完整分布
   保存在 `stream-analysis.json`。
   三轮流进度没有计数回退；每轮 B／C 的 TCP 443 头部捕获均完整结束、零内核捕获
   丢包，不含载荷／pcap。Direct 的终端路径由产品路径快照证明，443 捕获不能代表
   UDP 终端数据。guest TCP 仍不等于出口代理外层或 Relay 服务端的边界观测。
   详细日志的丢失／抑制报告分别为基线 12 条、新 Relay 22 条、Direct 3 条。
   同步链虽完整，逐包日志并非完整，不能把日志中缺少的数据报直接判为链路未送达。
6. **P34 已有受控因果证据，历史 R03 尚未闭环。** 公网基线本来就 12/12，修正后
   24 项也没有重现超过六十秒的停滞，因此不作前后通过率的因果结论。旧 `0003`、
   `0011` 缺少“接收已经就绪且发送持续占先”的观测，不能用新发现覆盖旧结论。
   下一步若重现，应对齐流进度、Relay 实际队列活动和出口外层／服务端送达边界；
   不以额外 resync、重建会话或延长期限替代定位。
7. `matrix-analysis.json`、`stream-analysis.json`、`post-source-guard.json` 和
   `final-summary.json` 保留来源摘要与结论，机器交接为 `checks/current-handoff.json`。
   最终收集 `collection-r03-relay-backpressure-final` 后再次预检，A／B／C 于
   **02:10:32 UTC／10:10:32 上海时间** 停止，锁可获取、故障日志为空，无后台实验。
   R02、完整核心 1460 项／扩展 88 项及一小时仍开放；没有提交或发布。


## 2026-10-10 R03 有限验收失败交接

1. 用户确认有限关闭门槛：同源四组合各至少十次有效短断网恢复，以及同一 B→C Relay
   会话连续一小时的短断网／输出／resize 压力验证，覆盖输出积压和 Relay 重连；
   保持六十秒、原会话和连续性断言。历史失败不要求逐次唯一归因；本轮按冻结的
   `checks/ordered-fixes/r03-acceptance/plan.json` 执行，不能在看到结果后放宽门槛。
2. 继续使用 `source-723cb5966555da18.tar.gz`，454 项构建输入一致，首尾安装哈希和
   六向预检通过，没有产品代码、请求预算、传输计时器或终端状态机改动。已有 P34
   三项回归和平台检查仍由前轮同源证据支持，本轮没有重跑完整 Rust／Android 构建。
3. 普通短断网补充结果如下。既有同源 B Relay 20/20、B Direct 4/4 保留原目录；
   加上本轮 26 项，四种组合为 A Direct 10、A Relay 10、B Direct 10、B Relay 20，
   **合计 50/50**。初连失败、恢复失败、阻塞均为零；三轮标准审计零问题。

   | 本轮运行 | 结果 | 有效恢复耗时 |
   | --- | --- | --- |
   | `r03-acceptance-a-direct-01` | 10/10 | 0.431–4.142 秒 |
   | `r03-acceptance-b-direct-02` | 6/6 | 0.926–3.321 秒 |
   | `r03-acceptance-a-relay-03` | 10/10 | 0.978–4.394 秒 |

4. 定向一小时使用独立编排脚本，调用原 `Suite` 的真实输入、故障、连续性和收集助手。
   计划每分钟一次、共 60 次，单次八秒内输出 80 KiB 可打印合成文本，三秒双向丢包中
   resize；每十次精确中断一次 B 测试 UID、已核对 home Relay 地址的 TCP socket。
   第一轮确实中断并建立了新 socket，daemon PID 不变。此夹具先做只读校验，修正了
   Relay DNS 名末尾点的匹配问题；校验阶段未关闭任何 socket。
5. `r03-acceptance-relay-hour-04` 在 **245.298 秒**处停止：**3 通过、1 失败、
   0 阻塞、56 未执行**。成功恢复为 10.510／12.938／7.355 秒，均保留同一 Session、
   Shell PID、cwd、变量和后台 PID 的实际输入证明。第 4 次未额外中断 socket：
   双向实际 DROP 为出站 32、入站 20，输出在断网期间由 1,024 增至 36,864 字节，
   最终写出 81,920 字节。超过原六十秒后仍 synchronizing、inputReady=true，
   显示 101×31，目标为 102×32；没有发送该次恢复后的输入证明，也没有重试该动作。
   该次 65.294 秒为包含注入和证据收集的用例总时长，不冒充精确恢复耗时。
6. 失败阶段：B 进程的 resize／snapshot 为 revision 375，**6 ms** 后 snapshot_sent，
   没有记录该 revision 的 ACK。C 在请求 resize 后的 61 个流采样中，read_bytes 从
   **1,670,244** 增至 **2,228,907**，decoded_frames 从 **204** 增至 **278**，
   其中 54 个采样存在未完成帧；未记录 102×32 的完整 SnapshotReceived／Applied。
   同一 C 进程记录两次 Relay stream_read 断开和一次 ping_timeout。没有发现本轮
   EMFILE／instrumentation 退出；计时期间仅 7 次 input、4 次 resize，没有 connect、
   select、home 或替换会话。此观察支持继续检查快照前的传输／排队等待，不证明某一
   外部节点或 P34 是历史根因；80 KiB 压力也高于普通短断网用例的输出量。
7. 相邻成功收集之后、相同 B／C 进程的观察窗口分别有 **17／5** 条诊断丢失或抑制
   报告，合计计数 1,395／93；原始流采样、同步阶段和摘要在 `failure-analysis.json`。
   没有同次 TCP 头部／出口代理／服务端捕获，不能由缺失的逐包记录断言永久丢失。
   原标准审计对前三项各报告一条“每行主机预检缺失”：补充脚本继承了独立矩阵的
   manifest 标记，而连续动作只在计时前准备一次主机。原 manifest 和三条报告均保留；
   `acceptance-analysis.json` 独立核对初始 Online、连续性、命令区间和原期限，证据
   审查无额外问题，产品验收仍明确失败。该元数据不一致没有造成第 4 次超时。
8. **本轮不能关闭 R03，且已有当前候选的新失败证据。** 不继续重跑直到碰到通过；
   先用保留的有界输出／断网夹具追查 revision 375 之前的流进度与 Relay 重拨关系，
   修改后再按相同关闭条件验收。同一会话一小时尚未通过，完整三端验收也未通过。
   最终收集 `collection-r03-acceptance-final`、预检和停止全部成功；A／B／C 于
   **02:38:55 UTC／10:38:55 上海时间**停止，**02:40:20 UTC** 独立确认均已停止、
   无故障规则残留、协调锁可获取。没有提交或发布。机器交接为 `checks/current-handoff.json`，
   本轮汇总为 `checks/ordered-fixes/r03-acceptance/final-summary.json`。


## 2026-10-10 R03 画面帧窗口修复与定向验收交接

1. 上轮失败 `r03-acceptance-relay-hour-04/0003` 的定位：新增只用客户端时钟的逐次统计
   `backlog-analysis.py`，对每次 resize 请求到对应完整快照之间读到的帧计数。Relay 各轮中位数
   41–53 帧、Direct 3–4 帧；失败那次请求后读 74 帧／560,075 字节仍无快照。结合主机 6 ms
   写出快照，确认等待在主机已写、客户端未读的有序流积压上。因主机未记录该快照写出时的流偏移，
   当时的确切积压量是推断值；样本也包含主机收到 resize 前写出的帧。
2. P35 在 [session_wire.rs](../../crates/daemon/src/session_wire.rs) 限制已写未回执的实时
   画面帧，[客户端](../../crates/client/src/session.rs) 与
   [视图驱动](../../crates/client/src/view.rs) 在可取消读取之外发送累计回执；协议见
   [terminal.proto](../../proto/zterm/v2/terminal.proto) 与
   [持久会话说明](../persistent-sessions.md)。显式协商、旧端兼容、回执不重放、本地附件不启用。
3. 受控复现、三项主机回归和一项客户端驱动回归的前后对照见 P35。首版复现夹具和客户端夹具
   错误的日志保留，不计作产品行为。
4. 候选 `source-2d86a806051a724c.tar.gz`：Linux 二进制 `ce9dd29d…`、调试 APK `319d11f1…`，
   测试 APK 未变；454 项输入首尾一致。平台检查见 `candidate/validation.json` 与各日志。
5. 冻结计划 `acceptance/plan.json` 沿用用户确认的门槛，不迁移旧制品结果：

   | 运行 | 结果 | 有效恢复耗时 |
   | --- | --- | --- |
   | `r03-window-b-relay-01` | 10/10 | 2.582–9.339 秒 |
   | `r03-window-a-relay-02` | 10/10 | 1.815–4.346 秒 |
   | `r03-window-a-direct-03` | 10/10 | 0.275–0.816 秒 |
   | `r03-window-b-direct-04` | 9 通过／1 初连失败（R02） | 0.273–2.183 秒 |
   | `r03-window-b-direct-05` | 10/10 | 0.401–0.592 秒 |
   | `r03-window-relay-hour-06`，同会话一小时 | 3601.2 秒，60/60，6 次 Relay 重连 | 1.723–13.526 秒 |

   `-04/0000` 在打开主机时 `deadline_exceeded`，没有故障注入目录，原失败保留；它不影响
   R03 判定，但说明 R02 仍在。一小时运行的标准审计有 60 条“每行主机预检缺失”，与上轮相同，
   来自连续动作共用一次初始准备；补充审计独立核对初始 Online、原会话和连续性证明（62 次）。
6. 修复后逐次统计（仅取新 APK 安装后的 resize）：Relay 中位数 6 帧、最多 11 帧／17 KB；
   Direct 中位数 2–4 帧。见 `backlog-after-summary.json`。这是现场观测，不替代受控因果对照。
7. 最终收集 `collection-r03-frame-window-final`、预检和停止成功，A／B／C 于 **07:28:27 UTC**
   停止，07:29:47 UTC 独立确认无故障规则残留、协调锁可获取。没有提交或发布。
   汇总为 `checks/ordered-fixes/r03-frame-window/final-summary.json`，机器交接为
   `checks/current-handoff.json`。
8. **R03 定向验收通过，关闭。** 更早的 `r03-sync-phases-03/0003`、`/0011` 只有送达缺口证据，
   未保存同类积压观测，不逐个宣称同因。R02 初连超时、完整核心 1460 项、扩展 88 项与完整三端
   稳定性仍开放；后续保持原期限，不重放不确定变更。

## 2026-10-11 R02 新连接选中路径修复交接

1. 从 `origin/main`（`2020d2a`）新建分支 `investigate/r02-stale-relay-route`。本机已没有
   历史证据目录，本轮不再分析旧运行，只做受控复现。
2. 最初假设“DNS 旧 Relay 先返回、`fresh_relay_hints` 只取第一个结果”被对照否定为主因；
   受控复现定位到 P36：同一对端旧连接未关闭时，新连接握手被钉在旧选中路径上。
3. 改动：[remote_state.rs](../../vendor/iroh/src/socket/remote_map/remote_state.rs)、
   [discovery_tests.rs](../../vendor/iroh/src/socket/remote_map/remote_state/discovery_tests.rs)、
   [vendor 说明](../../vendor/iroh/ZTERM.md)、客户端
   [iroh_controller.rs](../../crates/client/src/iroh_controller.rs) 的三项 `moved_host_` 回归。
   原 P31 测试“选中路径后停止补发”的断言与新规则冲突，已改为“另一连接的选中路径不终止补发、
   查询结束才停止”。期限、计时器和产品请求语义未改。
4. 已通过（Mac）：`moved_host_` 3/3 且连跑三次一致（修正前 1/3）、`just ci-iroh` 28/28（修正前 25/28）、
   zterm-client／zterm-daemon 429 通过／0 失败／12 忽略、vendor Iroh Clippy 与格式、`just android-check`。
   `just check` 全部测试 1280 通过／0 失败／16 忽略，依赖策略通过；末尾 `relay-static` 的
   `verify-upstream.sh` 首次因 curl 连接 github.com 出现 SSL_ERROR_SYSCALL 失败，原日志保留，
   单独重跑该 recipe 及其后的 Compose 静态检查均通过。证据在
   `checks/ordered-fixes/moved-host-handshake/`（`product-before-01.log`、`product-after-0{1,2,3}.log`、
   `vendor-before-01.log`、`vendor-after-01.log`、`just-check-01.log`、`relay-static-02.log` 等）。
5. 未执行：Linux 完整工作区与 Linux Iroh 全库、A／B／C 同源构建与矩阵。没有提交或发布。
   本地保留 P36 补丁，同时准备向上游提 issue。不依赖 zterm 代码的纯 Iroh 复现：上游逻辑下
   新 `connect()` 在 10 秒期限内未完成，放宽到 60 秒时约 30 秒自行失败（握手 idle），即旧连接
   存活期间新连接无法成功；补丁后约 5–7 ms 连上。维护者审阅并确认上游 AI 政策要求的声明后，
   已提交为 [n0-computer/iroh#4600](https://github.com/n0-computer/iroh/issues/4600)，
   草稿原文为同目录 `upstream-issue-draft.md`。上游修复发布前保留本地补丁。
6. 反向假设已否定：客户端（同一身份）被强杀、主机仍保留旧 Direct 选中路径时，新进程仅经
   Relay 拨号。纯 Iroh 探针在有无 P36 时均约 10 ms 连上，4 秒内 40 次往返最慢约 9 ms，新连接
   打洞到新端口，主机旧选中路径没有影响新连接。因此 `0020` 的主机→客户端缺包不能由该机制
   解释。探针为临时代码，未保留。PR [#70](https://github.com/leonfox28/zterm/pull/70) 的 CI 全部通过。
7. 下一步：补跑上述检查后，在三端针对 P36 前提设计定向行（主机强杀或重启到另一 Relay／端口时，
   控制端仍持有旧连接再拨号；A／B 互为控制端），并为 R02 冷启动失败继续保留同次 Relay 错误类型、
   实际 Home Relay 与两种地址来源的现场证据。R01 中“主机换 Relay 或端口后旧连接收不到 P27 重置、
   首个查询只能超时”只是候选解释，尚未验证。

## 2026-10-11 三端重建与 R02 续查交接

1. 实验根目录移出 `target/`，全新重建 A／B／C（环境 `zterm-md-20261011t084006z`），过程见
   [环境重建记录](multi-device-lab.md)。修正测试器 T33（新装应用缺 `files/` 导致 C 配对失败）
   和 T34（桥先写 `ready` 后发布状态导致 `'hosts'` KeyError）。
2. 首轮 cold 8/8 通过，但两行 Linux 冷启动 12.3／12.6 秒：所需 Relay 的首次 TLS 十秒无进展，
   以 `dial_deadline` 结束后重拨 0.47 秒即连上。据此实现 P37（3 秒对冲拨号）：受控 socket
   回归 10.09 秒→3.15–3.16 秒，`just check` 1286 通过、`just android-check` 通过。
3. P37 同源 cold ×3 两轮：`p37-cold-01` 21/24（其中 1 项为 T34 测试器问题），`p37-cold-02`
   23/24，审计均零问题。对冲在现场触发并救回过单连接卡死；剩余 R02 失败都与同时刻的
   Relay 路径故障吻合：某 Relay 成簇不可达（两次尝试都无进展）、连接建立后不转发数据、
   主机重启后 home relay 在 27 秒内被改选。
4. 只读探针（Mac 与 VM 同时）：aps1-1 约 9% 失败且有约 1.5 分钟成簇中断，euc1-1／use1-1
   零星超时，usw1-1 无失败；Mac 与 VM 同时出现，属于共享出口路径。A、B、C 的 home relay
   均为 `euc1-1`。
5. 分支：P36 在 PR #70（未合并）；`test/rebuild-multi-device-lab` 与 `fix/r02-hedged-relay-dial`
   依次叠在其上，均未推送。A／B／C 保持运行；旧 VM 与悬空 AVD 记录待确认后删除。
6. 下一步需要选择方向：在可靠路径上自建 Relay（产品已支持 SelfHosted 配置）以隔离代理影响；
   评估主机重启后 home relay 改选的抑制；或主机同时保持备用 Relay 并发布。期限与计时器保持不变。
