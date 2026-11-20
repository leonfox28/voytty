# 2026-11-19 项目审查误报核查与共识修订

核查基准为 `main` 提交 `9ad6b81`（Rust `1.98.0`）。可从修复清单中划去 `M23` 整项、`M14` 整项，以及 `H4` 中关于 `AcceptCell::wait_until` 的子项；`H4` 涉及的另外两处 `ConnectionBroker` 逻辑保留。文末同步记录双方达成一致的调级与证据边界修订（保留原始编号与交叉引用）。[原审查报告](project-review-2026-11-19.md)

## M23：下载产物的摘要不匹配时，工作流默认会失败

原报告认为，部分下载步骤省略 `digest-mismatch: error`，会让内容校验失败的产物只触发警告。核对 CI 和发布工作流，报告列出的四处下载都使用同一提交锁定的 `actions/download-artifact` v8.0.1。[CI 工作流](../.github/workflows/ci.yml#L614-L618) · [发布工作流的两处下载](../.github/workflows/release.yml#L194-L203) · [发布前下载](../.github/workflows/release.yml#L257-L260)

该版本将 `digest-mismatch` 的默认值设为 `error`。只要检测到某个产物的实际摘要与预期不符，下载步骤最终就会报错退出。显式填写 `error` 和沿用默认值的行为相同，M23 可以关闭。[上游参数定义](https://github.com/actions/download-artifact/blob/3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c/action.yml#L42-L46) · [上游报错逻辑](https://github.com/actions/download-artifact/blob/3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c/src/download-artifact.ts#L233-L242)

## H4：配对等待已覆盖检查结果与开始等待之间的通知

原报告认为，配对可能恰好在 `AcceptCell::wait_until` 检查结果之后、首次轮询之前完成，导致等待方漏掉通知。等待方实际先调用 `notified()` 创建等待对象，再检查共享结果。完成方先写入结果，再调用 `notify_waiters()`。[配对完成与等待代码](../crates/daemon/src/pairing_service.rs#L1131-L1159)

项目锁定的 Tokio 1.53.3 保证，等待对象从创建起就能收到 `notify_waiters()` 的广播，即使尚未被轮询。如果配对已经完成，等待方会直接读到结果。如果配对在检查之后完成，已创建的等待对象会收到广播。因此，这条路径无须补加 `enable()`。[锁定版本](../Cargo.lock#L4373-L4375) · [Tokio 1.53.3 的通知语义](https://github.com/tokio-rs/tokio/blob/tokio-1.53.3/tokio/src/sync/notify.rs#L525-L531)

H4 中的 `run_dial_worker` 和 `promote_candidate` 都先检查连接状态，之后才创建等待对象，两处确认保留在原报告 `H4` 中。[拨号逻辑](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/connection_broker.rs#L1856-L1875) · [候选连接逻辑](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/connection_broker.rs#L2378-L2404) · [等待函数](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/connection_broker.rs#L3437-L3444)

## M14：Rust 1.98 各受支持目标的 `Instant` 减 2 秒不会下溢

原报告认为，若系统开机不足 2 秒，诊断工作线程初始化处的 `Instant::now() - Duration::from_secs(2)` 会下溢 panic。[诊断工作线程初始化](../crates/diagnostics/src/recorder.rs#L372-L375)

核对项目锁定的 Rust `1.98.0`（[工具链固定文件](../rust-toolchain.toml)）标准库实现，所有受支持目标均不会在此处下溢：

- **Unix 目标（macOS / Linux / iOS / Android）**：`std::sys::time::unix::Instant` 包装 `Timespec`，其 `tv_sec` 为有符号 `i64`，`checked_sub_duration` 直接支持跨零负值；macOS 同工具链实测从 `Instant::now()` 倒推一百年成功。
- **Windows 目标**：`std::sys::time::windows::Instant::now()` 在由 QPC 换算出的纳秒值上额外叠加了 `u64::MAX / 4`（约 146 年）的起始偏移（对应 rust-lang/rust#156142），减去 2 秒不会触底。

因此 `M14` 在当前工具链与全部受支持目标上可撤回关闭（若日后改为 `Option<Instant>`，仅属不依赖标准库内部偏移约定的风格整理，非现行缺陷）。

## 同期达成一致的调级与证据边界修订（非误报）

下列条目并非误报，但在复核中对其现行等级、触发范围或证据口径做了明确校正，均已在 [原审查报告](project-review-2026-11-19.md) 中保留原编号并同步更新正文位置、统计与路线图：

| 保留编号 | 现行等级 / 批次 | 复核共识与证据边界 |
| --- | --- | --- |
| **C1** | Critical / P0 | 公开仓库追踪 [`.codex/config.toml`](../.codex/config.toml#L1-L3) 与 [`tests/secret-scan.sh`](../tests/secret-scan.sh#L5-L8) 扫描豁免均已证实；令牌当前是否仍有效未知（不探测、不转贴凭据），建议立即轮换、移出追踪并取消豁免。 |
| **C4 / C5** | Critical / P0 | 传统输入单按 `Esc` 滞留缓冲、systemd `WorkingDirectory` 被 `unit_quote` 加字面双引号（`enable`/`status` 仅查注册文件）均已证实，继续优先修复。 |
| **C3** | High / 优先处理（P0–P1 靠前） | 原 Critical 调至 High 但靠前处理。区分两条静态控制流：（1）`device_revoke_reply` 中后台数据库工作（`store.revoke`）可能已提交、但外层仍在 `.await` 其结果时被取消，该时序才会出现 SQLite 与内存状态不一致；恢复轮询后到同步 `guard.publish(Revoked)` 之间不再有新的 `.await`，取消若发生在发布之后则另影响后续 `close_remote`/`detach_remote_principal_until` 清理（外层超时亦违背 `PairOutcomeUnknown` 契约）；（2）[`local_ipc.rs`](../crates/daemon/src/local_ipc.rs#L539-L546) 停机已提交但回包写入/关闭失败时跳过 `stop_sender`，进程可能继续存活并拒绝新建会话。按 [`session.rs`](https://github.com/leonfox28/voytty/blob/9ad6b81fbc301633fbe72c718c75f0f11739ae68/crates/daemon/src/session.rs#L1982-L2014) 与 `1105–1116` 代码可再次正常 `stop` 恢复；故障与恢复均尚无现成运行记录。 |
| **C2** | High / P1 | 原 Critical 调至 High / P1。在后代仍持有从端且读端未收到 EOF、EIO 或其他退出事件的前提下，`BlockingReader` 无唤醒机制且 `finalize` 等待 `join` 可能无界；原证据为静态路径分析，持续时长与跨平台（含 Windows ConPTY）表现待实测，建议先补 Unix 复现测试再修。 |
| **H16** | High / P1 | 直接运行 [`tools/ci/workflow-policy.py`](../tools/ci/workflow-policy.py#L45-L53) 的 `path_plan` 实测确认，`crates/core/src/release.rs` 与 `crates/daemon/src/distribution.rs` 均选出 `native=false`，列为 P1。 |
| **M1** | Low / P2 | 原 Medium 调至 Low / P2。普通 ALPN 必须认证预期远端且授权代次非零，未发现给未授权设备增加访问权的证据；实际范围为已有授权时可能把失败配对报告为成功并覆盖本地名称/别名。修复时必须保留最终确认丢失经普通 ALPN 恢复的正常用例（现有两项 `normal_confirmation` 用例通过）。 |
