# 三端测试环境重建记录

本文记录 A／B（Linux VM）与 C（Android 模拟器）测试环境的位置、重建步骤和过程中的问题。
操作入口见 [运行说明](../../tests/e2e/README.md)，验收问题见 [问题清单](multi-device-issues.md)。
时间均为 UTC，括号内为上海时间。

## 实验根目录

运行器、主机准备、诊断续开和审计脚本共用一个根目录：

```sh
LAB=${ZTERM_MULTI_DEVICE_ROOT:-${XDG_STATE_HOME:-$HOME/.local/state}/zterm-verification/multi-device}
```

其中 `environment/` 保存环境清单 `environment.json`、C 的 AVD 磁盘、构建制品和日志；
`venv/` 是运行器的 Python 环境；每次运行的证据目录及 `checks/` 也在根目录下。

此前根目录是仓库内的 `target/verification/multi-device/`。2026-10-10 17:08（上海时间）
`target/` 被重建，环境清单、venv、全部证据，以及建在 `environment/avd` 下的 C 模拟器
一起丢失（问题清单 E09）。因此根目录移到仓库之外：它不再随 `cargo clean` 删除，也不会
被 `build` 打进源码归档（`build` 归档全部未忽略的工作区文件）。

同时，矩阵行的 `evidence` 改为记录绝对路径（故障计数和 soak 连接失败本来就是绝对路径），
审计仍兼容旧的相对仓库路径记录。运行器 47 项、主机准备 3 项协调回归通过。

## 2026-10-11 重建

### 1. 旧环境核对（08:20 UTC／16:20）

- A／B：`zterm-md-20261003t024103z-a`／`-b` 在 `~/.lima`，均为 Stopped，磁盘占用 16 GB／3.7 GB。
- C：`~/.android/avd/zterm-md-20261003t024103z-c.ini` 仍在，但其 `path` 指向已删除的
  `target/verification/multi-device/environment/avd`，模拟器实际已不存在。
- 运行器 `environment.json`、venv、日志及 `checks/` 均已删除。
- 用户选择全新重建 A／B／C，而不是保留旧 VM 再手工补环境清单。旧 VM 与悬空的 AVD 记录
  暂不删除，等待确认。

工具：Lima 2.2.0、Android emulator 37.1.11.0、system image `android-36;google_apis;arm64-v8a`、
OpenJDK 17.0.20.1、Python 3.12.13。Mac 剩余磁盘约 453 GiB。

### 2. 根目录迁移（08:30 UTC／16:30）

分支 `test/rebuild-multi-device-lab` 叠在 PR #70（P36）之上，因此本次构建的候选包含 P36。
新根目录下新建 venv 并安装 `requirements-multi-device.txt`；P36 当天的证据从旧位置移入
`checks/ordered-fixes/moved-host-handshake/`。

### 3. create（08:40:06–08:41:00 UTC／16:40–16:41）

`"$LAB/venv/bin/python" tests/e2e/multi-device.py create`，退出码 0，约 54 秒。

- 环境 ID `zterm-md-20261011t084006z`；VM `zterm-md-20261011t084006z-a`／`-b`
  （Ubuntu 24.04 ARM64，VZ，4 CPU／6 GiB／60 GiB，vzNAT + user-v2）。
- AVD `zterm-md-20261011t084006z-c`，磁盘在 `$LAB/environment/avd`；串号 `emulator-5586`。
- 运行器自动启动三端，并确认 adb root。Lima 基础镜像已在缓存，无需重新下载。
- 日志：`checks/lab-rebuild/create-01.log`，逐条命令在 `environment/logs/commands.jsonl`。

### 4. build 与 preflight（08:50:39–08:54:47 UTC／16:50–16:54）

`"$LAB/venv/bin/python" tests/e2e/multi-device.py build`，退出码 0，约 4 分钟，结束时自动执行 preflight 并通过。

| 项目 | 值 |
| --- | --- |
| 源码版本 | `b5ef999`（含 P36），工作区无改动（`modified_sources` 为空） |
| 源码归档 | `source-26d360bd627fa26d.tar.gz` |
| A／B Linux 二进制 SHA-256 | `37a7a90c…2e5cdf` |
| 调试 APK SHA-256 | `fe466c9e…c8de00b` |
| 测试 APK SHA-256 | `602d8232…80c77ab` |
| Android 应用 UID | 10213 |
| A／B 地址（lima0） | 192.168.64.4／192.168.64.5 |

- A 上安装 build-essential 等依赖和 Rust 1.98.0，在 A 内编译 `zterm-cli`（调试版，2 分 07 秒），
  复制到 A／B 后各自 `setup --profile official-n0` 并等到 Network Online。
- Mac 上构建调试 APK 与测试 APK 并安装到 C。
- preflight：A／B 二进制哈希一致；Mac→A／B、C→A／B、A↔B 的 ping 与 UDP 六个方向全部通过；
  C 为 adb root，应用 UID 正常。
- 日志：`checks/lab-rebuild/build-01.log`、`environment/logs/linux-build.log`、
  `environment/logs/android-build.log`，preflight 证据为 `environment/preflight.json`。

### 5. 首次运行：cold 场景（08:56:01–08:57:01 UTC／16:56–16:57）

先在 A／B 上 `zterm logs debug on` 打开 15 分钟详细诊断窗口，再执行
`run --scenario cold --repeat 1 --soak-seconds 0 --keep-running --run-id lab-rebuild-cold-01`（8 行）。

- 运行开始时自动配对：A↔B 两个方向通过（0.92 秒／0.56 秒）；C 与 A、C 与 B 的配对均失败。
- Linux 控制端 4 行全部通过，`connect_seconds`（含 CLI 自动启动 daemon）：
  A←B Direct 1.777、A←B Relay 2.843、B←A Direct 2.335、B←A Relay 1.962 秒。
- Android 4 行被配对前提阻塞（blocked），不是产品失败，也没有覆盖 Android 冷启动。

C 配对失败的原因是测试器问题（问题清单 T33）：运行器经 `run-as` 把票据写入应用私有目录的
`files/multi-device-ticket.txt`，但不创建 `files/`。新装应用直到 08:56:07 才由 ProfileInstaller
首次写入 `files/profileInstalled`，而写票据发生在 08:56:04–05，因此 `cat` 以退出码 1 失败。
旧模拟器上 `files/` 早已存在，所以此前没有暴露。手工核对：同一写法在 `files/` 存在后可写入
（探测文件写入后已删除）。

修正：写票据前先 `mkdir -p files`。新增回归在没有 `files/` 的临时应用目录中执行设备端脚本，
修正前以 `No such file or directory` 失败、修正后通过；协调回归 48/48。

证据：`$LAB/lab-rebuild-cold-01/`（`pairing.json`、`results.json` 及各行目录）、
`checks/lab-rebuild/run-cold-01.log`、`ticket-dir-before-01.log`、`ticket-dir-after-01.log`、
`coordinator-tests-01.log`。
