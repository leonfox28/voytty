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
