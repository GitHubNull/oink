# Linux X11 阶段 2 收口状态与后续开发 · 跨代理交接（第五轮）

- 日期：2026-11-20
- 仓库：`/home/wgh/dev/cross_platform_app/oink`
- 分支：`main`；HEAD `c5b94e0e64411cb29e0b437f64320300e97a519d`，与 `origin/main` 同一 SHA（`git ls-remote origin refs/heads/main` 已核验）
- 工作树：干净；`apps/`、`crates/` 自 `d8b2932` 起零改动；无 tag、无 Release
- 宿主：Linux Mint 22.3 / Cinnamon / Xorg / x86_64；12 核 / 31 GiB；**无触摸板、无触摸屏**（`xinput` 仅 USB 键鼠与 PiKVM）
- 交接时无后台子代理、无定时任务、无探针进程在运行

## 0. 一句话状态

阶段 2 的机制探针（A/A2/B/C/D/E）、FUSE、真实麦克风、物理按键与锁屏/会话切换证据均已收口；**未通过**的阶段 2 门槛只剩触摸板/触摸/精细滚动（本机无硬件，登记 BLOCKED）。产品阶段 3–8 **尚未开始**。本文件中的“通过”一律指隔离探针/夹具通过，不代表产品功能已支持 Linux。

## 1. 本轮已完成与未完成

| ID | 范围 | 状态 | 证据 / 下一步 |
|---|---|---|---|
| P2-A/A2 | 隔离环境、依赖、资源限额、规范二进制 | 完成 | `doc/linux-x11/plan.md` 实施记录；`.tools/fresh-20261117/` |
| P2-B | 真实应用隔离/身份/文本矩阵、XDND 双端 | 完成 | `dnd-all.log` 13 PASS（sessions=2） |
| P2-C | INCR、文件/目录、URI staging、取消与负面层 | 完成 | `incr.log` 7 PASS；`file-peer.log` 14 PASS；矩阵 7 行收口 |
| P2-D | 源重启、peer 重连、controller 恢复、代际拒绝 | 完成 | D 波记录；两代真实 selection 交付 |
| P2-E | 真实 toolkit 输入（GTK/Qt） | 完成 | `input-toolkit.log`：`rows=8 failures=0` |
| P2-F1 | FUSE 用户终端验收 | 完成 | `.release-tools/linux-probes/fuse-user.log`；不重跑 |
| P2-F2 | 物理按键、滚轮、锁屏/会话切换 | 完成 | 按键 34/35、按钮 17/17、motion 183、`reason=focus_lost`、`focus_out=1` |
| P2-F3 | 真实麦克风端到端 | 完成 | 桥/消费者同跑记录；`hardware-mic-user.log`；不重跑 |
| P2-F4 | 触摸板、触摸、精细滚动 | **BLOCKED（硬件不可得）** | 需真实设备 + 新 XI2 采集层；不得改写为 PASS |
| P2-F5 | Chromium 合成源拖拽起始 | BLOCKED（平台限制） | `browser_source_synthetic_blocked` 绊线保留 |
| P2-F6 | Firefox 文件源方向 | BLOCKED（未实现，已登记） | 若产品声明支持 Firefox 文件源，必须先实现并补矩阵 |
| P2-GATE | 阶段 2 总门槛 | **未通过** | 仅剩 F4；用户 2026-11-20 决策“不放宽门槛” |
| P3–P8 | 产品实现（入口/输入/显示/剪贴板/音频/协议/打包） | **未开始** | 见 §6 |

## 2. 接手先做的只读核验

```bash
cd /home/wgh/dev/cross_platform_app/oink
git status --short --branch
git log -3 --format='%h %s'
git ls-remote origin refs/heads/main
python3 build-support/linux_hardware_acceptance.py status   # 空 env 可跑，仅读日志摘要
```

期望：HEAD 与远程均为 `c5b94e0`；`status` 退出码 0 并输出 input/mic/fuse 三类日志摘要。任何一项不符，先停下核对，不要重跑证据覆盖现场。

## 3. 已验证证据（本机 `.release-tools/` 被 gitignore，仅磁盘存在）

### 3.1 提交与推送

- 代码：`1f33699` `test(linux): fix hardware input collector and capture physical evidence`
- 文档：`c5b94e0` `docs(linux): record phase 2 physical input evidence and gate status`
- 仅 `build-support/` 与 `doc/` 变更；`apps/`、`crates/` 零改动；无 tag/Release。

### 3.2 离线全量

```bash
cd build-support
python3 -B -m unittest discover -p 'test_linux*.py'
```

结果：**289 项 OK（约 41.1s）**。

### 3.3 原生四套件（新批次 `phase2-gate-20261120`）

批次目录：`.release-tools/linux-probes/phase2-gate-20261120/`

- `dnd-all.log`：13 PASS，`sessions=2`
- `incr.log`：7 PASS
- `file.log`：**红证据保留**（首跑漏 `--peer-x11`，`staging suite requires runner --peer-x11`）——禁止删除或覆盖
- `file-peer.log`：14 PASS（`--peer-x11` 复跑）
- `input-toolkit.log`：`{"failures": 0, "result": "INPUT_TOOLKIT_DONE", "rows": 8}`
- 每份日志唯一 `native_test_summary`：`probe_exit=result=remaining_mounts=0`，`services_ok/cleanup_ok/mount_check_ok=true`
- 产物 SHA-256：
  - `dnd-roles`（六源 roles/native/selection/incr/stream/file）`213e26bb0b0ba2b9bade3f1b12d0e8d4a8677d735cf5452ea82254e195de63cb`（与 canonical 一致）
  - `input-toolkit-route` `8af9ad1ce019c094abc6cacbdbe3b37454a4e1ad59bdfaa8f5e771bb4de14992`
  - `dnd-qt-fixture` `5765b6708be1b7253d714198140b6d0d99abe1865b5c4c7e5e044f57f90cfe35`（Qt 6.12.0）

### 3.4 用户物理证据

`hardware-input-user.log` 最终记录（started_epoch 1795184648.75 = 2026-11-20 22:24:08）：

```json
{"type": "input_stop", "reason": "focus_lost", "key_press": 34, "key_release": 35,
 "button_press": 17, "button_release": 17, "motion": 183, "focus_out": 1}
```

同轮 `input_start` 含 `"mapped": true, "focus_self": true`。判定：物理按键 ✅、物理滚轮/移动旁证 ✅、锁屏/会话切换 ✅。滚轮计数不升级为触摸/精细滚动 PASS。

### 3.5 红→绿证据（全部保留，不覆盖）

`phase2-gate-20261120/` 内：`collector-deadline-red.log` / `collector-deadline-green.log`、`collector-keys-smoke2.log`（红：`focus_self=true` 但 0 键）、`collector-keys-smoke3.log` / `collector-keys-smoke4.log`（绿）。接手者若重跑，必须使用新文件或新批次目录。

## 4. 环境手册

### 4.1 工具与路径

- Qt kit：`$PWD/.tools/fresh-20261117/Qt/6.12.0/gcc_64`
- XRes sysroot：`$PWD/.tools/fresh-20261117/sysroot`
- 规范二进制：`.tools/fresh-20261117/bin/dnd-roles`（禁止使用 `.release-tools/linux-probes/dnd-roles` 的 11-17 旧产物，协议不兼容）
- 阶段 2 探针只需 Python 3 + cc + pkg-config + Qt kit；Rust/Node 工具链到阶段 3 才需要，按仓库锁与 `doc/linux-x11/plan.md` §1 使用，不静默改版本
- 网络：宿主有本地 HTTP 代理 7890；`cargo`/`git`/`gh` 等可 `https_proxy=http://127.0.0.1:7890`。git 走 GnuTLS 被 RST 时用 `git@github.com:` SSH 显式 URL。离线探针不需要网络

### 4.2 依赖检查

```bash
export OINK_QT_KIT="$PWD/.tools/fresh-20261117/Qt/6.12.0/gcc_64"
export OINK_XRES_SYSROOT="$PWD/.tools/fresh-20261117/sysroot"
bash build-support/linux-deps.sh --check --native-tests --toolkit-tests
```

缺失即停并报告；系统安装交用户，不 sudo。

### 4.3 限额构建（不覆盖历史批次）

`bounded` 是当前 shell 函数，不是仓库脚本；限额为 MemoryMax=4G、CPUQuota=200%、TasksMax=1024（runner 经批准）：

```bash
bounded() { systemd-run --user --scope --quiet -p MemoryMax=4G -p CPUQuota=200% -p TasksMax=1024 timeout 180 "$@"; }
out="$PWD/.release-tools/linux-probes/phase2-gate-<新日期>"; mkdir -p "$out"
bounded cc -std=c11 -O2 -Wall -Wextra -Werror -o "$out/dnd-roles" \
  build-support/linux_dnd_roles.c build-support/linux_dnd_native.c build-support/linux_dnd_selection.c \
  build-support/linux_dnd_incr.c build-support/linux_dnd_stream.c build-support/linux_dnd_file.c \
  $(pkg-config --cflags --libs gtk+-3.0 x11 xi xtst)
bounded cc -std=c11 -O2 -Wall -Wextra -Werror -o "$out/input-toolkit-route" \
  build-support/linux_input_toolkit_route.c $(pkg-config --cflags --libs x11 xi xtst) -lXau
bounded python3 -B build-support/linux_input_toolkit_qt_build.py --output "$out/dnd-qt-fixture" \
  --kit "$PWD/.tools/fresh-20261117/Qt/6.12.0/gcc_64"
sha256sum "$out/dnd-roles"   # 必须复现 213e26bb…63cb
```

若 `dnd-roles` 因缺 XRes 头/库失败，追加：

```bash
-I"$OINK_XRES_SYSROOT/usr/include" \
-L"$OINK_XRES_SYSROOT/usr/lib/x86_64-linux-gnu" -lXRes \
-Wl,-rpath,"$OINK_XRES_SYSROOT/usr/lib/x86_64-linux-gnu"
```

产物 mtime 必须晚于全部输入；Qt 夹具与控制器字段必须同批重建，旧二进制不得混用。

### 4.4 原生四套件（串行）

```bash
python3 -B build-support/linux-native-test.py --peer-x11 --timeout 150 -- \
  python3 -B build-support/linux_dnd_scenarios.py --roles "$out/dnd-roles" --suite all
python3 -B build-support/linux-native-test.py --timeout 150 -- \
  python3 -B build-support/linux_dnd_incr_scenarios.py --roles "$out/dnd-roles"
python3 -B build-support/linux-native-test.py --timeout 150 -- \
  python3 -B build-support/linux_dnd_file_scenarios.py --roles "$out/dnd-roles" --suite all
python3 -B build-support/linux-native-test.py --timeout 180 -- \
  python3 -B build-support/linux_input_toolkit_scenarios.py \
  --route "$out/input-toolkit-route" --qt-fixture "$out/dnd-qt-fixture"
```

注意：`file` 套件**必须**带 `--peer-x11`；其余按上表。每份日志校验唯一 summary 与业务行，不以退出码单独判定。

### 4.5 用户物理入口（仅用户自己的普通终端）

```bash
python3 build-support/linux_hardware_acceptance.py status          # 空 env，只读
OINK_HARDWARE_USER=1 python3 build-support/linux_hardware_acceptance.py input-collector --seconds 300
```

窗口出现后先点击窗内（标题应为 `focus:yes`），打字/滚轮后按 `Ctrl+Alt+L` 锁屏，采集器失焦自动退出。用户只说“跑完了”，代理自行读取 `.release-tools/linux-probes/hardware-input-user.log` 尾部核对，不让用户描述现象。麦克风/FUSE 本轮不重跑；如需重跑，配方见 [af-waves §6](2026-11-20-linux-x11-phase2-af-waves.md)。

## 5. 已知坑与平台限制

1. X11 事件掩码是协议位域，不是事件号：`KeyPressMask=1<<0`、`KeyReleaseMask=1<<1`、`ButtonPressMask=1<<2`、`ButtonReleaseMask=1<<3`、`PointerMotionMask=1<<6`、`StructureNotifyMask=1<<17`、`FocusChangeMask=1<<21`。误写 `1<<KeyPress` 会选中 ButtonPressMask，按键永远 0。
2. 裸窗必须设 `WM_DELETE_WINDOW`，否则 WM 关窗走 `XKillClient` 掐断整个 X 连接且零日志。取焦必须等 `MapNotify`（WM 重父化前不可取焦，`XSetInputFocus` 抛 BadMatch，Xlib 默认处理会退出进程），并装非致命 `XSetErrorHandler`、用 `XGetInputFocus` 回验/轮询。
3. 截止时间一律 `time.monotonic()`，墙钟只写日志；日志逐行 `flush`，否则运行期间不可见。
4. Mint/Cinnamon 锁屏是 `Ctrl+Alt+L`；`Super+L` 打开 Looking Glass（用户实测核实）。
5. 私有音频栈无会话管理器：pulse 流永不自动链接，`--target` 无效、节点不自动建端口。唯一通路是 core pipewire socket + `pw-cli set-param PortConfig` + `pw-link` 显式连；`pw-cli`/`pw-link` 必须 `stdin=DEVNULL` 且有超时。
6. 读子进程 `stderr` 前先判空（`stderr=DEVNULL` 时为 `None`）；操作者主动 SIGTERM 不得计为设备错误。
7. `.release-tools/linux-probes/dnd-roles`（11-17 旧产物）与新协议不兼容（`invalid report shape`）；只用 canonical 或同批重建。
8. `pkill -f <模式>` 会匹配并杀掉自身 shell；查进程用 `pgrep -af '[l]inux_...'` 这类括号写法，或交给任务管理器。
9. Xephyr + metacity 是既定私有环境（无 Xvfb）；原生串行，不以不限额重试处理卡顿。`--inside-scope`、`require_alive`、清理与挂载检查不可绕过。
10. Chromium 无视 XTEST 拖拽起始；`browser_source_synthetic_blocked` 绊线保留，未来若放行会强制升级为 FAIL。Firefox 目标只接受 `text/plain`；nemo 源需真实图标坐标、`gvfsd` 激活与窗类 `Nemo`。
11. contacts `0x020b` 产品契约未声称端到端；不要把它当作已支持能力。
12. 单文件 ≤500 行是硬门禁；`build-support/test_linux_dnd_scenarios.py` 维护名单，新增源码要入表。超过先拆分，不豁免。
13. 旧批次与红证据是审计材料：不删除、不覆盖、不改写结论。一次重跑成功不能抹掉已出现的失败。

## 6. 后续工作与门槛决策

### 6.1 阶段 2 收口只有两条合法路径

- 路径 A（硬件可得）：在带触摸板/触摸屏的机器上补 XI2 原始触摸/滚动采集层（现采集器只订阅核心键鼠/移动事件，精细滚动未分类），私有环境先验证，再由用户上真机采集；证据入新批次后重评门槛。实现前先确认目标设备与内核/XI2 输入形态。
- 路径 B（显式例外）：用户书面决定接受该三项为 BLOCKED 并明确记录例外，代理才可更新计划中的门槛结论。**代理不得自行把 BLOCKED 改成 PASS，也不得自行放宽门槛。**

在用户给出决定前，不进入产品阶段 3–8。

### 6.2 若门槛关闭，产品实现顺序（`doc/linux-x11/plan.md` §3–§8）

1. 阶段 3：Linux 入口、后台、Tauri/WebKitGTK 生命周期、托盘/窗口/单实例。
2. 阶段 4：`oink-input` Linux 采集/注入/XKB/会话锁定；`oink-display` XRandR；`oink-locator` 覆盖层与中文渲染。
3. 阶段 5：`oink-clipboard` Linux selection/TARGETS/INCR、原生 MIME、DnD backend、FUSE 文件桥、Nemo 粘贴为文件。
4. 阶段 6：`oink-audio` Linux libpulse 播放/捕获/设备/虚拟麦克风（StartPermit 需求门控）。
5. 阶段 7：协议/设置/前端 Linux 分支、XDG autostart、诊断、本地包更新入口。
6. 阶段 8：`build.py`/`package.py` Linux target、DEB + 可解压包、安装/升级/卸载验收、README 更新。

当前产品代码事实（接手时复核过）：

- `apps/oink/src-tauri/Cargo.toml:37-43`：Tauri 依赖仅限 macOS/Windows
- `apps/oink/src-tauri/src/main.rs:37-45`：非 macOS/Windows 直接打印“界面只支持 macOS 与 Windows”并失败
- `apps/oink/src-tauri/build.rs:5-6`：非 macOS/Windows 提前返回
- `apps/oink/src-tauri/src/shell.rs:69-76`：窗口请求只有 Windows/macOS 执行路径
- `apps/oink/src/settings/model.ts:86`：非 Windows 一律当 Mac
- `crates/oink-input`、`oink-clipboard`、`oink-display`、`oink-audio` 无 Linux 后端（见 `doc/linux-x11/plan.md` Context）

阶段 2 探针通过不代表上述任何产品能力已实现；不得用探针结果对外声明“Linux 已支持”。

### 6.3 待用户确认项

- 阶段 2 门槛路径 A 还是 B。
- 是否提供带触摸硬件的测试机；若提供，具体设备型号/桌面会话。
- Windows 双机互通与安装包验收的时间窗口（阶段 8 后）。
- 任何 commit/push/tag/Release、Windows 安装或发布动作都需要**新的明确授权**；此前授权仅覆盖已推送的 `1f33699`/`c5b94e0`。

## 7. 用户红线与禁止事项

- 严禁向宿主 `DISPLAY=:0` 注入合成键鼠、grab 真实键盘、接入用户浏览器 profile/登录态；合成输入仅限私有 Xephyr。真实桌面的物理/锁屏操作交用户。
- 私有 HOME/XDG/Xauthority/DBus/Pulse/PipeWire、认证、进程归属、超时与失败保留不可绕过；不能绕过 `--inside-scope`、`require_alive`、清理与挂载检查。
- 构建/原生限额 4 GiB、CPU 200%；原生串行；不以不限额重试处理卡顿。
- 不 sudo、不装系统包、不重试代理 FUSE、不启用 helper/uinput、不改用户组/udev/防火墙/X server 权限。
- 不记录真实按键内容、剪贴板/音频正文、凭证；诊断只用测试随机数据与聚合数值。
- 阶段 2 只改必要的 `build-support/` 与实施记录；不动产品、工具链锁、CI。
- 保留红证据、修根因、补回归后再完整运行；不把一次重跑成功当根因修复。
- 用户报告的现象视为事实，不要求重复验证；需要新证据时换层级并说明理由。
- 用户偏好：中文沟通；产品 UI 默认中文且可切英文；不做简化实现，严格按规格；状态汇报按层给真实覆盖（骨架/已实现），不把未写的大块包装成“就差一步”。

## 8. 验收标准（DoD）

- 阶段 2：F4 通过（真实触摸设备证据）或用户书面例外；F5/F6 保持登记；四套件与离线全量可复现；红证据完整。
- 产品阶段 3–8：按 `doc/linux-x11/plan.md` 各阶段门槛逐项验收；`apps/`、`crates/` 产品路径有真实后端与集成测试；不以 mock、空列表、假成功或隐藏功能替代。
- 任何交付：`git diff --check` 通过；单文件 ≤500 行；零告警（含存量与新增分列）；工作树范围与授权一致；跨机验收在用户 Windows 机器完成并留存证据。

## 9. 交接记录

| 日期 | 代理 | 范围 | 结果 | 产物 |
|---|---|---|---|---|
| 2026-11-20 | 上一代理（Qoder） | 阶段 2 F 波补证 + 总门槛回归 + 物理按键/锁屏证据 | 完成除触摸类外全部收口；阶段 2 未通过；产品未开始 | 提交 `1f33699`、`c5b94e0`；证据见 §3；本文件 |
