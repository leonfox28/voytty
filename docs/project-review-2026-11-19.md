# voytty 项目深度审查报告

- **审查基准**：`main` 分支 `9ad6b81`（v1.0.3，PR #115 合入后），Rust `1.98.0` / Edition `2024`
- **审查日期**：2026-11-19
- **代码规模**：`crates/` 约 `127,390` 行 Rust（8 个工作区 crate + `tools/release`），以及 3 个本地补丁依赖（`vendor/noq-proto`、`vendor/iroh`、`vendor/portable-pty`）、安装脚本、Relay 部署与 CI/发布自动化
- **审查维度**：
  1. 安全与信任边界（配对与加密协议、本地 IPC / OS 权限、上传、自更新与终端安全策略）
  2. 守护进程并发、生命周期、Wire 协议与架构债务
  3. CLI、共享移动端/桌面客户端（`voytty-client`）、主机终端引擎、平台/PTY 与诊断日志
  4. 构建/CI、供应链、发布流水线、安装脚本、Relay 部署、补丁维护与文档一致性

> [!IMPORTANT]
> **证据口径说明**：本文档各条目区分三类依据——（1）**源码静态核对**（原始发现的路径与行号核对自 `9ad6b81`）；（2）**本地只读/实测观测**（下表本地基线门禁、`H16` 直接调用 `path_plan` 实测、既有 `normal_confirmation` 用例等）；（3）**尚待运行实测**（如 `C2` 的运行态触发与跨平台持续时长，以及标注为 `HOSTED-ONLY` 的托管环境验证边界），各条目内均按此口径如实标注。
>
> **2026-11-19 复核修订说明**：经与同工作区 Codex 按 `9ad6b81` 交叉复核，本文档保留全部原始编号与交叉引用，并按现行共识同步调整了正文位置、等级与路线图：
> - **已撤回（误报）**：`M23`、`H4` 中 `AcceptCell::wait_until` 子项、`M14`（详见[误报与修订核查](project-review-2026-11-19-false-positives.md)）。
> - **保留原编号并调级**：`C2`（Critical → **High / P1**）、`C3`（Critical → **High / 优先处理**）、`M1`（Medium → **Low / P2**）。
> - **实测与证据口径补充**：`C1` 明确公开追踪与扫描豁免已证实、令牌当前有效性未知；`H16` 经直接运行 `path_plan` 实测确认两处发布源文件均为 `native=false` 并列为 **P1**。

## P0 修复记录

2026-11-19，`fix/project-review-p0` 分支完成了本轮选定的修复。本轮代码验证截至 `02857b8`。后文的问题数量和原始发现均以 `9ad6b81` 为基准。

| 编号 | 状态与提交 | 修复结果及验证 |
| --- | --- | --- |
| C4 | 已修复 · `1961dc8` | 单按 `Esc` 后，即使没有下一次输入，40 ms 的等待窗口结束时也会送出按键。测试覆盖了紧随其后的鼠标移动、翻页键、分段到达的转义序列和跨输入轮次的按键。[输入回归测试](../crates/cli/src/terminal_ui/desktop_tests.rs#L2894) |
| C5 | 已修复 · `3f95d3e` | 生成的 systemd 用户单元使用 `WorkingDirectory=%h`。在 ARM64 Linux 虚拟机上，实际生成的单元通过了 `systemd-analyze --user verify`。已有安装可再次运行 `voytty daemon autostart enable` 更新旧单元。[自动启动说明](autostart.md#L28) |
| M21 | 部分修复 · `3f95d3e` | Linux 实测接受属主独占主组的 `0775` 目录，并拒绝共享组可写、带扩展访问 ACL 的组可写目录及 `0777` 目录。Windows 启动程序校验和交互终端的 panic 日志仍待处理。[目录权限测试](../crates/platform/src/autostart.rs#L627) |
| C3 | 已修复 · `02857b8` | 回归测试确认，等待撤权写锁超时会在写库前退出。撤权已提交给后台任务后，即使请求被取消，数据库与内存仍会同步为已撤权，并关闭该设备的连接、解除其终端会话挂接。停机成功后，即使客户端在回包前断开，监听器也会退出。[撤权测试](../crates/daemon/src/session_wire/tests.rs#L1630) · [停机测试](../crates/daemon/src/local_ipc.rs#L627) |
| C1 | 按维护者要求跳过 | 本轮未处置 Penpot 配置与令牌。 |

修复提交 `02857b8` 在 macOS 上通过了 `just check-changes origin/main`，涵盖工作区 Clippy 与测试、依赖策略、传输层补丁测试及 Relay 静态检查。Linux 专项验证覆盖了单元文件和目录权限。实际登录自启和托管跨平台 CI 尚未验证。

## P1 修复记录

`fix/project-review-p1` 承接 P0，代码截至 `8593cf4`。下表记录当前行为，后文各条发现描述审查基准 `9ad6b81` 上的行为。

| 编号 | 提交 | 当前行为 |
| --- | --- | --- |
| H16 | `bd629bc` | 单独修改发布元数据或守护进程更新器，都会选中原生打包检查。两条路径分别有分类器回归测试。 |
| H1–H3 | `408a541` | 每个输入批次中，组合字符和超链接各最多因配额触发一次网格扫描。REP 遇到首个被拒字符即停止，合法的短重复仍可用。回复达到 64 KiB 后按整条丢弃并计数，下一批输入恢复回复。控制串被新转义序列打断后，解析器从新序列继续。 |
| C2、H5 | `7362ae5` | Unix 根进程退出后，读线程会被唤醒，有 250 ms 排空末尾输出，然后可在从端仍打开时结束。模型按生成顺序将自动回复放入独立写队列。队列满时丢弃新的自动回复。阻塞写入期间仍能处理输出、调整窗口和发布画面。 |
| H4、H7、H8 | `51b6d97` | 拨号与候选连接仲裁在读状态前取得唤醒通知。中继观察通道关闭后退出该监听分支。停机先发通知，再等待清理和回收后台任务，绑定期间收到停机也会进入清理路径。 |
| H6 | `ef614bc` | 会话清理、表面帧压缩和上传临时文件删除交给阻塞线程，重绑等待使用异步计时器。会话请求在解压前拒绝压缩帧，普通控制回复直接按原编码发出。 |
| H9–H12 | `dddfda9` | 重连任务与前端命令独立。拨号最多 15 秒，附着最多 5 秒，重试间隔指数增长并带抖动。恢复期间变更的尺寸和颜色在新画面确认后补发。待交付的结束原因在接收其他命令或关闭命令通道后仍可读。原生键盘编码保留组合键修饰，并按子程序协商的协议输出。 |
| H13、H14 | `8593cf4` | Windows 控制台跳过颜色探测，原生粘贴中的定界符显示为文本。发送粘贴时遵循子程序的粘贴模式。配对提示自行读取中断键，先恢复终端设置再返回。Unix 使用实际配置的中断键。 |

H3 的通知回归测试另见 `79505b1`。测试逐字节送入被打断的通知，核对后续文字、颜色、光标和终端模式，并验证没有产生通知或回复。

公开仓库在 macOS 上以 `79505b1` 通过 `just check-changes origin/main`，包括工作区 Clippy 与测试、依赖与源码策略、补丁传输层测试和 Relay 静态检查。Cargo 结果汇总有 862 项工作区测试通过，连同传输层检查共 1,338 项通过。实际 PTY 测试确认从端保持打开时仍能收到末尾文字并在期限内结束。慢连接测试中，7 秒拨号和 4 秒附着期间反复取消读取并调整窗口，最终只发送一次附着请求。

移动端集成分支 `fix/project-review-p1` 的提交 `184dcc9` 固定公开仓库 `8593cf4`。在干净检出上通过 `just check-changes origin/main`。Android 的 Lint、单元测试、Debug 与 Release APK 构建及原生库对齐检查通过。iOS 设备目标构建通过，模拟器 XCTest 共 115 项，114 项通过，1 项需要一次性主机配对票据的远端验收用例按测试配置跳过。Windows x64 上的 CLI 及依赖已在 macOS 用 `cargo xwin check --locked -p voytty-cli --all-targets --target x86_64-pc-windows-msvc` 完成交叉编译检查，Windows 原生运行验证仍由托管 CI 执行。

## P2 修复记录

在 `fix/project-review-p2` 上接续 P1。本阶段涵盖路线图的 11 个编号。桌面修复截至 `616ef39`，移动端验证的共享代码截至 `e7737c9`，发布与 CI 加固截至 `9d25f05`。

| 编号 | 修复及验证 |
| --- | --- |
| H15、H17 | Linux 候选构建固定 rustup-init 1.29.0 及两个架构的 SHA-256。两份安装器在固定的 Debian 10 镜像中实际启动成功。为兼容 glibc 2.28，APT 仍使用历史 Debian 快照的有效期例外。原生候选、候选汇总及正式签发均重新下载依赖。普通 CI 的 Cargo 下载缓存仅含索引与压缩包，PR 只读。[构建脚本](../tools/release/setup-glibc-builder.sh) |
| M22 | 发布操作员确认有草稿读取权后分页查重。标签校验检查当前可见的 Release，发布作业在创建草稿前用写权限再次查遍所有页。签发只选择目标 SHA 上成功的完整 main 手动运行。夹具覆盖第二页已有草稿、GitHub API 出错，以及较新的快速推送遮住完整运行。[发布夹具](../tests/release/operator-fixture.sh) · [候选选择夹具](../tests/release/candidate-fixture.sh) |
| M1 | 发送配对证明以前的失败，以及主机确认凭据校验失败，都会立即结束，保留本地设备信息。证明已开始发出而最终回执丢失时，仍可通过普通认证连接确认授权。[配对回归测试](../crates/daemon/src/pairing_service.rs) |
| M2 | Unix 更新与安装检查可执行文件的祖先目录、属主和权限。允许 root 或本人拥有的 `0775`，识别 macOS 系统别名。更新暂存和备份使用系统随机数，待发布文件在替换原程序前通过身份自检。夹具验证了自检失败时旧程序仍可用。[更新器](../crates/platform/src/user_state.rs) |
| M3 | Windows 每次监听使用私有文件中的随机名称。内核依据用户 SID 和完整性标签限制连接，客户端检查已连接管道句柄的属主，避免二次按 PID 查进程。固定版本的 interprocess 已为首个实例和远程连接设置保护。显式监听器有独占、关闭与重新绑定测试。[Windows IPC](../crates/platform/src/local_windows.rs) |
| M4、L1 | 文件接收前按申报大小预留配额。每会话最多 200 MB、64 个文件、4 个同时上传，全账户为 1 GB、256 个文件、16 个同时上传。文件自最后修改起保留 24 小时。守护进程启动、每小时和接收新上传时清理过期文件。已发布文件被用户移动后会释放相应配额。Unix 旧目录在维护时按已知文件名清理。暂存句柄在重命名和删除前关闭。[上传说明](../wiki/remote-cli.md) |
| M24 | Windows 安装引导拒绝空响应与非法版本。安装器在目标目录下用独占创建和私有 ACL 暂存，检查目录权限及重解析点，并保留系统 TLS 策略。Windows CI 增加真实引导脚本的本地 HTTPS 成功与拒绝用例。macOS PowerShell 实测通过脚本解析、Win32 互操作代码编译与非法版本检查。[安装夹具](../tests/release/installer-windows.ps1) |
| M25 | Relay 和探针均固定到 Iroh 1.3.0。两个架构的镜像均已构建并执行。容器只读、移除 Linux capabilities，限制内存、进程数和单客户端接收速率。本机容器已通过 HTTP 健康检查和真实 Iroh 认证握手。部署改为用已校验制品在本地构建版本化镜像。[部署说明](relay.md) |
| M26 | CI 运行 Iroh 完整库测试，含 3 项真实 TLS Relay 回归。本机 165 项通过，1 项沿用上游忽略标记。工作区、Relay 探针、vendor/iroh 和 vendor/noq-proto 的四份锁文件均通过依赖审计。上游测试依赖去掉不需要的 heapless 默认特性，移除已停止维护的 atomic-polyfill。保留的维护状态与许可证例外已记录原因。[传输补丁说明](../vendor/iroh/VOYTTY.md) · [依赖策略](../deny.toml) |

公开仓库在 macOS 上以 `616ef39` 通过 `just check-changes origin/main`。Cargo 报告工作区 866 项、noq-proto 441 项、Iroh 165 项通过，合计 1,472 项，另有 17 项按已有标记忽略。发布与安装夹具、Clippy、四份锁文件审计以及两个架构的 Relay 制品校验通过。

本机从大型 Cargo 编译目录运行时，守护进程的 UDP 重新绑定测试曾超时，随后从默认目录和独立临时目录单独运行均通过。平台库并行测试时，诊断日志有一次在 1 秒内未取得写锁。最终检查在独立临时目录运行相同的测试程序，平台库的不同用例依次执行，日志用例内部的四个并发写入者、断言与时限均保留。

移动端 `fix/project-review-p2` 的 `3b5aaac` 固定公开仓库核心 `e7737c9`，在独立检出通过 `just check-changes origin/main`。Android 完成 Lint、单元测试、Debug 和 Release 构建及原生库对齐检查。iOS 设备目标构建成功，模拟器 XCTest 共 115 项，114 项通过，1 项需要一次性配对票据的远端验收按配置跳过。

Windows x64 在 `616ef39` 完成全工作区及测试目标的 `cargo xwin check --locked --workspace --all-targets --target x86_64-pc-windows-msvc`。Windows 管道 ACL、PowerShell 5.1 的真实 HTTPS 安装及 Windows ARM64 运行仍由原生托管 CI 验证。

## P3 修复记录

2026-11-20，`fix/project-review-p3` 从同一审查基准接续 P2。

| 范围 | 当前行为 |
| --- | --- |
| M15 桌面重绘 | 首次绘制和丢失绘制基线后，完整设置鼠标捕获、焦点和光标。增量绘制只发送改变的输入模式，光标形状或闪烁设置变化时才发送 DECSCUSR。[ANSI 呈现器](../crates/cli/src/terminal_ui/ansi_presenter.rs) |
| M16 历史缓存 | 主屏历史达到上限后，原地改字和清行保留缓存。滚动实际挤出旧行、清空历史或调整窗口大小会更新历史版本。测试覆盖了新旧行内容相同和切换备用屏幕。[终端模型](../crates/terminal/src/model.rs) |
| 桌面与守护进程模块 | 桌面入口、输入解码、历史视口和 Windows 控制台使用独立 Rust 模块。守护进程按会话服务、附着命令、附着流读写、连接需求与拨号拆分。原内联测试移到对应模块。[桌面入口](../crates/cli/src/terminal_ui.rs) · [会话](../crates/daemon/src/session.rs) · [会话协议](../crates/daemon/src/session_wire.rs) · [连接管理](../crates/daemon/src/connection_broker.rs) |
| 帧读取与命令排队 | 共享读取函数保留半帧和一次读到的后续帧。各调用方保留自己的流结束语义，包括本机隧道断开时允许重连。会话和数据库命令通过有界通道等候空位，保留各自的截止时间、错误分类与已开始操作的处理。[帧读取](../crates/client/src/framing.rs) · [数据库队列](../crates/daemon/src/store.rs) |
| 文档与清理 | 更新依赖图、移动端编译目标、五个桌面发布目标、十二件发布资产、Relay 本地构建与回滚步骤，以及 Windows、iOS 的交付状态。18 处历史死链指向拆库前的公开提交 `02f19ddb`，逐一核对 Git 对象。移除了已执行的 Trellis 迁移计划和空钩子配置。[开发指南](development.md) · [移动端边界](mobile-repository.md) · [历史验收](verification/multi-device.md) |

公开仓库以 `5956410` 在 macOS 的干净构建目录通过 `just check-changes origin/main`。工作区通过 868 项、noq-proto 441 项、Iroh 165 项，合计 1,474 项通过，17 项按已有标记忽略。守护进程启动、诊断日志并发写入及两项新增终端回归均以默认测试并发通过。Clippy、四份锁文件的依赖审计和两个架构的 Relay 制品校验通过。

Windows x64 在 `5956410` 通过全工作区及测试目标的 `cargo xwin check --locked --workspace --all-targets --target x86_64-pc-windows-msvc`。原生控制台运行验证由 Windows 托管 CI 执行。

移动端分支 `fix/project-review-p3` 的 `df286c0` 固定共享提交 `5cd5ba5`，在独立检出通过 `just check-changes origin/main`。Android Lint、单测与 Debug、Release 构建通过。iOS 设备构建通过，模拟器 XCTest 共 115 项，114 项通过，1 项需要一次性配对票据的远端验收按配置跳过。

## P3 后续修复记录

2026-11-20，分支 `fix/project-review-daemon-followup` 接续 P3。提交 `df27983` 处理守护进程的三项遗留问题。

| 编号 | 修复结果 |
| --- | --- |
| M6 | 授权持久化、配对票据提交和内存授权发布完成后，立即释放该设备的写锁。对端接收配对确认的速度不再影响同一设备的握手或撤权。[入站配对](../crates/daemon/src/pairing_service.rs) |
| M7 | 关停与登记新的配对任务共用一把锁。关停前登记的任务由守护进程等候，之后到达的任务在启动前收到取消结果。[配对任务管理](../crates/daemon/src/pairing_service.rs) |
| M10 | 本机错误回包的写入和关闭沿用本次请求的截止时间。单次请求和远端隧道只有在进度消息完整写出后才能继续使用连接。进度写入中断时立即关闭连接，已提交的操作仍可完成。[本机 IPC](../crates/daemon/src/local_ipc.rs) · [进度消息](../crates/daemon/src/connection_progress.rs) · [远端隧道](../crates/daemon/src/remote_tunnel.rs) |

本轮全分支检查还发现，终端自然退出时偶尔只收到断线。提交 `a24a296` 修复了两个已用回归测试单独确认的条件。会话工作线程退出时会释放队列中未执行请求的回复端，唤醒等候者。过时的屏幕更新通知遇到会话已结束时，写入任务继续交付最后的屏幕变化和退出原因。[会话队列](../crates/daemon/src/session.rs) · [终端事件写入](../crates/daemon/src/session_wire/attachment_io.rs)

验收使用提交 `a24a296`。macOS 通过 `just check-changes origin/main`，工作区 872 项、noq-proto 441 项、Iroh 165 项测试通过，合计 1,478 项通过，17 项按原有标记忽略。Clippy、依赖审计和两个架构的 Relay 制品校验通过。Windows x64 和 ARM64 均通过守护进程及其测试目标的 `cargo xwin check --locked -p voytty-daemon --all-targets`。ARM64 交叉编译使用 Clang 驱动及其配套的 NEON 头文件。本轮 Windows 验证范围为交叉编译，原生运行仍待托管 CI。

## 会话管理后续修复

2026-11-20，分支 `fix/project-review-session-followup` 接续守护进程修复。代码提交 `9aaac17`。

| 编号 | 修复结果 |
| --- | --- |
| M5 | 同一守护进程内的租约序号不重复。最多保留 64 个租约，满额时回收没有未完成操作的旧租约。已淘汰租约不能在原设备或新的授权代次下重新执行。测试覆盖 64 个同时在途的租约和随后 129 次授权代次切换。[重放登记](../crates/daemon/src/session.rs) |
| M11 | 接管前先检查终端状态。在同一次终端更新中切换配色、画面基线和通知接收者，成功后才交出旧控制权。回归测试模拟通知接收者无法切换，确认旧控制端仍能输入、原配色保留，之后可以正常接管。[会话接管](../crates/daemon/src/session/attachments.rs) |
| M12 | 重复操作按各自请求的截止时间等待。会话开始关闭后，清理任务保存最终结果。即使首个调用和重复调用均已超时，在租约和结果的保留期内，用原操作 ID 仍可取回结果。[会话服务](../crates/daemon/src/session/service.rs) · [实现约定](persistent-sessions.md) |

提交 `9aaac17` 在 macOS 上通过 `just check-changes origin/main`。工作区 873 项、noq-proto 441 项、Iroh 165 项测试通过，合计 1,479 项通过，17 项按已有标记忽略。Clippy 和两个架构的 Relay 制品校验通过。Windows x64、ARM64 均通过守护进程及其测试目标的交叉编译检查，Windows 原生运行仍待托管 CI 验证。

截至 `9aaac17`，上轮列出的中危问题中还有七项待修，M21 部分完成。

## 连接管理后续修复

分支 `fix/project-review-broker-followup` 接续会话管理修复，代码提交为 `7940911`。

| 编号 | 修复与回归验证 |
| --- | --- |
| M8 | 退役请求在返回调用者之前就排入设备锁的等待队列。只有指定的旧连接仍被选中且符合退役条件时才将其关闭。接用已启动的备用连接时，新拨号可等候空位。真实中继测试占住唯一的拨号名额和设备锁，确认旧连接仍被退役，重试接用了备用连接，全程只建立两条连接。[拨号](../crates/daemon/src/connection_broker/dial.rs) · [回归测试](../crates/daemon/src/connection_broker/tests.rs) |
| M9 | 同一个后台任务按 250 ms 窗口合并已验证的路由更新。停机时立即处理已接收的更新，并在调用方的截止时间内等待后台任务退出。定期清理空闲设备记录，同时保留在用连接和仍需拒绝过时握手的记录。测试覆盖 512 次路由更新、数据库阻塞时的停机超时与后续排空，以及空闲设备记录回收。[后台维护与测试](../crates/daemon/src/connection_broker/maintenance.rs) |

提交 `7940911` 通过守护进程 Clippy、21 项连接管理测试和 11 项远端会话缓存测试。macOS 通过 `just check-changes origin/main`，其中工作区 875 项、noq-proto 441 项、Iroh 165 项测试通过，合计 1,481 项通过，17 项按已有标记忽略。Windows x64 和 ARM64 通过守护进程及测试目标的交叉编译。ARM64 使用 Clang 配套的 NEON 头文件，Windows 原生运行仍待托管 CI 验证。

上轮列出的中危问题中，M13、M17、M18、M19、M20 仍待修复。M21 的 Linux 目录权限已修复，Windows 自动启动程序校验和交互终端的崩溃日志仍待处理。

---

## 一、总体评估与本地基线验证

`voytty` 的整体工程成熟度与安全意识远高于一般同类项目，尤其在以下方面表现突出：
- **零 `unsafe` / 严苛静态门禁**：工作区全局启用 `unsafe_code = "forbid"`、`unwrap_used = "deny"`、`clippy::all = "deny"`、`missing_docs = "warn"`，且所有 9 个 workspace member 均正确继承 `[lints] workspace = true`。
- **端到端加密与防降级发布体系**：配对采用域分离规范字节流 + HKDF/HMAC-SHA256 双向确认与 `Zeroizing` 缓冲；自更新强制校验 Ed25519 签名、精确长度与 SHA-256、单文件 tar 白名单及防降级 SemVer，并在激活前后执行无副作用自检与自动回滚；GitHub Actions 全部外部 Action 均 40 位 SHA 锁定，无脚本注入漏洞。
- **高纪律性的协议与终端安全边界**：压缩层防解压炸弹；OSC 52 禁用读剪贴板、OSC 8 严格限制 `http`/`https`、通知与网格单元校验拦截控制字符；诊断日志采用封闭 Schema 且严格脱敏。

### 本地只读基线检查结果

| 检查项 | 结果 | 说明 |
| --- | --- | --- |
| `cargo +1.98.0 clippy --workspace --all-targets --all-features -- -D warnings` | 通过 | 零警告 |
| `RUSTDOCFLAGS="-D warnings" cargo +1.98.0 doc --workspace --no-deps` | 通过 | 零文档/链接警告 |
| `cargo +1.98.0 fmt --all -- --check` | 通过 | 格式一致 |
| `source-policy.sh` / `workspace-version.sh` / `terminal-dependency-policy.sh` / `licensing.py check` | 通过 | 策略校验通过 |
| `secret-scan.sh` / `actionlint` / `shellcheck` | 通过 | 注意 `secret-scan.sh` 存在人工豁免（见 C1） |
| `tests/ci/*_test.py`、`recover_draft_test.py`、`resume_signed_test.py` | 通过 | 35/35 通过 |
| `cargo +1.98.0 deny --offline check licenses bans sources` | 通过（27 条重复依赖警告） | 离线环境未联网执行 `advisories` |
| 其他 OS/架构、Docker/QEMU Relay、签名与发布 | `HOSTED-ONLY` | 由 GitHub Actions 托管验证，本次本地未运行 |

### 审查基准的问题分布

| 复核后级别 | 基准数量 | 编号范围与主要集中领域 |
| --- | :---: | --- |
| **严重（Critical）** | 3 | `C1`、`C4`、`C5`：公开仓库追踪私密令牌配置与扫描豁免、单按 `Esc` 永久缓冲并破坏输入、Linux systemd `WorkingDirectory` 语法错误（原 `C2`/`C3` 保留编号并调至 High） |
| **高危（High）** | 19 | `H1`–`H17` + 调级移入的 `C2`、`C3`：终端复杂度 DoS 与 `DCS +q` 杀会话、未闭合控制串卡死解析器、PTY 孙进程有条件等待无界（`C2`）、IPC 取消边界与停机回包漏发信号（`C3`）、`ConnectionBroker` `Notify` 唤醒丢失（`H4`，已移除 `AcceptCell` 误报子项）、PTY 双向死锁、单线程运行时饥饿、网络任务 100% 忙旋与关闭泄漏、客户端重连被指令打断/丢结果/无退避、Kitty 键盘编码、Windows 控制台与 `pair accept` `Ctrl+C` 终端残留、CI 分类器遗漏（`H16`）与供应链/缓存边界 |
| **中危（Medium）** | 23 | `M2`–`M13`、`M15`–`M22`、`M24`–`M26`：Unix/Windows 路径与 Named Pipe 边界、文件上传磁盘耗尽、Broker/Session/LocalIPC 并发细节、Relay 容器与版本漂移、Windows 安装脚本边界、渲染/滚动缓冲与移动端耗电问题（原 `M1` 调至 Low；`M14`、`M23` 已撤回，见[误报与修订核查](project-review-2026-11-19-false-positives.md)） |
| **低危与信息（Low / Info）** | 1 项 + 13 组 | 调级移入的 `M1`（已有授权下的配对结果与本地元数据边界），以及原 13 组边界清理、零化一致性、`ulimit -f` 风险、脚本与检查覆盖面缺口、文档漂移与 18 处失效链接 |

---

## 二、严重问题（Critical）

> 以下 3 项在审查基准上评为 Critical。C4、C5 已在修复分支验证，C1 按维护者要求跳过。

### C1. 公开仓库跟踪了含 Penpot MCP 令牌的配置文件，并在密钥扫描中被硬编码豁免
- **位置**：[`.codex/config.toml`](../.codex/config.toml#L1-L3)、[`tests/secret-scan.sh`](../tests/secret-scan.sh#L5-L8)、[`tests/secret-scan-fixture.sh`](../tests/secret-scan-fixture.sh#L72-L91)、[`docs/ci-release-optimization.md`](ci-release-optimization.md#L77-L78)
- **现象与根因**：`git ls-files .codex/config.toml` 已证实该文件处于公开仓库 `leonfox28/voytty` 的 Git 追踪之下，其第 3 行包含 `https://design.penpot.app/mcp/stream` 的 `userToken` JWE 凭据（注释明确写明“contains a private token”）。`tests/secret-scan.sh` 通过 `approved_connection` 显式跳过了对该文件的凭据赋值和 JWT/JWE 扫描。该令牌当前是否仍然有效**未知**（审查未做线上探测，也不转贴凭据值）。
- **影响**：任何克隆或浏览公开仓库及历史提交的人均可读取该凭据，且扫描豁免使该暴露路径持续绕过 CI 检查。
- **修复建议**：无论当前有效性如何均建议立即轮换/撤销该 Penpot 令牌；将 `.codex/config.toml` 移出 Git 追踪（迁至本机 `~/.codex/config.toml` 并加入 `.gitignore`）；移除 `tests/secret-scan.sh` 中的 `approved_connection` 豁免及对应测试桩。

> **编号索引说明**：原 `C2`、`C3` 经 2026-11-19 复核降为 **High**，保留原编号并移至下文[守护进程并发、网络与生命周期](#守护进程并发网络与生命周期)小节。

### C4. Unix CLI 中单按 `Esc`（`0x1b`）在 `HostInputCodec` 被无限期缓冲，不仅卡死 `Esc` 还把后续鼠标移动/方向键拼成乱码注入终端
- **位置**：[`crates/cli/src/terminal_ui.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui.rs#L2744-L2813) / [`terminal_ui.rs#L2843-L2916`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui.rs#L2843-L2916) / [`terminal_ui.rs#L3001-L3006`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui.rs#L3001-L3006)、[`crates/cli/src/terminal_ui/session.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui/session.rs#L196-L213)
- **触发路径**：
  - 在默认传统键盘模式（`keyboard_flags == 0`，Shell / Vim / tmux / fzf / htop 等）下，按下 `Esc` 只发送单字节 `0x1b`。
  - `known_host_prefix(b"\x1b")` 因 `PAGE_UP`（`\x1b[5~`）以 `\x1b` 开头而返回 `true`，导致 `HostInputCodec::feed` 直接 `break`，且整个 `TerminalUiSession::run_loop` 没有任何 ESC 歧义消解超时或 flush 机制。
  - **连锁破坏**：
    1. 单按 `Esc` 永远无响应（如 Vim 无法退出插入模式）。
    2. 由于外层终端开启了任意移动鼠标上报（`HOST_INPUT_CAPTURE` = `\x1b[?1003h\x1b[?1006h`），按 `Esc` 后轻晃鼠标会产生 `pending = b"\x1b\x1b[<35;col;rowM"`；L2893 将前两字节 `\x1b\x1b` 作为 `Opaque` 吃掉，剩下的 `[<35;col;rowM` 失去前导 ESC 后被当作**用户键盘输入的纯文本**直接打进远端 PTY！按 `Esc` 后紧接方向键/翻页/粘贴也会产生同等乱码。
    3. 按 `Esc` 后输入 `]` 会进入 OSC 响应吞噬模式（L2744-2813），将后续所有按键静默吞为 `HostReply::Ignored`；跨输入 epoch 栅栏时残留的 `0x1b` 还会污染下一按键的 `source_epoch`。
- **修复建议**：为 `HostInputCodec` 引入显式的 ESC 歧义消解计时器（如 25–50 ms），并在 `run_loop` / `await_while_inactive` 中轮询 flush；当 `pending == [0x1b]` 且下一字节仍是 `0x1b` 时，先刷出独立的 `0x1b` 再按新转义序列解析，禁止把 `\x1b\x1b` 合并为 2 字节 `Opaque`。

### C5. Linux systemd 自动启动生成了带字面双引号的无效 `WorkingDirectory="..."`
- **位置**：[`crates/platform/src/autostart.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/platform/src/autostart.rs#L262-L266) / [`autostart.rs#L401-L409`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/platform/src/autostart.rs#L401-L409)
- **触发路径**：`Manager::Systemd` 渲染单元文件时使用 `unit_quote(home).replace("$$", "$")` 输出 `WorkingDirectory="/home/user"`。与 `ExecStart=` 不同，systemd 的 `WorkingDirectory=` 解析器（`config_parse_working_directory`）**不剥离双引号**，看到首字符 `"` 会判定非绝对路径并拒绝加载/启动单元，导致 Linux systemd 环境下 `voytty daemon autostart enable` 实际无法生效。
- **修复建议**：直接使用 systemd 内置用户目录占位符 `WorkingDirectory=%h`，或在校验后的原始路径上仅做 `% -> %%` 转义（不加引号、不反斜杠转义，并拒绝以 `\` 结尾的路径）。

---

## 三、高危问题（High）

### 安全策略与终端引擎

#### H1. 终端组合字符与超链接配额对账缺少脏标记 + `CSI b` 4096 倍放大导致单核 CPU 耗尽 DoS
- **位置**：[`crates/terminal/src/engine.rs`](../crates/terminal/src/engine.rs#L337-L365) / [`engine.rs#L414-L440`](../crates/terminal/src/engine.rs#L414-L440)、[`crates/terminal/src/hyperlinks.rs`](../crates/terminal/src/hyperlinks.rs#L60-L69) / [`hyperlinks.rs#L86-L111`](../crates/terminal/src/hyperlinks.rs#L86-L111)、[`crates/terminal/src/ingress.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/terminal/src/ingress.rs#L441-L455) / [`ingress.rs#L584-L601`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/terminal/src/ingress.rs#L584-L601)
- **根因**：一旦会话内组合字符（4,096 格 / 64 KiB）或超链接（1,024 条 / 1 MiB）配额被回滚缓冲中的存活单元格占满，后续**每一个**组合字符或新 `OSC 8` 都会无条件全量扫描 `(history_size + screen_lines) * cols`（最多 `(2000 + 80) * 240 = 499,200` 格）。同时 `print_character()` 也将组合字符写入 `preceding_character`，使得一条 7 字节的 `CSI 4096 b`（REP）触发 `4,096 × 499,200 ≈ 20.4 亿` 次单元格检查，持锁（`model_shared.commit`）将 `voytty-terminal-model` 线程打满数分钟。
- **修复建议**：为网格维护仅在覆盖/擦除/滚动/切屏/缩放/重置时置位的脏标记或变更代数；对账后清零脏标记，未变脏时 O(1) 直接拒绝；禁止 `CSI b` 重复零宽组合字符。

#### H2. 不可信 PTY 输出通过 `DCS +q` 10.6 倍响应放大触发 `ReplyOverflow`，直接杀死活跃会话
- **位置**：[`crates/terminal/src/ingress.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/terminal/src/ingress.rs#L156-L162) / [`ingress.rs#L694-L697`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/terminal/src/ingress.rs#L694-L697)、[`crates/terminal/src/colors.rs`](../crates/terminal/src/colors.rs#L321-L347)、[`crates/daemon/src/terminal_driver.rs`](../crates/daemon/src/terminal_driver.rs#L386-L394)、[`crates/daemon/src/session.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L3297-L3339)
- **根因**：`DCS +q` 查询 `"Setulc"`（13 字节输入）会生成 138 字节回复（10.6 倍放大）。单次 8 KiB PTY 读取中仅需 7 条 `DCS +q`（约 7.2 KiB）即可产出 75,348 字节响应，突破 `MAX_REPLY_BYTES_PER_UPDATE`（64 KiB）并返回致命错误 `IngressError::ReplyOverflow`。`TerminalDriver` 将任何 `ingest()` 错误视为驱动故障并终止模型线程，进而促使 `SessionActor` 杀死用户 Shell 并以 `DriverFailure` 永久结束会话。
- **修复建议**：将响应缓冲超限降级为“丢弃超额回复 + 上报有限侧事件”（类似 `EventsDropped`），而非终止驱动；限制单条 `DCS +q` / `OSC 4` / `CSI $p` 最多应答的条目数。

#### H3. 未闭合的 OSC/DCS/APC/PM/SOS 控制串会永久卡死主机终端解析器
- **位置**：[`crates/terminal/src/ingress.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/terminal/src/ingress.rs#L332-L366)
- **根因**：在 `PolicyState::String` 状态中，遇到 `ESC` 后若下一字节不是 `\`（例如 `\x1b[2J` 或 `\x1bc` RIS 重置），代码将 `0x1b` 与该字节压入字符串并**停留在 `PolicyState::String`**，违背 ECMA-48/VT500 规范（应中止控制串并转入 `PolicyState::Escape`）。任何子程序输出截断的 OSC/DCS 后，整个会话后续输出（含 `reset`）均会被永久吞噬。
- **修复建议**：当 `string.saw_escape && byte != b'\\'` 时，终止当前控制串并将 `byte` 交由 `PolicyState::Escape` 处理。

### 守护进程并发、网络与生命周期

#### C2（保留原编号，现行等级：High / P1）. Unix 下后代持有 PTY 从端且读端未退出时，`TerminalDriver::finalize` 存在有条件的无界等待
- **位置**：[`crates/platform/src/pty/unix_io.rs`](../crates/platform/src/pty/unix_io.rs#L32-L37) / [`unix_io.rs#L79-L95`](../crates/platform/src/pty/unix_io.rs#L79-L95)、[`crates/platform/src/pty.rs`](../crates/platform/src/pty.rs#L638-L651)、[`crates/daemon/src/terminal_driver.rs`](../crates/daemon/src/terminal_driver.rs#L626-L657)、[`crates/daemon/src/session.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L1117-L1141) / [`session.rs#L3355-L3373`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L3355-L3373)
- **证据与触发条件**（原有结论基于静态路径分析，跨平台持续时长及 Windows ConPTY 表现待实测验证）：
  1. 触发前提为会话内存在继承了 PTY 从端 FD 且未退出的后代进程（例如常见后台任务 `cmd &`、`nohup ...`）。
  2. 根 shell 退出或被 `session close` / `voytty daemon stop --yes` 终止后，`PtyChild::close_explicitly()` 仅终止直接子进程 PID；在后代仍持有从端且读端没有收到 EOF、EIO 或其他会让其退出的事件的前提下，读线程会继续停在阻塞读中。
  3. `BlockingReader::read()` 在 `poll(&mut events, None)` 上阻塞且没有 `mio::Waker` 或 `InputStop` 检查（与同文件 `InterruptibleWriter` 不同）；`TerminalDriver::finalize()` 在释放 `self.io`（持有 `MasterPty`）前先阻塞调用 `reader_thread.join()`，在上述前提下 `join` 可能无界等待，按代码路径会影响 `SessionActor::finish_runtime`、`session close`（5 秒超时）及 `voytty daemon stop --yes` 停机。
- **修复建议**：先补齐 Unix 集成回归测试复现验证；为 `BlockingReader` 增加与 `InterruptibleWriter` 对称的 `mio::Waker` + 停止标志，使显式关闭能够立即唤醒并退出读线程；对自然退出增加有限宽限期的排空超时后再中断并关闭 master PTY；评估在显式关闭/强停时向 PTY 进程组发送 `SIGHUP`/`SIGTERM`。

#### C3（保留原编号，现行等级：High / 优先处理）. 撤权持久化与内存发布存在取消边界，且 `local_ipc.rs` 停机回包失败会漏发停机信号
- **位置**：[`crates/daemon/src/local_ipc.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/local_ipc.rs#L489-L547)、[`crates/daemon/src/service.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/service.rs#L463) / [`service.rs#L664-L667`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/service.rs#L664-L667) / [`service.rs#L806-L860`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/service.rs#L806-L860) / [`service.rs#L1002-L1020`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/service.rs#L1002-L1020)、[`crates/daemon/src/session.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L1105-L1117) / [`session.rs#L1982-L2014`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L1982-L2014)
- **证据与两条独立路径**（审查基准上的静态分析）。修复后的运行结果见 [P0 修复记录](#p0-修复记录)。
  1. **撤权持久化与内存发布取消边界（及配对超时契约违背）**：`local_ipc.rs:508` 用第二个 `tokio::time::timeout(remaining, ...)` 包裹已携带相同 `absolute_deadline` 的 `dispatch_until_with_progress`。在 `device_revoke_reply`（`service.rs:806-860`）中，`revoke_guard(device_id).await` 未受内层 `deadline` 约束；随后的 `store.run_blocking_until(...)`（L809-813）在后台执行 SQLite `store.revoke(...)`，外层仍在 `.await` 该后台结果——**若后台数据库工作已提交而外层此时被取消，就会出现 SQLite 已为 `Revoked` 而内存 `AuthorizationRegistry` 未发布的不一致**；一旦 `.await` 恢复轮询，到同步的 `guard.publish(Revoked)`（L827）之间不再有新的 `.await`，因此若取消发生在 `guard.publish` 之后，内存态已更新，受影响的则是后续 `close_remote` 与 `detach_remote_principal_until` 清理。同时该外层超时违背了 `service.rs:664-667` 不得覆盖 `PairOutcomeUnknown` 的注释契约。
  2. **会话已关闭，守护进程可能漏收停机信号。** 成功处理停机后，注册表停止接收新会话。此时若写出回复或关闭回复流失败，IPC 处理函数会提前返回，跳过 `stop_sender.send(())`。另一个时序是外层请求先超时，后台停机工作仍可能完成。此时 IPC 返回的超时回复带有 `stop_after_flush = false`，即使写出成功也不会发送停机信号。两种情况下，进程都可能继续存活并拒绝新建会话。按源码，在会话已清空时重试 `voytty daemon stop` 可让进程正常退出，此恢复路径尚待实测。[请求超时与回包](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/local_ipc.rs#L528-L546) · [错误回复的停机标志](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/service.rs#L1234-L1246) · [空会话停机](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L1104-L1116)
- **修复建议**：移除 `local_ipc.rs:508` 的重复外层 `timeout`，统一依赖内层 `absolute_deadline`（并为 `service.rs:806` 的 `revoke_guard` 加上 deadline）；当 `reply.stop_after_flush == true` 时无论回包写入是否成功均须触发 `stop_sender`；在 `store.revoke` 提交后通过不可取消边界保证完成 `guard.publish(Revoked)` 及后续断连清理。

#### H4. `ConnectionBroker` 两处 `Notify::notified()` 在释放互斥锁后才创建，存在唤醒丢失
- **位置**：[`crates/daemon/src/connection_broker.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/connection_broker.rs#L1856-L1896) / [`connection_broker.rs#L2378-L2415`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/connection_broker.rs#L2378-L2415) / [`connection_broker.rs#L3437-L3443`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/connection_broker.rs#L3437-L3443)
- **根因**：`run_dial_worker` 与 `promote_candidate` 均在释放 `slot.state` 锁**之后**才调用 `wait_for_notify_optional` / `wait_for_notify` 创建 `slot.changed.notified()`。若另一任务在“解锁 → 创建 `notified()`”窗口内触发 `slot.changed.notify_waiters()`，本次广播会被漏掉，分别导致重连卡死长达 30 秒或健康候选连接超期自毁。（注：初版同时列出的 `PairingService` `AcceptCell::wait_until` 经对照 Tokio 1.53.3 源码复核为误报——该处先于查锁创建 `notified()`，且 Tokio 对 `notify_waiters()` 保证从 `notified()` 创建起即捕获后续广播，已从本项移除；详见[误报核查](project-review-2026-11-19-false-positives.md)。）
- **修复建议**：在这两处释放 `slot.state` 锁**之前**先创建 `let notified = slot.changed.notified();`，再在解锁后等待该 `notified`（或沿用同文件 L1668-1670 的 `tokio::pin!(notified); notified.as_mut().enable();` 模式）。

#### H5. `model_thread` 持锁同步写终端应答导致 PTY 双向死锁
- **位置**：[`crates/daemon/src/terminal_driver.rs`](../crates/daemon/src/terminal_driver.rs#L377-L405)、[`crates/platform/src/pty/unix_io.rs`](../crates/platform/src/pty/unix_io.rs#L103-L121)
- **根因**：`model_thread` 持有 `model_shared.commit` 锁并同步调用 `io.write_input(&update.replies)`，而 `InterruptibleWriter::write` 在 `WouldBlock` 上无限等待。若子进程挂起（`SIGSTOP`）或正在向已满的 PTY 输出缓冲同步写入，则 `model_thread` 阻塞 -> `ByteQueue`（64 KiB）满 -> `reader_thread` 停止排空 PTY 输出 -> 典型双向死锁，同时阻塞全部会话命令。
- **修复建议**：禁止在 `model_thread` 持锁期间执行无界阻塞写，将自动应答写入独立的有界非阻塞/异步写队列（满时按策略丢弃多余自动回复），确保 PTY stdout 持续排空。

#### H6. 单线程 Tokio 守护进程运行时在关闭/重绑期间完全冻结，且被同步 zlib 与同步文件删除阻塞
- **位置**：[`crates/daemon/src/lifecycle.rs`](../crates/daemon/src/lifecycle.rs#L452) / [`lifecycle.rs#L465-L491`](../crates/daemon/src/lifecycle.rs#L465-L491) / [`lifecycle.rs#L554-L571`](../crates/daemon/src/lifecycle.rs#L554-L571)、[`crates/daemon/src/session_wire.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session_wire.rs#L636) / [`session_wire.rs#L1068-L1074`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session_wire.rs#L1068-L1074)、[`crates/daemon/src/session_wire/upload.rs`](../crates/daemon/src/session_wire/upload.rs#L183)
- **根因**：整个守护进程运行在 `Builder::new_current_thread()` 上：
  1. 关闭和监听器重绑阶段退出 `runtime.block_on` 后同步执行长达 5 秒的 `sessions.shutdown_until()` 和 `thread::sleep(20ms)`，导致 Tokio reactor 彻底停摆，活跃 QUIC 连接心跳中断且 `TerminalSessionEnded` 无法在停网前刷出。
  2. 最大 8 MiB 快照的 zlib 压缩/解压直接在唯一异步线程上执行（且开启 zlib 时每个出站小控制帧还多绕了一遍 `FrameDecoder` 解析+重编码）。
  3. 上传取消/失败时 `StagedUpload::drop` 直接在异步线程上同步执行 `fs::remove_file`/`fs::remove_dir`。
- **修复建议**：将 `sessions.shutdown_until` 通过 `spawn_blocking` 放在 `runtime.block_on` 内执行、重绑改用 `tokio::time::sleep`；大帧 zlib 与上传目录清理卸载到 `spawn_blocking`，并省去非表面帧的二次解码；评估迁移至有界多线程运行时（如 2–4 worker）。

#### H7. `NetworkStartup::run_bound` 中 `relay_status.updated()` 断开后导致 100% CPU 忙旋并饿死所有入站连接
- **位置**：[`crates/daemon/src/network.rs`](../crates/daemon/src/network.rs#L739-L754)
- **根因**：`tokio::select!` 声明了 `biased;`，且 `status = relay_status.updated()` 位于 `handlers.join_next()` 和 `endpoint.accept()` 之前却**没有断开守卫**。若 Iroh 在 `endpoint.closed()` 前释放了状态发送端，该分支将立即返回 `Err` 并死循环抢占，彻底饿死入站连接并打满单线程 CPU。
- **修复建议**：增加 `let mut relay_status_open = true;` 条件守卫（`if relay_status_open`），收到 `Err` 后置为 `false`。

#### H8. `NetworkSupervisor::shutdown_until` 在 `quiesce_until` 超时时泄漏活跃 `Endpoint`，且启动竞态下停机失败
- **位置**：[`crates/daemon/src/network.rs`](../crates/daemon/src/network.rs#L630) / [`network.rs#L664`](../crates/daemon/src/network.rs#L664) / [`network.rs#L793-L820`](../crates/daemon/src/network.rs#L793-L820)
- **根因**：`shutdown_until` 在 L801 对 `broker.quiesce_until(deadline).await?` 提前返回，未发 `shutdown` 通知也未关闭 `Endpoint`；此外若 `quiesce_until` 发生在 `endpoint.bind()` 与 `broker.attach_endpoint()` 之间，后者返回 `TransportUnavailable` 导致正常停机失败。
- **修复建议**：无论 `quiesce_until` 是否报错都必须继续发 `shutdown` 并等待/关闭 `Endpoint`（最后再返回首个错误）；在 `attach_endpoint` 前后感知 `shutdown` 标志并将其视为正常停机。

### 共享客户端、CLI 与跨平台交互

#### H9. `run_terminal_driver` 收到任意前端指令时直接取消正在进行的远端重连
- **位置**：[`crates/client/src/view.rs`](../crates/client/src/view.rs#L835-L880)、[`crates/client/src/session.rs`](../crates/client/src/session.rs#L468-L543)
- **根因**：`client.read_next_event()` 直接置于 `tokio::select!` 中与 `commands.recv()` 并列。断线重连期间，任何 `Resize`、`UpdateColors`、`WriteInput`、`UploadOrigin` 等指令到达都会立刻取消 `reconnect_remote()`，反复打断 QUIC 拨号和附着握手。
- **修复建议**：将重连 Future 独立固定（仅在 `Detach` 或 `events.closed()` 时取消），非 detach 指令仅更新期望状态而不中断活跃重连。

#### H10. `run_terminal_driver` 排空 `pending` 时收到指令会静默丢弃 `SessionEnded` / `LeaseLost` / 错误结果
- **位置**：[`crates/client/src/view.rs`](../crates/client/src/view.rs#L883-L914)
- **根因**：当 `stop_after_pending == true` 且正在向有界通道（容量 8）排空终止事件时，`select!` 仍监听 `commands.recv()`。若此时到达任意命令或发送端关闭，L896 会直接 `return`，丢弃 `pending` 中的真实结束原因且不置 `terminal_outcome_queued = true`。
- **修复建议**：在 L884 为命令分支加上 `if !stop_after_pending` 守卫。

#### H11. `SessionClient::reconnect_remote` 以 250 ms 无退避永久重试，且将重连拨号+附着总超时错误压至 5 秒
- **位置**：[`crates/client/src/session.rs`](../crates/client/src/session.rs#L472-L526) / [`session.rs#L549`](../crates/client/src/session.rs#L549) / [`session.rs#L577-L582`](../crates/client/src/session.rs#L577-L582)
- **根因**：
  1. 对端离线或移动端断网时，每 250 ms 固定间隔无退避无抖动持续重试，消耗移动端电量并轰炸 Pkarr/DNS/QUIC。
  2. 首次连接给 `connector.open()` 分配 `REMOTE_STARTUP_DEADLINE`（15 s）+ 附着 5 s，但重连把 `connector.open()` 和附着共同限制在 `control_deadline()`（5 s）内，在 5–15 s 慢链路/中继冷启动下形成永久活锁。
- **修复建议**：重连复用“15 s `open()` + 5 s attach”两阶段预算，并采用带抖动的有界指数退避（如 250 ms → 1 s → 2 s → 5 s → 10 s，网络变化或成功后重置）。

#### H12. `KeyInput::encode` 在 Kitty `DISAMBIGUATE` 模式下吞掉组合 `Enter`/`Tab`/`Backspace`，且传统模式下泄露 CSI-u
- **位置**：[`crates/client/src/keyboard.rs`](../crates/client/src/keyboard.rs#L140-L176) / [`keyboard.rs#L236-L252`](../crates/client/src/keyboard.rs#L236-L252)
- **根因**（影响 iOS/Android 移动端与 Windows 控制台）：
  1. `recovery_key` 匹配 `Enter | Tab | Backspace` 时未检查 `modifiers == 0`，导致 Neovim/Helix/Fish/Claude Code 常用的 `Shift+Enter`、`Ctrl+Enter`、`Ctrl+Tab`、`Ctrl+Backspace` 退化为裸 `\r`/`\t`/`\x08`。
  2. L175-176 对无修饰按键仍输出 `;1`（如 `\x1b[1;1A` 而非 `\x1b[A`），违反 Kitty 协议规范。
  3. `legacy_unicode` 在 `Ctrl+Shift+<char>` 时回落输出原始 CSI-u（`\x1b[65;6u`），在 `keyboard_flags == 0` 的传统 Windows Shell 中产生乱码。
  4. `MOD_SUPER` 抑制仅写 `#[cfg(target_os = "macos")]` 而漏了 `target_os = "ios"`，iPadOS 外接键盘按 `Cmd+<key>` 会向终端键入字符。
- **修复建议**：限定 `recovery_key` 仅用于 `modifiers == 0`；无修饰 `Press` 省略 `;1`；传统模式不输出 CSI-u；将 `target_os = "ios"` 纳入 `MOD_SUPER` 抑制条件。

#### H13. Windows 控制台启动/重连固定卡顿 250 ms（264 条无应答探测），且 `Event::Paste` 允许跳出括号粘贴注入指令
- **位置**：[`crates/cli/src/terminal_ui.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui.rs#L375-L419)、[`crates/cli/src/terminal_ui/windows_console.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui/windows_console.rs#L141-L159) / [`windows_console.rs#L307`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui/windows_console.rs#L307)、[`crates/cli/src/terminal_ui/session.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui/session.rs#L359-L381) / [`session.rs#L924`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui/session.rs#L924)
- **根因**：
  1. Windows 下仍无条件发送 264 条 VT 颜色探测序列，但 `windows_console::StdinPump` 走 `crossterm::event::read()` 永不产出 `TerminalReply`，导致每次启动/重连固定卡满 250 ms 超时。
  2. `windows_console.rs:307` 将 `Event::Paste(text)` 直接拼装为 `\x1b[200~` + `text` + `\x1b[201~` 而不剔除内部 `\x1b[201~`，恶意粘贴文本可提前闭合粘贴态并触发本地 `Ctrl+]` 前缀命令（如断连/上传）。
  3. `session.rs:359-381` 未检查远端 `self.surface.modes().bracketed_paste`，直接透传带定界符的粘贴字节。
- **修复建议**：在 Windows crossterm 后端禁用 VT 颜色探测（或读取原生 VT 应答）；对 `Event::Paste` 过滤/剥离 `\x1b[201~` 与 `\x1b[200~`；远端未开启 bracketed paste 时剥除定界符后再转发。

#### H14. `voytty pair accept` 提示输入期间按 `Ctrl+C` 会让用户终端永久留在无回显（Unix 下兼非规范）模式
- **位置**：[`crates/cli/src/lib.rs`](../crates/cli/src/lib.rs#L1120-L1144) / [`lib.rs#L1194-L1226`](../crates/cli/src/lib.rs#L1194-L1226)
- **根因**：关闭 `ECHO | ECHONL | ICANON`（Windows 为 `ENABLE_ECHO_INPUT`）但保留 `ISIG`，并依赖 `Drop` 恢复终端属性。用户按 `Ctrl+C` 时 OS 直接终止进程，析构函数不运行，终端永久无回显。
- **修复建议**：在读取凭据期间关闭 `ISIG` 并由读取循环显式识别 `0x03`（`VINTR`）返回中断错误，或安装信号/控制台处理器确保恢复终端属性后再退出。

### 构建、CI 与发布供应链

#### H15. 正式 Linux 发布构建器以 `root` 执行未校验的 `curl https://sh.rustup.rs | sh`
- **位置**：[`tools/release/setup-glibc-builder.sh`](../tools/release/setup-glibc-builder.sh#L9) / [`setup-glibc-builder.sh#L23-L24`](../tools/release/setup-glibc-builder.sh#L23-L24)、[`.github/workflows/ci.yml`](../.github/workflows/ci.yml#L374)
- **根因**：编译最终被签名发布的 Linux 官方二进制时，容器以 UID 0 直接管道执行可变远程脚本，且缺少 `--proto-redir '=https'`、`--max-time` 与 SHA-256 校验（同文件 L13-19 亦对 `http://snapshot.debian.org` 关闭了 `Check-Valid-Until`）。
- **修复建议**：直接下载版本与平台固定的 `rustup-init` 二进制并硬编码 SHA-256 校验（或使用按 digest 锁定的自建 builder 镜像）。

#### H16. CI 路径分类器遗漏 `crates/core/src/release.rs` 与 `crates/daemon/src/distribution.rs`，PR 跳过全部 5 项原生打包门禁（P1）
- **位置**：[`tools/ci/workflow-policy.py`](../tools/ci/workflow-policy.py#L45-L53)
- **根因与实测确认**：L52 写成 `path.startswith("crates/core/src/release/")`（带尾斜杠），仅匹配到 `android.rs`，完全漏掉了桌面发布清单/签名核心文件 `crates/core/src/release.rs` 以及承载 `--internal-release-*` 协议的 `crates/daemon/src/distribution.rs`；**实际直接调用 `workflow-policy.py` 的 `path_plan` 已证实这两处路径均返回 `native=false`**。另外改动 `release/`（含 `release/public-key.hex`）时未触发 `mobile=True`。
- **修复建议**：补充 `path == "crates/core/src/release.rs"`、`crates/daemon/src/distribution.rs`（以及 `release/` 联动 `mobile=True`），并在 `tests/ci/workflow_policy_test.py` 增加回归测试。

#### H17. 发布签发及“冷构建”候选任务仍读写缓存可变的 `~/.cargo/registry`（含已解包 `src/`）
- **位置**：[`.github/workflows/release.yml`](../.github/workflows/release.yml#L246-L254)、[`.github/workflows/ci.yml`](../.github/workflows/ci.yml#L280-L288) / [`ci.yml#L375-L383`](../.github/workflows/ci.yml#L375-L383) / [`ci.yml#L603-L611`](../.github/workflows/ci.yml#L603-L611)
- **根因**：缓存 `~/.cargo/registry` 会把 `~/.cargo/registry/src/` 一并缓存；当目录中存在 `.cargo-ok` 时 Cargo 会跳过 `.crate` 对 `Cargo.lock` 的 SHA-256 校验。与 `candidates-windows` 零缓存策略以及“冷构建候选”文档声明不一致。
- **修复建议**：普通 CI 仅缓存 `~/.cargo/registry/index` 与 `~/.cargo/registry/cache`（PR 仅用 `actions/cache/restore`）；`candidates-*`、`candidate` 与 `release.yml` 的 `publish` 作业彻底移除 Cargo 缓存。

---

## 四、中危问题（Medium）

### 安全边界

> **编号索引说明**：原 `M1` 经 2026-11-19 复核降为 **Low / P2**，保留原编号并移至[第五节（低危与信息性发现）](#调级移入项保留原编号)。

#### M2. Unix `validate_executable_parent` 未检查父目录属主与全局可写权限，且更新临时路径可预测
- **位置**：[`crates/platform/src/user_state.rs`](../crates/platform/src/user_state.rs#L576-L610) / [`user_state.rs#L982-L994`](../crates/platform/src/user_state.rs#L982-L994)、[`crates/daemon/src/operations.rs`](../crates/daemon/src/operations.rs#L707-L733)
- **说明**：Unix 端仅校验直接父目录不是符号链接，未检查祖先符号链接、属主（本人或 root）或全局可写位（`0o002` 无粘滞位），且 `activate_executable` 使用可预测的 `.voytty.update-<pid>-<seq>` 并在执行 `verify_activated_candidate` 前存在短暂重命名窗口。多用户主机上若可执行文件位于不安全目录可导致本地提权/劫持。
- **修复建议**：在保留对 root/本人属主 `0o775`（如 macOS `/usr/local/bin`）兼容的前提下，拒绝非 root/非本人属主及全局可写目录，检查祖先路径符号链接，临时文件名改用密码学随机后缀。

#### M3. Windows Named Pipe 命名可预测、缺少显式防抢注/防模拟标志，且存在 PID 复用 TOCTOU
- **位置**：[`crates/platform/src/local_windows.rs`](../crates/platform/src/local_windows.rs#L73-L86) / [`local_windows.rs#L103-L118`](../crates/platform/src/local_windows.rs#L103-L118) / [`local_windows.rs#L160-L170`](../crates/platform/src/local_windows.rs#L160-L170) / [`local_windows.rs#L299-L304`](../crates/platform/src/local_windows.rs#L299-L304)、[`crates/platform/src/windows_security.rs`](../crates/platform/src/windows_security.rs#L101-L121)
- **说明**：`pipe_name` 仅由可预测的 `user_sid || socket_path` 哈希生成；与同文件 `spawn_isolated_command` 不同，主 IPC 路径经由 `interprocess` 未显式设置 `FILE_FLAG_FIRST_PIPE_INSTANCE`、`PIPE_REJECT_REMOTE_CLIENTS`、客户端 `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` 及强制完整性 SACL；且 `authorize_stream_peer` 异步按 `peer_pid` 二次 `OpenProcess` 校验存在 PID 复用竞态。
- **修复建议**：在受保护的用户私有目录保存每次启动的随机管道后缀，复用 `win-custody`/原生 Win32 API 显式开启上述标志，并通过管道令牌直接校验对端 SID。

#### M4. 文件上传缺少累计配额/数量/并发限制，且已发布文件永不自动清理（磁盘耗尽 DoS）
- **位置**：[`crates/daemon/src/session.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L3490-L3509)、[`crates/daemon/src/session_wire/upload.rs`](../crates/daemon/src/session_wire/upload.rs#L79-L236)、[`crates/platform/src/upload.rs`](../crates/platform/src/upload.rs#L28-L147)
- **说明**：仅限制单文件 50 MB，未限制单会话/全局累计大小、文件数与单会话并发数（单连接最多 16 并发 = 800 MB 瞬时暂存），且 `publish()` 后的文件在会话关闭和守护进程停止后永不清理；Unix 端还与 `runtime_fallback` 共用 `/tmp/voytty-{uid}`。
- **修复建议**：增加单会话与全局累计字节/文件数/并发配额；会话结束或守护进程启停时安全清理会话上传目录（或引入 TTL/LRU 淘汰），并将上传目录与 `runtime_fallback` 隔离。

### 守护进程与协议细节

#### M5. `ReplayRegistry`（上限 64 主体）永不清理或淘汰
- **当前状态**　已修复，见[会话管理后续修复](#会话管理后续修复)。
- **位置**：[`crates/daemon/src/session.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L53) / [`session.rs#L2414-L2447`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L2414-L2447)
- **说明**：按 `(device_id, auth_generation)` 建键但设备撤销或重新配对后从不回收，长驻守护进程累计超过 64 次配对/轮换后将永久返回 `ResourceExhausted`。

#### M6. 入站配对持 `AuthorizationWriteGuard` 跨网络发送 `PairAccepted` 与 `shutdown`
- **当前状态**　已修复，见 [P3 后续修复记录](#p3-后续修复记录)。
- **位置**：[`crates/daemon/src/pairing_service.rs`](../crates/daemon/src/pairing_service.rs#L853-L872)
- **说明**：`guard.publish(durable)` 后未立即 `drop(guard)`，慢速配对流会阻塞同设备的即时普通握手或本地 `device revoke` 最长 15 秒。

#### M7. `PairingService::shutdown_until` 与 `accept_until` 竞态泄漏后台任务
- **当前状态**　已修复，见 [P3 后续修复记录](#p3-后续修复记录)。
- **位置**：[`crates/daemon/src/pairing_service.rs`](../crates/daemon/src/pairing_service.rs#L291-L314) / [`pairing_service.rs#L453-L491`](../crates/daemon/src/pairing_service.rs#L453-L491)
- **说明**：检查 `shutdown` 与加锁插入 `self.tasks` 非原子，停机瞬间可能漏掉新 spawn 的任务。

#### M8. `ConnectionBroker` 锁竞争静默丢弃失败连接退役，且 `dial_replacement` 双重扣减 `pending_dials` 许可
- **当前状态**　已修复。复核时确认，备用拨号持有的许可在拨号任务结束时释放。旧实现的问题是，即使已有可用的备用连接，重试也先要求一个空闲许可。[连接管理后续修复](#连接管理后续修复)
- **位置**：[`crates/daemon/src/connection_broker.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/connection_broker.rs#L1580-L1644) / [`connection_broker.rs#L1955-L1965`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/connection_broker.rs#L1955-L1965)
- **原实现**　`retire_failed_candidate` 使用 `try_lock()`，锁暂不可用就放弃退役。`dial_once` 先申请新拨号许可，再取走 `state.replacement`。名额用尽时，即使备用连接已经就绪，重试也会先收到 `ResourceExhausted`。

#### M9. `ConnectionBroker` 后台任务未纳入追踪，路径事件无合并风暴刷库，`peers` 映射只增不减
- **当前状态**　已修复。[连接管理后续修复](#连接管理后续修复)
- **位置**：[`crates/daemon/src/connection_broker.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/connection_broker.rs#L287) / [`connection_broker.rs#L1411-L1436`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/connection_broker.rs#L1411-L1436) / [`connection_broker.rs#L2430`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/connection_broker.rs#L2430) / [`connection_broker.rs#L2667-L2716`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/connection_broker.rs#L2667-L2716)
- **说明**：`quiesce_until` 不等待已脱离的拨号/连接/持久化任务；每次 `PathEvent` 都新起 `tokio::spawn` 写 SQLite，网络抖动易打满 `STORE_COMMAND_CAPACITY`（64）；空闲/撤销对端的 `PeerSlot` 永不回收。

#### M10. `local_ipc.rs` 回包无写超时（可耗尽连接槽位），进度帧写超时后继续写错包破坏帧边界
- **当前状态**　P0 已处理普通单次请求的最终回复。其余早期错误回包和进度消息半帧问题已在 [P3 后续修复](#p3-后续修复记录) 中完成。
- **位置**：[`crates/daemon/src/local_ipc.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/local_ipc.rs#L539-L544) / [`local_ipc.rs#L575-L584`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/local_ipc.rs#L575-L584)、[`crates/daemon/src/connection_progress.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/connection_progress.rs#L106-L119)
- **说明**：同 UID 挂死客户端停止读取即可永久占住 `max_local_connections`；`LocalConnectionProgress` 的 `write_all` 被超时取消留下半帧后，外层继续往同一流写入 `ServiceReply::error` 破坏线缆 framing。

#### M11. `SessionRuntime::takeover()` 失败时状态非原子变更，旧控制者失联且未收到 `LeaseLost`
- **当前状态**　已修复，见[会话管理后续修复](#会话管理后续修复)。
- **位置**：[`crates/daemon/src/session.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L4162-L4225)
- **说明**：在可能失败的 `full_sync` / `reconcile_effect_target` 之前就替换了 `runtime.controller`，出错返回时旧控制者已被丢弃且未发送 `LeaseLost`，缓存摘要也与内存不一致。

#### M12. `OperationCell::wait` 与 `SessionService::close_inner` 忽略调用方 `deadline`
- **当前状态**　已修复，见[会话管理后续修复](#会话管理后续修复)。
- **位置**：[`crates/daemon/src/session.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L690-L706) / [`session.rs#L1337-L1348`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L1337-L1348) / [`session.rs#L2533-L2544`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L2533-L2544)
- **说明**：重放等待无界调用 `Condvar::wait`；`close_inner` 丢弃传入 `deadline` 并硬编码等待 5 秒。

#### M13. `async fn` 中直接调用阻塞同步 I/O（`UnixStream` / `FileLock` / `diagnostics.flush`）
- **位置**：[`crates/daemon/src/lifecycle.rs`](../crates/daemon/src/lifecycle.rs#L128-L130) / [`lifecycle.rs#L635`](../crates/daemon/src/lifecycle.rs#L635)、[`crates/daemon/src/operations.rs`](../crates/daemon/src/operations.rs#L618) / [`operations.rs#L1208-L1461`](../crates/daemon/src/operations.rs#L1208-L1461)、[`crates/daemon/src/handoff.rs`](../crates/daemon/src/handoff.rs#L351-L519)
- **说明**：在单线程 Tokio 运行时中直接执行同步 `UnixStream` 读写、文件锁和最长 2 秒的日志刷盘，阻塞超时器与取消响应。

### 客户端、终端渲染、平台与诊断

#### ~~M14. 系统开机不足 2 秒时诊断工作线程因 `Instant::now() - 2s` 下溢 panic~~（已撤回：复核为误报）
- **复核结论**：在项目锁定的 Rust `1.98.0` 及全部受支持目标上不会下溢 panic——Unix（含 macOS、Linux、iOS、Android）的 `Instant` 使用有符号 `Timespec.tv_sec: i64` 可直接表示跨零负值（macOS 同工具链实测倒推一百年成功）；Windows 的 `std/src/sys/time/windows.rs` 在 QPC 纳秒值上额外叠加了 `u64::MAX / 4` 偏移（rust-lang/rust#156142）。已撤回本项（保留编号占位，详见[误报与修订核查](project-review-2026-11-19-false-positives.md)）。

#### M15. `DesktopPresenter::present_resolved` 每帧重复关开鼠标/焦点模式并重发 DECSCUSR
- **位置**：[`crates/cli/src/terminal_ui/ansi_presenter.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui/ansi_presenter.rs#L290-L314) / [`ansi_presenter.rs#L655-L690`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui/ansi_presenter.rs#L655-L690)
- **说明**：尽管已实现 `write_changed_input_modes`，增量重绘仍每帧先发 `?1003l`/`?1006l`/`?1004l` 再重开 `HOST_INPUT_CAPTURE` 并重发光标样式，打断外层鼠标拖拽、制造虚假 FocusIn 并重置光标闪烁。

#### M16. 回滚缓冲满 2,000 行后，每次 PTY 写入均递增 `history_epoch`，令客户端历史缓存持续失效
- **位置**：[`crates/terminal/src/model.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/terminal/src/model.rs#L518-L530)、[`crates/core/src/viewport_cache.rs`](../crates/core/src/viewport_cache.rs#L385-L397)
- **说明**：满容后即便零行滚动（如提示符打字、原地刷新进度条），`retained == self.scrollback_rows` 恒成立，每次输入都失效桌面/移动端所有未钉住的历史页，引发持续远端拉取。

#### M17. `VoyttyColorState::stack`（XTPOPCOLORS）出栈将全部 262 个色槽钉死为显式覆盖并强制软件闪烁光标
- **位置**：[`crates/terminal/src/colors.rs`](../crates/terminal/src/colors.rs#L72-L73) / [`colors.rs#L123-L135`](../crates/terminal/src/colors.rs#L123-L135)
- **说明**：压栈保存了展平后的快照而非 `overrides`，出栈后永久阻断系统深浅色切换并迫使桌面端退化为 500 ms 定时器软件光标。

#### M18. `user_state::open_read` 读取时申请写权限，每写一条日志均重复整理 4 个文件
- **复核修订**　读取文件时仍申请写权限，写日志时也仍会重复整理四个文件。原文关于缺少 `FILE_SHARE_DELETE` 的归因已撤回。Rust 1.98 在 Windows 上默认设置该共享标志，项目使用的 `configure_secure_open` 只设置自定义打开标志。Windows 上并发读取和轮转的实际行为仍待原生验证。[文件打开逻辑](../crates/platform/src/user_state.rs)
- **位置**：[`crates/platform/src/user_state.rs`](../crates/platform/src/user_state.rs#L432-L434) / [`user_state.rs#L932-L943`](../crates/platform/src/user_state.rs#L932-L943)、[`crates/platform/src/diagnostics.rs`](../crates/platform/src/diagnostics.rs#L227-L251) / [`diagnostics.rs#L313-L328`](../crates/platform/src/diagnostics.rs#L313-L328)
- **说明**　`open_read` 可能因此无法读取属主只读（`0o400`）的文件。`Store::append` 每写一条日志就对 4 个文件执行 `normalize()`，增加了每次写入的系统调用次数。

#### M19. 移动端 `IrohController` 存在 `open_bi` 零退避忙旋与 1 秒周期唤醒/分配及 1 ms RTT 抖动重绘
- **位置**：[`crates/client/src/iroh_controller.rs`](../crates/client/src/iroh_controller.rs#L583-L605) / [`iroh_controller.rs#L992-L1068`](../crates/client/src/iroh_controller.rs#L992-L1068)
- **说明**：`open_bi()` 失败但 `close_reason()` 仍为 `None` 时无退避死循环至 15 s；`IrohSessionIo::read` 空闲时每秒唤醒一次并反复堆/栈分配并零化 16 KiB 缓冲，且 RTT 波动 1 ms 即触发状态事件与状态栏重绘。

#### M20. 上传取消可能截断正在写入的帧或阻塞 5 秒，且 `open_upload` 仅给 5 秒超时
- **位置**：[`crates/client/src/upload.rs`](../crates/client/src/upload.rs#L185) / [`upload.rs#L219-L236`](../crates/client/src/upload.rs#L219-L236)、[`crates/client/src/iroh_controller.rs`](../crates/client/src/iroh_controller.rs#L953)
- **说明**：`select!` 中途取消 `transfer` 后向同一流写入 `Cancel` 可能污染半帧或在断链时卡满 5 秒；冷启动远端上传应使用 15 s `REMOTE_STARTUP_DEADLINE`。

#### M21. `ScopedPanicHook::suppress` 吞掉交互期间所有 `voytty_diagnostics` panic 记录；自动启动权限校验跨平台不均

- **部分修复**　P0 已修复 Linux 目录权限误拒。下列 panic 日志和 Windows 校验问题仍待处理。[P0 修复记录](#p0-修复记录)
- **位置**：[`crates/cli/src/terminal_ui.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui.rs#L260-L273) / [`terminal_ui.rs#L1059-L1079`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui.rs#L1059-L1079)、[`crates/platform/src/autostart.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/platform/src/autostart.rs#L364-L370)、[`crates/platform/src/autostart_windows.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/platform/src/autostart_windows.rs#L34-L49)
- **说明**：
  - `ScopedPanicHook` 将全局 panic hook 替换为 `|_| {}`，使交互式终端内所有崩溃完全丢失诊断记录。
  - Linux 自动启动对 `$HOME` / `~/.config` 严禁 `0o022`，在 Ubuntu/Fedora/Debian 默认 UPG（`umask 0002`、`~/.config` 为 `0o775`）下直接失败；Windows 自动启动则相反，仅检查 `executable.is_file()`（跟随符号链接、未调 `validate_owned_executable`、未拦截控制字符）。

### 发布流水线、安装脚本、Relay 与 Vendored 依赖

#### M22. 发布空位检查遗漏既有 Draft Release，且 `find-green-main-ci.sh` 接受无候选产物的快速 `push` 运行
- **位置**：[`.github/workflows/release.yml`](../.github/workflows/release.yml#L60-L75) / [`release.yml#L279-L294`](../.github/workflows/release.yml#L279-L294)、[`tools/release/operator.sh`](../tools/release/operator.sh#L145-L158)、[`tools/release/find-green-main-ci.sh`](../tools/release/find-green-main-ci.sh#L17-L31)
- **说明**：`releases/tags/$TAG` 对草稿发布返回 404，可能创建重复草稿（参考 `resume-signed.py:116-121` 分页检查）；`find-green-main-ci.sh` 未优先/限定 `workflow_dispatch`，可能选中不含候选产物的快速 `push` 运行导致误报失败或绕过完整验证检查。

#### ~~M23. `release.yml` 与 `ci.yml` 多处 `actions/download-artifact` 漏配 `digest-mismatch: error`~~（已撤回：复核为误报）
- **复核结论**：这四处下载均锁定在 `actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c`（v8.0.1），该版本已将 `digest-mismatch` 默认值设为 `error`，摘要不匹配时默认即 fail-closed 报错退出，无需作为安全/功能缺陷修复（保留编号占位以避免后续 `M24`–`M26` 串号，详见[误报核查](project-review-2026-11-19-false-positives.md)）。

#### M24. Windows 安装脚本（`install.ps1` / `versioned.ps1.in`）多处边界缺陷且缺乏运行时测试
- **位置**：[`install/install.ps1`](../install/install.ps1#L5-L8) / [`install.ps1#L31-L48`](../install/install.ps1#L31-L48)、[`install/versioned.ps1.in`](../install/versioned.ps1.in#L9) / [`versioned.ps1.in#L51-L59`](../install/versioned.ps1.in#L51-L59) / [`versioned.ps1.in#L106-L121`](../install/versioned.ps1.in#L106-L121)
- **说明**：HTTP 200 空响应静默退出 0；在系统 `%TEMP%` 而非目标 `$InstallDir` 暂存（违背 `install/README.md` 契约）且创建目录后才 `Set-Acl`；缺少 `$candidate` 重解析点检查与 `$InstallDir` 目录类型检查；SemVer 正则过松；覆盖 `SecurityProtocol` 禁用了 TLS 1.3；`Path` 拼接可能出现 `;;` 或重复；`install.ps1` 在 CI 中仅有 AST 解析检查，无运行测试。

#### M25. Relay 部署滞后于 `iroh 1.3.0`、缺少容器加固且 CI 未执行真实握手探针
- **位置**：[`deploy/relay/compose.yaml`](../deploy/relay/compose.yaml#L7-L14)、[`deploy/relay/relay.toml`](../deploy/relay/relay.toml#L6-L9)、[`deploy/relay/artifact.sh`](../deploy/relay/artifact.sh#L5)、[`tests/relay/static.sh`](../tests/relay/static.sh#L96-L128)、[`tests/relay/smoke.sh`](../tests/relay/smoke.sh#L50-L53)
- **说明**：Relay 镜像与 `handshake-probe` 仍停留在 `iroh 1.0.3`（而工作区为 `iroh 1.3.0`，且 `vendor/iroh/VOYTTY.md:201` 明确依赖 `iroh-relay 1.3.0` 的连接降级/晋升语义）；`ci-relay` 仅对探针跑 `clippy` 而从未对容器执行真实握手；`compose.yaml` 引用可变 `:latest` 镜像、无 `read_only`/`cap_drop`/`no-new-privileges`/资源上限且 `relay.toml` 完全开放无 `[limits]`，而 `tests/relay/static.sh` 反而阻止加入加固配置。

#### M26. `vendor/iroh` 9 项补丁在 CI 中既未跑上游单测也未跑 3 项真实 TLS Socket 回归，独立 vendor lockfile 绕过 `cargo-deny`
- **位置**：[`justfile`](../justfile#L88-L98)、[`vendor/iroh/VOYTTY.md`](../vendor/iroh/VOYTTY.md#L118-L216)
- **说明**：`just ci-iroh` 仅执行 `--lib voytty_`，漏掉了文档记录的 3 个不以 `voytty_` 命名的 TLS-relay 回归测试以及上游单测；`just ci-dependencies` 未检查 `vendor/noq-proto/Cargo.lock` 与 `vendor/iroh/Cargo.lock`。

---

## 五、低危与信息性发现（Low / Info）

### 调级移入项（保留原编号）

#### M1（保留原编号，现行等级：Low / P2）. 已有授权时 `resolve_normal_confirmation` 可将失败配对报告成功并覆盖本地设备元数据
- **位置**：[`crates/client/src/pairing.rs`](../crates/client/src/pairing.rs#L194-L229) / [`pairing.rs#L327-L351`](../crates/client/src/pairing.rs#L327-L351)、[`crates/daemon/src/pairing_service.rs`](../crates/daemon/src/pairing_service.rs#L637-L670)、[`crates/client/src/iroh_controller.rs`](../crates/client/src/iroh_controller.rs#L651-L696)
- **范围与说明**：普通 ALPN 必须认证票据中预期的远端设备且返回非零授权代次，**未发现给未授权设备增加访问权的证据**；实际影响限于控制器**已有授权**时，可能把失败的配对尝试报告为成功并覆盖本地 `known_devices` 的 `remote_name`/`alias`（以及在未发 `PairProof` 时白耗一次普通 ALPN 拨号）：
  1. `execute_accept` 和 `pair_ticket` 在 `pair_attempt.repair_required == false`（发 `PairProof` 之前的明确失败）时仍无条件拨号普通 ALPN，且 `resolve_normal_confirmation` 在 `Ok(confirmation)` 分支未检查 `repair_required`。
  2. `validate_pair_accepted`（`client/src/pairing.rs:334-338`）明确判定 `host_confirmation_proof` 校验失败时仍返回 `repair_required = true, accepted_generation = None`，使后续 L220-227 的检查被跳过。
- **修复建议**：仅在发完 `PairProof` 后遭遇传输/超时歧义时允许普通 ALPN 恢复（**必须保留最终确认丢失的正常恢复行为及既有 `normal_confirmation` 测试**）；对 `!repair_required` 跳过普通 ALPN 补查，对 `validate_pair_accepted` 明确的密码学/协议错误直接拒绝。

### 其他低危与信息性发现

1. **Windows `StagedUpload::drop` 与 `publish` 在 `File` 句柄仍打开时操作 `.part`**（[`crates/platform/src/upload.rs`](../crates/platform/src/upload.rs#L120-L147)）：Rust 先跑 `Drop::drop` 再析构字段，Windows 上 `remove_dir` 因句柄未关持续报 `ERROR_DIR_NOT_EMPTY` 并泄漏空目录。改为 `file: Option<File>` 并先 `take()`。
2. **Unix 固定共享 `/tmp/voytty-{uid}` 路径允许同机用户预占 DoS**（[`crates/platform/src/upload.rs`](../crates/platform/src/upload.rs#L41-L48)、[`crates/platform/src/user_state.rs`](../crates/platform/src/user_state.rs#L1117-L1119)）：虽 fail-closed 不会劫持，但多用户机上可被他人抢注阻断上传与回退运行目录。
3. **Unix `atomic_create` 使用 `fs::rename` 而非 `hard_link`**（[`crates/platform/src/user_state.rs`](../crates/platform/src/user_state.rs#L882-L889)）：并发调用下不具备严格原子 no-clobber 语义。
4. **过期配对票据明文（含 `offer_key`）驻留守护进程内存永不主动清理**（[`crates/daemon/src/pairing.rs`](../crates/daemon/src/pairing.rs#L770-L776)）：`PairingManager::prune_expired()` 在全仓零调用；仅在下次配对操作时惰性清理。
5. **长期身份私钥加载/创建临时缓冲未 `Zeroizing`**（[`crates/daemon/src/identity.rs`](../crates/daemon/src/identity.rs#L23-L47)）；部分帧编解码与读缓冲零化不一致（[`crates/proto/src/lib.rs`](../crates/proto/src/lib.rs#L1022-L1029)、[`crates/daemon/src/session_wire.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session_wire.rs#L1282)、[`crates/daemon/src/remote_tunnel.rs`](../crates/daemon/src/remote_tunnel.rs#L139)）。
6. **Windows 文件创建与延迟删除微瑕**（[`crates/platform/src/user_state.rs`](../crates/platform/src/user_state.rs#L966-L980)、[`crates/platform/src/windows_security.rs`](../crates/platform/src/windows_security.rs#L141)）：`create_new_executable` 缺 `FILE_FLAG_OPEN_REPARSE_POINT` 与失败回滚删除；`remove_retired_executable` 重试删除前未复查重解析点属性。
7. **帧头预分配最大 8 MiB 与 `finish_unary` 多余字节送入解码器**（[`crates/proto/src/lib.rs`](../crates/proto/src/lib.rs#L963)、[`crates/daemon/src/session_wire.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session_wire.rs#L419-L452)）：收到 4 字节长度头即立即 `Vec::with_capacity(length)`（建议上限 64 KiB 渐进增长）；`finish_unary` 收到非 EOF 字节应立即拒绝。
8. **附着流上任何可恢复子错误（如并发历史窗口超限、陈旧色序）直接断开整个终端附着**（[`crates/daemon/src/session_wire.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session_wire.rs#L1306-L1324) / [`session_wire.rs#L1441-L1507`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session_wire.rs#L1441-L1507)）；`SessionAttachment::detach()` 未唤醒 `SessionActor`（[`crates/daemon/src/session.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L405-L407)）。
9. **终端/CLI/平台细节**：
   - Windows `voytty pair create` 从不显示二维码（[`crates/cli/src/pair_qr.rs`](../crates/cli/src/pair_qr.rs#L10-L26) 未接已引入的 `crossterm::terminal::size()`）。
   - 非折行选区复制在每行末尾填充全宽空格（[`crates/core/src/terminal_selection.rs`](../crates/core/src/terminal_selection.rs#L114-L133)）。
   - `OSC 1` 的 `bounded_text` 未过滤控制字符且按字节截断可能劈裂 UTF-8（[`crates/terminal/src/ingress.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/terminal/src/ingress.rs#L903-L907)）。
   - `voytty_diagnostics::export` 将被过滤的 debug 快照字节误算入 `truncated_bytes`（[`crates/diagnostics/src/reader.rs`](../crates/diagnostics/src/reader.rs#L227)）。
   - Unix `BlockingReader::read` 未重试 `EINTR`（[`crates/platform/src/pty/unix_io.rs`](../crates/platform/src/pty/unix_io.rs#L84-L95)）；Windows `spawn_login_shell_with_identity` 未清理 `TMUX`/`WT_SESSION` 等外层终端环境变量且未用 `build_identity().version`（[`crates/platform/src/pty.rs`](../crates/platform/src/pty.rs#L443-L450)）。
   - `voytty doctor` 存在 `[error]` 仍返回退出码 0（[`crates/cli/src/lib.rs`](../crates/cli/src/lib.rs#L1606-L1617)）；`voytty logs` 带过滤条件无匹配时误报 `"No daemon logs yet."`（[`crates/cli/src/lib.rs`](../crates/cli/src/lib.rs#L1734-L1740)）；`TerminalSignals` 未处理 `SIGTSTP`（[`crates/cli/src/terminal_ui.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/cli/src/terminal_ui.rs#L884-L897)）；`windows_console.rs:107` 空闲 100 ms 轮询窗口尺寸而非消费 `Event::Resize`；`LocalAttachmentEvent::Takeover` 覆盖未确认的同步态（[`crates/client/src/view.rs`](../crates/client/src/view.rs#L1022-L1028)）。
10. **POSIX 安装脚本 `ulimit -f` 重定向日志时触发 `SIGXFSZ`**（[`install/versioned.sh.in`](../install/versioned.sh.in#L146) / [`versioned.sh.in#L201`](../install/versioned.sh.in#L201)）：下载 64 字节签名时 `ulimit -f 1` 会让 `curl` 往已超过 512 字节的重定向 stderr 日志写入时被内核杀死。
11. **依赖与工具链 hygiene**：
    - [`Cargo.toml`](../Cargo.toml#L55) 中 `reqwest` 开启了未使用的 `"blocking"` feature；同时 `distribution.rs` 在 `spawn_blocking` 里额外起了一个 `tokio::runtime::Runtime`。
    - `actions/upload-artifact` 仍停留在基于 Node.js 20 的 v4（[`.github/workflows/ci.yml`](../.github/workflows/ci.yml#L313) 等）。
    - `just ci-policy` 的 `shellcheck` 与 `check-python-syntax.sh` 漏掉了部分脚本（如 `source-policy.sh`、`workspace-version.sh`、`deploy/relay/*.sh`、`tools/licensing.py`）；`tests/workspace-version.sh` 漏检 `client`/`diagnostics`/`terminal`/`tools/release`；`tests/secret-scan.sh` 漏检 `github_pat_` 与 `ENCRYPTED/PGP PRIVATE KEY`。
    - `Cargo.lock` 存在 27 组重复 crate 版本（如 `windows-sys` ×4、`hashbrown` ×3、`nix` ×3）。
12. **文档漂移、18 处死链与仓库残留**：
    - [`docs/development.md`](development.md#L97-L119) 依赖图遗漏 `voytty-client` 和 `voytty-diagnostics`，目标列表遗漏 `aarch64-apple-ios-sim`。
    - [`docs/install.md`](install.md#L97-L110) 仍写“3 个原生目标 / main push 构建候选”；[`docs/mobile-repository.md`](mobile-repository.md#L28-L35) 仍写“9 个资产”（实为 12 个）及“尚未迁移许可证”；[`deploy/relay/README.md`](../deploy/relay/README.md#L14-L16) 与 [`docs/relay.md`](relay.md#L129-L145) 存在已删除脚本说明与真实域名残留；[`docs/roadmap.md`](roadmap.md#L25) 仍将已交付的 Windows/iOS 标为未来事项；[`docs/remove-trellis-plan.md`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/docs/remove-trellis-plan.md) 与空文件 `.cursor/hooks.json` 尚未清理。
    - `docs/verification/multi-device-issues.md`（16 处）与 `docs/verification/multi-device.md`（2 处）存在指向已拆分 `apps/android`、`crates/android`、`tests/e2e/multi-device*.py` 的失效相对链接。
13. **纵深防御备忘（Info）**：`attachment_writer` 发送 `TerminalHostEffect` 未走 `request_context.run_effect`（极窄撤销竞态，[`crates/daemon/src/session_wire.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session_wire.rs#L1743-L1747)）；`PairSecret`/`PairProof`/`PairAccepted` 派生了非常量时间 `PartialEq`（当前校验均走 `ring::hmac::verify`，属未来防呆）；`autostart.rs:235` 通过 `PATH` 调用 `systemctl` 而非绝对路径；`proto` 中多处 `operation_id` 字段声明但服务端未使用（建议文档标明保留或补齐幂等）。

---

## 六、架构与可维护性债务

| 热点 | 现状与量化 | 风险与重构方向 |
| --- | --- | --- |
| **超大单体文件** | `terminal_ui.rs` (7,943 LOC)、`daemon/session.rs` (7,598 LOC)、`session_wire.rs` (5,539 LOC)、`client/session.rs` (4,875 LOC)、`connection_broker.rs` (4,219 LOC)、`proto/lib.rs` (3,854 LOC) | `terminal_ui.rs` 通过 `include!()` 拼接 11 个子文件（甚至把 `windows_console.rs` 包含在 `mod unix` 内），严重削弱模块边界与 IDE/审查体验；建议拆分为规范子模块并将千行级内联测试移至独立 `*_tests.rs` |
| **帧读取与超时工具重复** | 5+ 套独立帧读取循环（零化策略不统一）；9+ 套 `timeout_until`/`timeout_at` 辅助函数；`store.rs` 与 `session.rs` 重复 1 ms sleep 轮询邮箱 | 统一收口到 `voytty_client::framing::FrameReader` 和共享 `DeadlineMailbox`（用 `Condvar` 替代 1 ms 轮询），减少边界行为漂移 |
| **3 个本地补丁分叉（14 项补丁）** | `vendor/noq-proto`（4 项）、`vendor/iroh`（9 项）、`vendor/portable-pty`（1 项） | 上游安全更新合入成本极高；建议持续推动上游 issue/PR 合入、补齐 `vendor/iroh` CI 测试覆盖与独立 lockfile `cargo-deny` |

---

## 七、分阶段修复建议路线图

```mermaid
flowchart LR
    P0["P0 立即处置<br/>(安全止血与主路径修复)"] --> P1["P1 核心稳定性<br/>(并发、终端与跨平台)"]
    P1 --> P2["P2 供应链与纵深加固<br/>(CI/发布/安全边界)"]
    P2 --> P3["P3 架构减债与文档治理<br/>(拆文件、去重、修文档)"]
```

1. **P0（本轮已完成）**　C4、C5、C3 与 M21 的 Linux 目录权限子项已修复，C1 按维护者要求跳过。[提交与验证记录](#p0-修复记录)
2. **P1（核心稳定性与主线门禁，已提交修复）**。各项提交及验证见 [P1 修复记录](#p1-修复记录)。以下保留原始清单。
   - 修复 CI 路径分类器遗漏 `crates/core/src/release.rs` 与 `crates/daemon/src/distribution.rs`（**H16**，已实测确认两处均为 `native=false`）。
   - 补齐 Unix 复现测试并修复 `BlockingReader` 缺 Waker 导致的孙进程有条件等待无界（**C2**，现行等级 High）。
   - 修复终端 `DCS +q` 杀会话（**H2**）、组合字符/超链接脏标记 DoS（**H1**）与未闭合控制串卡死（**H3**）。
   - 修复 `ConnectionBroker` 2 处 `Notify::notified()` 唤醒丢失（**H4**）、`model_thread` PTY 写死锁（**H5**）、`network.rs` `biased` 忙旋与停机泄漏（**H7/H8**）、守护进程单线程冻结/同步阻塞（**H6**）。
   - 修复客户端重连被打断/丢结果/无退避及 5 s 活锁（**H9/H10/H11**）、Kitty 键盘编码（**H12**）、Windows 控制台与 `pair accept` `Ctrl+C` 终端残留（**H13/H14**）。
3. **P2（供应链、发布与安全纵深加固，已提交修复）**。详见 [P2 修复记录](#p2-修复记录)。
   - 移除发布/候选构建中的 `curl | sh` 与 `~/.cargo/registry/src` 缓存（**H15/H17**）、补齐 Draft 空位检查（**M22**；原 **M23** 已撤回）。
   - 收窄已有授权时的 `resolve_normal_confirmation` 行为，保留最终确认丢失的正常恢复用例（**M1**，现行等级 Low）。
   - 加固 Unix `validate_executable_parent`（**M2**）、Windows Named Pipe（**M3**）、上传累计配额与自动清理（**M4/L1**）及 Windows 安装脚本测试（**M24**）。
   - 补齐 `vendor/iroh` TLS 回归与独立 lockfile `cargo-deny`（**M26**），对齐 Relay `iroh 1.3.0` 与容器加固（**M25**）。
4. **P3（架构减债与文档对齐，已提交修复）**。实施和验收见 [P3 修复记录](#p3-修复记录)。原始清单如下。
   - 拆分 `terminal_ui.rs`（移除 `include!()`）、`daemon/session.rs`、`session_wire.rs`、`connection_broker.rs`，收敛重复帧读取与邮箱轮询；修复 `ansi_presenter` 增量模式差量（**M15**）与 `history_epoch` 误失效（**M16**）；同步文档并清理 18 处历史死链与残留文件。
