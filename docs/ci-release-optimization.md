# 开发、PR 与发布流程优化（2026-10-10）

本文记录 2026-10-10 对开发、PR、发布流程的审查结论、已落地的改动、实测效果和
剩余事项。日常命令以 [development.md](development.md) 和
[releasing.md](releasing.md) 为准；本文是这一轮优化的依据和记录。

## 各阶段运行什么

| 阶段 | 运行内容 | 说明 |
| --- | --- | --- |
| 编辑中 | `cargo clippy -p <crate>`、`cargo test -p <crate> <filter>` | 只覆盖正在改的 crate |
| commit 前 | `just check-fast` | source policy、`ci-policy`、workspace Clippy |
| push 前 | `just check`；改了 `crates/android` 或 `apps/android` 时加 `just android-check` | 组合 `ci-policy`、`ci-unix true`、`ci-dependencies`、`relay-static`，与 CI 共用 recipe |
| PR CI | policy、dependencies、relay、Rust 三平台、Android、三个 native 候选（只构建不上传）、`CI gate` | 同一 PR 的新提交会取消旧运行 |
| main push | PR CI 的全部内容，加上传 native 与 Android 候选、组装 unsigned candidate | main 运行从不被取消；编译缓存只由 main 保存 |
| 发布 | `just release-prepare VERSION`（可直接在 feature PR 上运行，省一轮 CI），再 `just release VERSION PR` | tag 复用 main 的 exact-SHA candidate；签名需 `release` 环境审批 |

## 审查时的基线与问题

基线为 main fbc1439，数据来自约 75 次 PR 运行和约 90 次 main 运行：

- PR CI 约 16 分钟。最慢的 Rust / macOS arm64 用时 15:48：clippy 3:07、workspace
  测试 6:40、noq 1:07、iroh 2:16、smoke 1:51。main CI 约 12 分钟，release.yml 3–4 分钟。
- 本地 10 核 Mac：冷启动 Clippy 44 秒，冷启动全部测试 2 分 41 秒。

发现的问题：

1. **Rust 编译缓存从未保存。** 裁剪后仍有 3.1–5.2 GiB，超过 1 GiB 上限，每个 PR 的
   Rust job 都在冷编译。
2. **不稳定测试造成实际损失。** `daemon_autospawn.rs:601` 让 v0.1.36 发版前的 main
   运行耗时 97 分钟。
3. **本地与 CI 的 recipe 漂移。** `just check` 手抄了 `ci-unix` 和 `ci-relay` 的命令。
   vendor/iroh 的 fmt 检查放在 `ci-iroh` 里，三个平台各跑一次；CI 没装 rustfmt 的问题
   直到 PR #60 才暴露，本地被 `rust-toolchain.toml` 掩盖。
4. **secret scan 区分大小写**，匹配不到 `userToken`、`dbPassword` 这类驼峰命名的赋值。
5. **native 发布构建只在合并后运行。** glibc 2.28、macOS 13.0 下限或 release 自检的
   问题要到 main 才发现，届时 main 没有可发布的 candidate。
6. **smoke 重编。** `cargo run -p zterm-cli` 的特性组合与测试不同，要重编约 151 个 crate。
7. **main 运行可能被取消。** `cancel-in-progress` 对 main push 也生效，连续合并会取消
   前一个 commit 的 main 运行。
8. **Android job 没有任何缓存。** 每次从源码编译 cargo-ndk（约 40 秒），Rust 依赖冷编译
   两遍：host bindgen 约 2 分钟，arm64 release 约 2.5 分钟。

## 已落地的改动

| PR | 改动 |
| --- | --- |
| #61 | Rust 矩阵关闭 dev debuginfo，裁剪时一并清掉 path 包（workspace 与 patched vendor），上限 2 GiB，缓存 key 升到 v3。`just check` 改为组合 hosted recipe，所有 fmt 检查和 secret scan 只在 `ci-policy` 跑一次，并拆出无 Docker 的 `relay-static`。secret scan 不区分大小写并识别 JWT/JWE。只在 PR 上取消旧运行 |
| #62 | 编译缓存上限改为 2.5 GiB；超限时发出 workflow warning，不再静默跳过 |
| #63 | Android job 恢复共享的 Cargo 下载缓存，并缓存 cargo-ndk、Gradle `modules-2` 和编译后的 Rust 依赖（编译缓存只给 PR 恢复）。host bindgen 构建关闭 debuginfo。`trim-rust-cache.sh` 支持多种 target 布局 |
| #64 | 启动器拉起 daemon 后，把单次 250 ms 探测超时视为"仍在启动"，由 5 秒启动预算判定失败；新增回归测试 |
| #65 | bounded-stop 测试期限从 40 ms 改为 150 ms，仍低于 portable-pty 约 200 ms 的 SIGHUP→SIGKILL 宽限 |
| #66 | 三个 native 候选在每次运行时都构建，并纳入 `CI gate`，只有 main 上传。无参数 CLI smoke 改在 release 二进制上运行，`ci-unix` 只保留 docs 参数。同一 PR 带上 v0.1.37 版本提交 |

## 关键取舍

- **不加自动重试。** 已定位的 CI 独有失败中，两次是真实竞态并已修复（d961452、
  be1945e），一次是启动器 bug（#64），一次是测试期限过紧（#65）。自动重试会把这类问题
  藏起来，所以不稳定测试逐个定位修复。
- **PR 的 native 构建不上传。** 带 source commit 的二进制能通过
  `require_official_distribution_build`，这项检查保护 `update` 和 `uninstall`，未审查的
  PR 二进制不能离开 runner。因此 PR 不覆盖归档和 manifest 组装：policy 中的
  `tests/release/candidate-fixture.sh` 仍覆盖这部分，真实组装在 main 上进行。
- **main 的 Android 候选冷编译。** 编译缓存只给 PR 恢复，与 native 候选一样，发布产物
  不使用编译缓存。本地对比中，恢复构建与冷构建的 `libzterm_android.so` 逐字节一致；
  以后若想让 main 也用缓存，可以参考这一结果。
- **smoke 改为运行 release 二进制。** `--internal-release-self-check` 不覆盖无参数时
  真实默认路径下的 `observe()`，所以保留无参数运行，改在三个 native 候选上执行。原先
  只在两个平台的 debug 构建上跑。
- **不缓存 path 包。** Cargo 按源码 mtime 判断 path 包是否过期，新 checkout 总会重编它们，
  缓存只浪费空间。
- **`.codex/config.toml` 精确路径豁免。** 该文件按维护者决定保留在仓库中。豁免只覆盖
  通用赋值和 token 形状规则；同一文件里出现可识别的 key 仍会被拦截。

## 实测效果

编译缓存体积（裁剪后 / 压缩后）：

| 缓存 | 裁剪后 | 压缩后 |
| --- | --- | --- |
| Rust / macOS arm64 | 1.54 GiB | 435 MiB |
| Rust / Linux arm64 | 1.50 GiB | 406 MiB |
| Rust / Linux x64（多编 docs） | 2.02 GiB | 542 MiB |
| Android（三种布局） | 1.50 GiB | 417 MiB |

本地模拟（相同 checkout 路径，裁剪 → 存档 → 新 checkout → 恢复）：

- **Rust：** clippy / 测试构建 / CLI 构建从 40 秒 / 49 秒 / 20 秒（430 / 336 / 111 个
  crate）降到 11 秒 / 25 秒 / 12 秒（15 / 15 / 12 个 crate）。
- **Android：** `native.py` 从 93 秒（630 个 crate）降到 35 秒（19 个 crate），`.so`
  逐字节一致。

PR CI 用时（#64 到 #66 各次运行的范围）：

| Job | 优化前（PR #60） | 优化后 |
| --- | --- | --- |
| Rust / macOS arm64 | 15:48 | 5:29–8:06 |
| Rust / Linux x64 | 13:34 | 4:54–6:27 |
| Rust / Linux arm64 | 9:35 | 5:41–6:02 |
| Android | 9:04（常见 9–14 分钟） | 5:13–7:24 |
| native 候选（新增，并行） | 只在 main 上运行 | 3:42–5:28 |
| PR 整体 | 约 16 分钟 | 约 8–9 分钟 |

## 剩余事项

1. **不稳定测试 `two_daemon_owners_reuse_endpoint_for_pair_and_normal_confirmation`**
   （`crates/daemon/tests/two_daemon_transport.rs:423`，Linux arm64，PR #66 首次运行）。
   `wait_for_primary` 等待的是 `peer_observation`，而断言读取的 `observe().snapshot()`
   连接计数是单独发布的，可能滞后。
2. **pairing 日志计数断言**，已记录在
   [session-continuity.md](verification/session-continuity.md)。
3. **缓存 `target/noq-tests` 与 `target/iroh-tests`。** 两者每个平台合计约 2–2.5 分钟，
   仍在冷编译。预计可省 1–1.5 分钟，未实测。
4. **评估 nextest。** 它能并行运行测试二进制，需先确认现有测试兼容。
5. **纯文档 PR 跳过重型 job**，main 仍跑全量。
6. **补 AGENTS.md。** 它在移除 Trellis 时一并删除了，目前 agent 分支没有关于 push 前
   检查和发布入口的说明。
7. **每周定时运行 `just ci-dependencies`**，让新的 RUSTSEC 公告不再随机打断无关 PR。
   可选用 Dependabot 跟踪已 pin 的 action 和 crate。
8. **`actions/upload-artifact` 的 Node.js 20 弃用警告。** 当前 pin 的版本基于 Node.js 20，
   运行时会出现这条警告。
9. **删除 `.cursor/hooks.json`**，这是 Trellis 遗留的空文件。

## 运维注意事项

- main 保护要求 PR 分支包含最新的 main。两个 PR 并行打开时，第一个合并后，第二个要先
  update-branch 并重跑 CI 才能合并。
- 曾出现 `CI gate` 的步骤全部完成，但 job 状态停在 `IN_PROGRESS`、PR 显示 `BLOCKED`。
  这是 GitHub 侧的状态问题，单独重跑这个 job 即可恢复。
- 编译缓存超限不会让 CI 失败。裁剪脚本发出 workflow warning 并跳过保存，之后的运行
  继续恢复上一次保存的条目。看到这条 warning 时检查体积，必要时调整上限或提高 key 版本。
- 版本提交会改变 `Cargo.lock`，所以每次发布后 main 会用新 key 重新保存编译缓存，旧条目
  由 GitHub 按 LRU 淘汰。
