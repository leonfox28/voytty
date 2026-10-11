# 开发、PR 与发布流程优化（2026-10-10）

本文记录 2026-10-10 对开发、PR、发布流程的审查结论、已落地的改动、实测效果和
剩余事项。日常命令以 [development.md](development.md) 和
[releasing.md](releasing.md) 为准；本文是这一轮优化的依据和记录。

## 各阶段运行什么

| 阶段 | 运行内容 | 说明 |
| --- | --- | --- |
| 编辑中 | `cargo clippy -p <crate>`、`cargo test -p <crate> <filter>` | 只覆盖正在改的 crate |
| commit 前 | 受影响的检查；可选 `just check-fast` | 不要求每个 commit 重跑 workspace；`check-fast` 保留为较全面的检查入口 |
| push 前 | 产品/测试/构建/CI 改动跑 `just check`；Android 及其共享依赖、UniFFI、构建工具变化加 `just android-check`；纯文档可只跑 `just ci-policy` | 组合 `ci-policy`、`ci-unix true`、`ci-dependencies`、`relay-static`，与 CI 共用 recipe |
| PR CI | policy、dependencies、`CI gate` 始终运行；非纯文档另跑 relay、Rust 三平台、Android、三个 native 候选（只构建不上传） | 同一 PR 的新提交会取消旧运行；分类不确定时跑全量 |
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
  PR 二进制不能离开 runner。`tests/release/candidate-fixture.sh` 仅覆盖候选查询与筛选，
  不覆盖组装；2026-10-11 补充的 Rust 集成测试使用合成输入执行完整 unsigned 组装，
  真实跨平台候选的集中组装仍在 main 上进行。
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

## 后续事项及当前状态

1. **不稳定测试 `two_daemon_owners_reuse_endpoint_for_pair_and_normal_confirmation`**
   （`crates/daemon/tests/two_daemon_transport.rs:423`，Linux arm64，PR #66 首次运行）。
   `wait_for_primary` 等待的是 `peer_observation`，而断言读取的 `observe().snapshot()`
   连接计数是单独发布的，可能滞后。2026-10-11 已改为在原 deadline 内等待该
   snapshot 的 watch 通知，保留所有计数断言。
2. **pairing 日志计数断言**，已记录在
   [session-continuity.md](verification/session-continuity.md)。当前代码已将该断言隔离到
   子进程，原记录保留为历史失败证据。
3. **缓存 `target/noq-tests` 与 `target/iroh-tests`。** 两者每个平台合计约 2–2.5 分钟，
   原先仍在冷编译。2026-10-11 已添加独立依赖缓存，裁剪时使用各自 manifest；
   三平台 hosted 收益需待合并后 main 填充缓存再实测。
4. **评估 nextest。** 它能并行运行测试二进制，需先确认现有测试兼容。
5. **纯文档 PR 跳过重型 job**，2026-10-11 已实现保守白名单与配套 gate 测试；
   `docs/relay.md` 是测试输入，仍跑全量。main 和手动运行始终跑全量。
6. **补 AGENTS.md。** 它在移除 Trellis 时一并删除了，目前 agent 分支没有关于 push 前
   检查和发布入口的说明。2026-10-11 已恢复简短入口与检查边界。
7. **每周定时运行 `just ci-dependencies`**，2026-10-11 已增加独立定时 workflow，
   提前发现新 RUSTSEC 公告；PR/main 仍保留阻断检查，因此新公告仍可能使 PR 失败。
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

## 2026-10-11：进一步缩短运行时间

- 纯文档白名单为根目录 `README.md`、`AGENTS.md` 和 `docs/**/*.md`，排除
  `docs/relay.md`。比较整个 PR 测试合并树与 base，使用 NUL 分隔且关闭重命名折叠，
  防止只看最后一次提交或漏掉移入 docs 的源码删除。缺历史、无法分类则跑全量。
- 稳定 `CI gate` 根据明确分类要求每个 job 为 success 或预期的 skipped；分类器、
  policy、dependencies 始终必须成功。回归测试覆盖失败、取消、缺失、意外跳过、
  文档/源码混合改动和 main 错误分类。
- Native 与 Android 统一采用“main 冷构建后保存裁剪依赖、PR 可恢复”的模式。
  native 缓存不含 workspace/patched vendor 包、测试或正式产品二进制，PR 仍执行
  glibc/macOS 下限、自检和 CLI 默认路径检查。main 只查询 cache key，从不恢复这些
  发布编译缓存。
- noq/iroh 独立测试 target 的第三方依赖加入缓存，key 绑定各自 manifest/lockfile、
  编译器、OS 和架构；测试范围没有减少，patched 包仍重编。
- Rust workspace 测试新增完整合成候选组装，覆盖 archive、prepare、unsigned verify、
  确定性、禁止覆盖、APK/installer 篡改和混合源码身份。复用现有测试编译，不新增 job。
- 此轮未引入自动测试重试，也未放宽发布候选冷构建、签名和安装验证要求。
  新缓存首次需要 main 填充；不能把缓存未命中的首轮 CI 当作稳定收益。

本地恢复验证使用独立 checkout（基线 `7507f19`）、CI 相同的关闭 incremental/
dev debuginfo 设置，先冷编译，再裁剪、归档、更新源码 mtime、恢复到相同路径并重编。
下列为编译耗时，vendor 使用 `--no-run`，不计测试执行；缓存为 gzip 大小，GitHub
实际采用的压缩格式与网络速度不同，不应直接作为 hosted CI 提速比例。

| 编译目标 | 冷构建 | 恢复后构建 | 编译 crate 数（冷 / 恢复） | 压缩缓存 | 本地解包 |
| --- | --- | --- | --- | --- | --- |
| noq 测试 | 16.77 s | 4.70 s | 125 / 1 | 92.66 MiB | 0.76 s |
| Iroh 测试 | 29.97 s | 13.60 s | 332 / 4 | 215.01 MiB | 1.53 s |
| macOS arm64 native | 约 103 s | 46.17 s | 358 / 12 | 264.80 MiB | 1.78 s |

Native 冷构建耗时取 Cargo 的 `1m 43s` 日志，其余构建/解包为实测 wall time。
Native 冷构建与恢复后 SHA-256 均为
`ea801349961fac0d5dc669976cc80301bd1d7c5ceab3e897219c4fb9a3a262be`。
三份本机缓存合计约 572 MiB；三平台实际体积和 hosted critical path 仍需观察。

恢复实验同时发现 Cargo 1.98 对显式 `--target-dir` 清理要求 `CACHEDIR.TAG`，而部分
目录缓存不包含这个文件。裁剪现在通过 `CARGO_TARGET_DIR` 定位已知缓存根，仍仅让
Cargo 清理 metadata 找出的 path 包；新增真实 Cargo fixture 覆盖缺 marker 的恢复
场景、多个 profile、path 包清除和第三方库保留。

首轮 hosted CI（`0328164`）全绿，用时 7m40s。Android 是 critical path（7m23s），
其中已命中 Rust 编译缓存，host/arm64 编译分别约 19/60 秒，其余主要是 Gradle。
因此继续补充 Gradle task-output 缓存：main 冷构建后保存、PR 才恢复，单项上限
1 GiB；JVM Test 与所有 lint* task 明确禁止复用检查结果，Rust/identity 自定义 task
也保持不可缓存。Gradle 根据每个 task 的实际输入内容判断编译输出能否复用。

本地 Gradle 验证使用独立 task-output cache，首次完整构建后停止 daemon、`clean`
删除项目输出，再从缓存构建 debug/release APK 并运行两套 lint 与 JVM 测试：

| Task | 首次执行 | 恢复后 | 恢复行为 |
| --- | --- | --- | --- |
| compileDebugKotlin | 49.091 s | 0.135 s | FROM-CACHE |
| compileReleaseKotlin | 45.945 s | 0.108 s | FROM-CACHE |
| lintAnalyzeDebug | 76.300 s | 54.871 s | 实际执行 |
| lintAnalyzeRelease | 64.890 s | 48.688 s | 实际执行 |
| testDebugUnitTest | 2.093 s | 0.708 s | 实际执行，11 项通过 |

共 50 项 task 从缓存恢复，55 项执行，缓存约 57 MiB。debug 与 unsigned release APK
在两轮构建中分别逐字节一致。总构建时间从 4m20.90s 到 1m14.64s，但这也包含 Cargo
本地热构建和 Gradle 初始化的收益，不能全部归因于新 Gradle 缓存；上表单列实际
Kotlin 编译与检查 task 的耗时。hosted 稳态收益仍需 main 首次填充后验证。
