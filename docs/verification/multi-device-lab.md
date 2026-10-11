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

### 6. 修正后重跑 cold 场景（09:11:18–09:13:22 UTC／17:11–17:13）

重新打开 A／B 的详细诊断窗口后执行同一筛选，`--run-id lab-rebuild-cold-02`。产品制品未变
（运行器只在 Mac 侧执行，修正不影响已安装的二进制与 APK）。

- 配对：C 与 A 1.36 秒、C 与 B 2.03 秒通过；A↔B 沿用上一轮已有配对。
- 8 行全部通过，审计零问题：

  | 行 | `connect_seconds` |
  | --- | --- |
  | A←B Direct／Relay | 12.293／2.831 |
  | A←C Direct／Relay | 1.808／9.507 |
  | B←A Direct／Relay | 12.639／2.307 |
  | B←C Direct／Relay | 1.801／6.903 |

**环境重建完成**：A／B／C 已建立、安装同源候选 `b5ef999`、通过 preflight 并完成全部配对，
可用于后续定向矩阵。三端保持运行（`--keep-running`）。

两行 Linux Direct 接近十五秒预算，按控制端详细日志逐次拆分 Relay 拨号（脚本
`checks/lab-rebuild/relay_dials.py`，结果 `relay-dials-cold-01.json`／`relay-dials-cold-02.json`）：
两行各有一次所需 Relay 的拨号在 TLS 阶段 10 秒无进展，以 `dial_deadline` 结束；随即重拨
约 0.47 秒连上。其余拨号 TLS 0.09–2.1 秒完成。归因与后续处理记入 [问题清单](multi-device-issues.md) 的 R02。

### 旧环境清理（13:55 UTC／21:55）

经用户确认，已删除旧 VM `zterm-md-20261003t024103z-a`／`-b`（Stopped，约 20 GB）与指向
已删除目录的 AVD 记录 `zterm-md-20261003t024103z-c`。当前环境 `zterm-md-20261011t084006z`
保留；用户自己的其他模拟器（非运行器创建）未动。

### 7. P37 候选重建与 cold ×3（09:47–10:00 UTC／17:47–18:00）

提交 P37 后重新 `build`：源码 `58adff3`、工作区无改动，A／B 二进制 `1e04d8b6…81878c`、
调试 APK `29904588…663c33c`，测试 APK 未变，preflight 通过（`checks/lab-rebuild/build-02.log`）。
续开 A／B 详细诊断后执行 `run --scenario cold --repeat 3 --soak-seconds 0 --keep-running --run-id p37-cold-01`：

- **21 通过／3 失败**，审计零问题（`p37-cold-01/acceptance-audit.json`）。
- 失败一：第 1 轮 A←B Relay，CLI `deadline_exceeded: timed out opening remote Session tunnel`（R02）。
- 失败二：第 2 轮 B←C Relay，Android 打开主机 `deadline_exceeded`，会话列表同样超时（R02）。
- 失败三：第 2 轮 A←C Direct，错误仅为 `'hosts'`，是测试器问题 T34：Android 桥先写 `ready`
  再发布首个 `status.json`，运行器看到 `ready` 后立即读状态得到空值，`snapshot()` 返回 `{}`，
  `connect()` 取 `hosts` 时 KeyError。已改为先发布状态再写 `ready`（需重建测试 APK）。不计作产品结果。

Relay 拨号拆分（`checks/lab-rebuild/relay-dials-p37-cold-01.json`）：对冲共触发 12 个拨号周期，
4 个在该周期内连上（+3.8、+4.1、+6.4、+7.1 秒；前两例为对冲救回，单次尝试将等到 10 秒），
4 个两次尝试都失败，均为同一 Relay 在同一时段连续不可达（一次以 `tls` 错误结束，其余为
`dial_deadline`），4 个到行窗口结束仍未结束。两次 R02 失败都发生在这种 Relay 整体不可达的
时段：对新连接而言该 Relay 数十秒内任何尝试都没有进展，而主机早先建立的 Relay 连接仍在线。
P37 能救回单个连接卡死，不能解决这种 Relay 整体不可达，原因另行排查。

### 8. Relay 可达性探测（10:07:32–10:17:41 UTC／18:07–18:17）

为区分产品与网络路径，在 Mac 和 A 内同时运行只读探针（`checks/lab-rebuild/relay_probe.sh`）：
每约 5 秒对四个 n0 Relay 各发一次 HTTPS `GET /`（服务端返回 404），记录 TCP 连接、TLS 完成
与总耗时，12 秒超时。不创建产品连接，也不读取产品身份。

| Relay | Mac：失败／TLS 中位数／最大 | A：失败／TLS 中位数／最大 |
| --- | --- | --- |
| aps1-1 | 6/70／1.19／5.99 秒 | 6/63／1.26／6.91 秒 |
| use1-1 | 2/70／0.55／4.78 秒 | 3/63／0.55／10.01 秒 |
| euc1-1 | 2/70／0.40／0.54 秒 | 3/63／0.39／0.54 秒 |
| usw1-1 | 0/70／0.69／1.50 秒 | 0/63／0.71／1.74 秒 |

- aps1-1 在 10:10:38–10:12:11 连续失败（TCP 连接 12 秒超时与 `SSL_ERROR_SYSCALL`），Mac 与 A
  同时出现，说明是共享的出口路径（Mac 的代理／出口）而不是 VM 或产品。
- euc1-1、use1-1 的失败是零星的单次超时，不成簇。
- 经 n0 DNS 只读查询，A、B、C 发布的 home relay 均为 `euc1-1`，即本实验中所有连接都依赖它。

结论：路径上同时存在两类问题。零星的单连接卡死可由 P37 对冲救回；成簇的整段不可达期间，
任何新连接在十五秒内都无法到达该 Relay，若它恰是主机的 home relay，客户端重试无法解决。
原始数据 `relay-probe-mac-01.jsonl`、`relay-probe-vm-a-01.jsonl`。

### 9. T34 修正后重建与 cold ×3（10:20–10:37 UTC／18:20–18:37）

重建源码 `95aab2f`（含 P36、P37、T33、T34），测试 APK 变为 `8ec870ca…`，preflight 通过
（`checks/lab-rebuild/build-03.log`）。运行期间在 Mac 上同时运行探针
（`relay-probe-mac-02.jsonl`），以便与失败同一时刻对照。`run-id p37-cold-02`：

- **23 通过／1 失败**，审计零问题；T34 未再出现。
- 唯一失败：第 2 轮 A←C Direct（10:33:25 开始），Android 打开主机 13.6 秒后
  `transport_unavailable`。
- 慢行集中在 10:33–10:36 且都以 A 为主机（A←B Direct 6.6、A←B Relay 10.3、
  A←C Relay 13.8、A←C Direct 8.4 秒）；同期以 B 为主机的行都在 1.5–3.4 秒。

同一时刻三方证据：

1. A 在 10:33:07 因路径模式切换按设计重启 daemon，10:33:09 拨 Relay X，TLS 2.36 秒，
   10:33:12 就绪。
2. 同一进程在 10:33:34 改选另一个 home relay Y（同进程内 Relay 标签不同，期间没有 X 的
   失败事件），10:33:35 就绪。查询时 A 发布的 home relay 为 `euc1-1`。
3. C 在 10:33:29 连上 A 当时发布的 Relay，通过了 TLS 与认证，但始终收不到 pong，
   10:33:34 以 `relay_unconfirmed_lost`／`ping_timeout` 断开；C 自己的另一条 Relay 连接未受影响。
4. 探针：aps1-1 的 TLS 在 0.1–3.5 秒间大幅波动，10:32:41 失败；euc1-1 在 10:33:06 起出现
   12 秒连接超时；use1-1、usw1-1 同期正常。

解释：经 Mac 代理到公共 Relay 的路径质量剧烈波动，主机重启后最初按延迟选出的 home relay
在数十秒内又被改选，而客户端正连向最初发布的那一个，且那条路径只建立了连接、不转发数据。
这属于网络路径（问题清单 R06 的边界）叠加 home relay 改选，不是能在本机复现的产品逻辑缺陷。

### 10. 区分代理 TUN 与直连线路（11:38–11:53 UTC／19:38–19:53）

用户确认 Bettbox 对 `iroh.link` 走直连、不用 fake-ip，默认 DNS 为 1.1.1.1／8.8.8.8 DoH。
核对：四个 Relay 域名经 Bettbox、经 8.8.8.8 和 A 内解析的 IP 完全一致，DNS 无问题；但去往
这些 IP 的路由都进入 Bettbox 的 utun5（由它在本机完成 TCP 握手后再自行直连），默认路由才是 en5。

同时运行两路探针 15 分钟：经 Bettbox（默认路由）与绑定 en5 绕过 Bettbox。

| Relay | 经 Bettbox 失败 | 绕过 Bettbox 失败 | 绕过时 TCP 连接 >1 秒（SYN 重传） |
| --- | --- | --- | --- |
| use1-1 | 4/96 | 6/87 | 10/87 |
| usw1-1 | 0/96 | 0/87 | 13/87 |
| euc1-1 | 10/96 | 9/87 | 0/87 |
| aps1-1 | 4/96 | 4/87 | 25/87 |

- 绕过 Bettbox 的失败率与经过它时相当，失败时刻有一部分重合（13 次两边 10 秒内都失败，
  11 次只在经 Bettbox 时失败，13 次只在绕过时失败）。问题在本地网络到这些 Relay 的直连线路
  （运营商／国际出口的丢包与阻断），不是 Bettbox 的规则或 DNS。三端共用的 home relay
  `euc1-1` 此期间约 10% 的新连接失败。
- 用户提供的 Bettbox 日志在 19:48:29 记录 `dial tcp 87.99.151.110:443: i/o timeout`（直连到 use1-1
  无应答），与此一致。经 Bettbox 时产品只能看到 TLS 无进展或被重置，看不到外层连接失败。
- 同一日志中对 `euc1-1:80` 的 `connection refused` 来自 Iroh 每次网络探测中的 captive portal
  检查（`http://<relay>/generate_204`）；绕过 Bettbox 同样被拒绝，说明 Relay 不提供 80 端口。
  Relay 连接只用 443，这些条目不影响连接。
- 原始数据：`relay-probe-tun-03.jsonl`、`relay-probe-en5-03.jsonl`、对比 `relay-probe-compare-03.txt`。

### 11. Relay 走代理对比与 herdr 本地网络权限（12:20–13:45 UTC／20:20–21:45）

用户在 Bettbox 的 `personal-proxy` 规则集加入 `AND,((DOMAIN-SUFFIX,iroh.link),(NETWORK,TCP))`，
使 Relay 的 TCP 443 走 `📲 个人服务` 代理组，UDP（7842 地址发现及直连打洞）仍按 `personal`
规则直连。Bettbox 连接列表确认：TCP 443 经新加坡节点，UDP 7842 为 DIRECT。

**探针（3 分钟，经新加坡节点）**：四个 Relay 均 0/31 失败，TLS 中位数 0.10–0.56 秒、最大
0.31–0.85 秒；同一方法的直连基线为 0–10% 失败、最大 1.9–7.9 秒（`relay-probe-proxy-04.jsonl`）。

**环境事故 E10**：其间 C 的两行 Direct 连续两轮保持单一 Relay 路径（`proxy-cold-01`、
`proxy-cold-c-direct-02`），切回直连后仍复现（`direct-cold-c-direct-03`），说明与代理规则无关。
排查结果：本会话及其启动的进程（含 08:40 启动的模拟器）对任何局域网地址发包都返回
`No route to host`，且看不到 ARP 邻居；绕过命令沙箱无变化；同一命令在普通 Ghostty 标签页成功、
在 herdr 标签页失败。herdr 服务脱离 Ghostty 由 launchd 托管，macOS 按 herdr 自身判定“本地网络”
权限；该权限在 10:36–12:34 UTC 之间失效（原因未确认，系统设置中无 herdr 条目、也未弹窗）。
用户重启 herdr 服务后新标签页恢复，但已在运行的进程不会恢复；由用户在新 herdr 标签页
`adb emu kill` 后执行 `multi-device.py start` 重启模拟器即恢复。之后：

- `proxy-cold-c-direct-04`：C 的两行 Direct 通过，连接 1.76／2.32 秒，0.02–0.03 秒内切到直连。
  确认 Relay 只让 TCP 走代理不影响直连。
- `proxy-cold-05`（cold ×1，Relay 走代理）：**8/8 通过**，审计零问题，连接 1.23–4.19 秒，
  无 Relay 拨号失败。Relay 路径 RTT 中位数 199–755 ms，此前直连 Relay 时同类行为 478–2582 ms。

样本量小（各一轮），只作为方向性证据。今后若 C 的 Direct 行突然只剩 Relay 路径，先在本会话
检查 `python3 -c "import socket;socket.socket(2,2).sendto(b'x',('10.0.0.1',9))"` 是否报
`No route to host`，再排查产品。
