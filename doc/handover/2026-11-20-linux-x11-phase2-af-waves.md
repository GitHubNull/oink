# Linux X11 阶段 2 交接：A2–F 分波实施与 F 用户实证

日期：2026-11-20。仓库：`/home/wgh/dev/cross_platform_app/oink`。

**一句话状态：A2 依赖获取、B/C/D/E 机制门槛与 F 的 FUSE、真实麦克风用户实证已完成；物理按键（本轮采集 0）、锁屏/切换、触摸板/触点、精细滚动仍缺用户证据，阶段 2 未通过，产品阶段 3–8 未开始。**

本文件是第四轮交接，不覆盖前几轮快照。完整目标以 [Linux X11 计划](../linux-x11/plan.md) 为准；A–F 的原始范围与禁止事项仍见 [第三轮交接](2026-11-17-linux-x11-phase2-qt-continuation.md)，本文件只记录增量与当前状态。

## 1. 基线、git 状态与环境

- 分支 `main`；本轮代码提交 `8dbc9ab432ce3ca7d626ecf46764cd1b96245041`（`test(linux): add phase 2 application, transfer and recovery probe waves`，42 个 `build-support/` 文件），文档提交 `65279d1e7706a583254a4947c4c319a0903b0a22`（`docs(linux): record phase 2 wave evidence and hand over hardware gates`）。
- 两个提交均已推送 `origin/main`，推送后 `git ls-remote origin refs/heads/main` 与 `65279d1` 一致；`git diff --name-only 19bc294..65279d1 -- apps crates` 为空，`git diff --check` 通过。未推送 `upstream`，未创建 tag/Release，未改动产品、锁文件或 CI。
- 本次交接状态更新随后独立提交并推送；其自身 SHA 用 `git log -1 --format='%H %s' -- doc/handover/2026-11-20-linux-x11-phase2-af-waves.md` 查询，不在文件内循环记录。该次 push 只覆盖本轮交付，不构成后续持续授权。
- 沿用上轮核实：本机 Rust `1.99.0`，仓库 Node `v24.14.1`；会话系统 Node `v24.22.1` 不可冒充仓库工具链。
- 本轮 Qt kit/XRes 使用 A2 新树：`.tools/fresh-20261117/Qt/6.12.0/gcc_64`、`.tools/fresh-20261117/sysroot`。
- 资源限额：构建/原生运行 4 GiB、CPU 200%；runner `TasksMax` 经会期内批准为 1024（其余限额不变），原生串行。
- `.tools/`、`.release-tools/` 均为本机忽略目录，不随 git 交付；换机须重跑 A2 获取与全部重建。

## 2. 任务总表

| ID | 范围 | 状态 | 证据/下一步 |
|---|---|---|---|
| A1 | 本地 Qt6/XRes 依赖检查 | 已完成（保持回归） | 27 项依赖测试；`linux-deps.sh` 本地优先/缺失回退/不完整失败 |
| A2 | fresh clone 固定版本依赖获取 | **PASS** | 全新 `.tools/fresh-20261117` 从固定来源安装两组件；`continuation-20261117-b/{deps-fresh-install,deps-fresh-check,deps-fresh-recheck,fresh-qt-build,fresh-qt-matrix}.log`；50 项依赖/生命周期测试（含红→绿） |
| A3 / A4–A5 | C 产物基线与 Qt 文本矩阵 | 已完成 | 见第三轮交接；六源零告警重建、Qt/GTK 矩阵 |
| B | 真实应用隔离、身份与文本矩阵 | 已完成 | Firefox 3 行、nemo 2 行、Chrome 1 行 PASS；Chrome 源方向 3 行 BLOCKED（平台限制，绊线保留）；`app-chrome-*.log`、`app-firefox-29/32.log`（红证据保留）、计划 B 波记录 |
| C | 文件/目录、分块桥、真实 INCR | 已完成 | ≥1 MiB 真 INCR（1048699 B / 21 块握手与终止）、授权根实读→peer staging→真实打开；应用文件矩阵 7 行收口；`e-regress-incr.log`、`e-regress-file.log` |
| D | 新源 generation、peer/controller 恢复 | 已完成 | 受控故障两代真实交付、旧身份拒绝、无残留；`linux_dnd_recovery.py` + 8 项离线；计划 D 波记录 |
| E | 专属 master 到真实 toolkit 输入 | 已完成 | `INPUT_TOOLKIT_DONE rows=8 failures=0`（`input-toolkit-wave-6.log`）；红波 4/5 保留（重复模型断言、`row` 未导入） |
| F1 | FUSE 普通用户验收 | 已完成（用户实证） | `fuse-user.log`：`content_and_offset=ok write_errno=30 expected=30` + 唯一汇总全真 |
| F2 | 物理输入/锁屏/触点 | **部分/仍阻塞** | 按钮 10/10、移动 262 有实证；按键 0/0 如实记录；锁屏/切换、触摸板/触点、精细滚动无用户证据 → BLOCKED |
| F3 | 真实麦克风 | 已完成（用户实证） | 桥+会话内消费者同跑：双侧 `links=1`、真实 RMS、`mic_stop reason=timeout 0/0`、`consumer_stop exit=0 linked=1`、runner 汇总全真 |
| F4–F5 | 全量回归与阶段 2 总门槛 | 部分 | 离线 285 项 OK；原生三套件在 D/E 收口复跑全绿且此后未改原生共享代码；总门槛因物理项 BLOCKED 未过 |
| 产品 3–8 | Linux 产品、包装、Windows 互通 | 未开始 | 见 §9 |

## 3. 本轮新增/修改代码（全部在 `build-support/`）

- A2：`linux_deps_fetch.py`、`linux_deps_archive.py`、`linux_deps_sources.json`、`test_linux_deps_*.py`。仅 HTTPS 有界下载；deb 只读 data 不执行安装脚本；组件自有 stage、原子发布、并发锁、已有目录不覆盖。
- B：`linux_app_harness.py`、`linux_app_x11.py`、`linux_dnd_app_scenarios.py`、`linux_app_page.py`（页面夹具与 app 拖拽助手，自 app scenarios 拆出的纯移动）、`linux_dnd_app_page.html`。私有 HOME/DBus/临时 profile，本地页面；XRes PID + server epoch + 实际 owner 身份链。
- C：`linux_dnd_file.c/.h`、`linux_dnd_incr.c/.h`、`linux_dnd_stream.c/.h`、`linux_dnd_file_scenarios.py`、`linux_dnd_app_file_scenarios.py`。INCR 握手/终止、offset/遍历/符号链接边界、URI staging。
- D：`linux_dnd_recovery.py`、`test_linux_dnd_recovery.py`。故障许可注册表、进程身份（ppid+starttime）、EOF 回收屏障、旧代恒拒。
- E：`linux_input_toolkit_route.c`（专属 master + 行命令路由）、`linux_input_toolkit_scenarios.py`、`linux_input_toolkit_qt_row.py`、`linux_input_toolkit_qt.cpp`、`linux_input_toolkit_qt_build.py`、`linux_input_app_page.html`、`test_linux_input_toolkit.py`。
- F：`linux_hardware_acceptance.py`（414 行）、`test_linux_hardware_acceptance.py`（164 行，21 项）。
- 协议/共享钩子：`linux_dnd_wire.h`、`linux_dnd_protocol.py`、`linux_dnd_roles.c/.h`、`linux_dnd_selection.c`、`linux_dnd_qt_control.cpp`、`linux_dnd_qt_probe.cpp`、`linux_input_probe.c`、`linux-native-test.py`、`test_linux_dnd_scenarios.py`（500 行门禁名单）、`test_linux_native_test.py`。
- 规范二进制（本机）：`.tools/fresh-20261117/bin/dnd-roles`（六源：roles/native/selection/incr/stream/file；SHA-256 `213e26bb0b0ba2b9bade3f1b12d0e8d4a8677d735cf5452ea82254e195de63cb`）、`dnd-qt`。`.release-tools/linux-probes/dnd-roles`（11-17 旧产物）与新控制器协议不兼容，禁止复用。

## 4. 已验收证据

证据均在本机 `.release-tools/linux-probes/`，不随 git 推送。

| 波次 | 日志与关键结果 |
|---|---|
| A2 | `continuation-20261117-b/deps-fresh-install.log`（exit0，固定来源）、`deps-fresh-check.log`、`deps-fresh-recheck.log`、`fresh-qt-build.log`、`fresh-qt-matrix.log`（Qt/GTK 8 场景 PASS，唯一汇总真） |
| B 文本 | `app-chrome-1/2/3.log`、`app-diag-*.log`（Chrome 源 BLOCKED 诊断，已入档）；Firefox/nemo 行见计划 B 波记录 |
| C INCR/文件 | `e-regress-incr.log`（`incr_handshake=1`、`incr_bytes=1048699`、`incr_chunks=21`、target `incr_done=1`，两场景 PASS）；`e-regress-file.log`（文件/目录行 PASS，summary 全真） |
| C/B 矩阵 | `qt-matrix-after-d-wave.log`、计划 C 波记录（矩阵 7 行收口：nemo 三行 PASS、Firefox/Chrome 目标 PASS、两个源行 BLOCKED 登记） |
| D | 计划 D 波记录；`test_linux_dnd_recovery.py` 8 项；两代恢复原生日志（唯一汇总真） |
| E | `input-toolkit-wave-6.log`（`{"failures": 0, "result": "INPUT_TOOLKIT_DONE", "rows": 8}` + 汇总全真）；wave-4/5 红日志保留 |
| E 回归 | `e-regress-dnd-all.log`、`e-regress-incr.log`、`e-regress-file.log`：唯一 `native_test_summary`，`probe_exit=result=remaining_mounts=0`、`services_ok/cleanup_ok/mount_check_ok=true`（`sessions=2` 为 peer 所需） |
| F FUSE | `fuse-user.log`：`private_fuse: content_and_offset=ok write_errno=30 expected=30`；汇总 `result=0` 全真 |
| F 输入 | `hardware-input-user.log`：唯一 `input_stop reason=window_closed`，`button_press/release 10/10`、`motion 262`、`key_press/release 0/0`、`focus_out 0` |
| F 麦克风 | `hardware-mic-user.log`：最终运行 `mic_start`（`oink-native-6h3png1r`）→`mic_route links=1`（bridge 21→sink 16）→`mic_rms` 起于 −15.6 dB→`mic_stop reason=timeout capture_exit=0 playback_exit=0`；`f-mic-consumer.log`：`consumer_route links=1`（21→19）、持续 `consumer_rms`（nonzero≈2045/2048）、`consumer_stop exit=0 linked=1`、runner 汇总 `result=0 services_ok=true` |
| 离线全量 | 2026-11-20 `discover -p 'test_linux*.py'`：**285 项 OK / 41.2 s**（含硬件入口 21 项）；500 行门禁通过 |

失败与红证据全部保留：`input-toolkit-wave-4/5.log`、`app-firefox-29/32.log`、`hardware-mic-user.log`/`f-mic-consumer.log` 中的历史失败行（流超时、`device_error`、`JSONDecodeError`、`pw-cli` 超时、`AttributeError`）未被删除或覆盖。

## 5. 未验证与阻塞

- 物理按键交付：本轮采集窗内 `key_press/release=0/0`（用户实际按键情况未知，只记录事实），不能推断通过。
- 锁屏/会话切换、触摸板/触点、精细滚动：无用户实证 → BLOCKED。E 的 `XIScrollClass/XITouchClass` 只读采集只说明能力类存在，不代替物理支持。
- Chromium 无视 XTEST 拖拽起始（文本与文件源方向）：平台限制 BLOCKED，`browser_source_synthetic_blocked` 绊线保留；物理鼠标 counterpart 属用户手测。
- Firefox 文件源方向未实现（页面夹具仅 text/plain）→ BLOCKED 登记。
- contacts `0x020b` 产品网络契约未声称端到端；测试桥不是产品网络，虚拟麦克风未接产品。
- 阶段 2 门槛据此仍未通过：任一必需项 BLOCKED/缺证据不得进入阶段 3。

## 6. 环境与复跑手册

命令在仓库根、同一 Bash 会话执行；原生一律串行、走 runner 私有会话，不手动指向宿主 DISPLAY。

```bash
cd /home/wgh/dev/cross_platform_app/oink
export PATH="$PWD/.release-tools/node-v24.14.1-linux-x64/bin:$PATH"
bash build-support/linux-deps.sh --check --native-tests --toolkit-tests
```

指定 A2 新树时可加 `OINK_QT_KIT="$PWD/.tools/fresh-20261117/Qt/6.12.0/gcc_64"`、`OINK_XRES_SYSROOT="$PWD/.tools/fresh-20261117/sysroot"`。

限额重建（`bounded` 仅为当前 shell 函数；新批次目录名不得覆盖历史证据）：

```bash
bounded() { systemd-run --user --scope --quiet -p MemoryMax=4G -p CPUQuota=200% -p TasksMax=1024 timeout 180 "$@"; }
out="$PWD/.release-tools/linux-probes/handover-20261120"; mkdir -p "$out"
bounded cc -std=c11 -O2 -Wall -Wextra -Werror -o "$out/dnd-roles" \
  build-support/linux_dnd_roles.c build-support/linux_dnd_native.c build-support/linux_dnd_selection.c \
  build-support/linux_dnd_incr.c build-support/linux_dnd_stream.c build-support/linux_dnd_file.c \
  $(pkg-config --cflags --libs gtk+-3.0 x11 xi xtst)
bounded cc -std=c11 -O2 -Wall -Wextra -Werror -o "$out/input-toolkit-route" \
  build-support/linux_input_toolkit_route.c $(pkg-config --cflags --libs x11 xi xtst) -lXau
bounded python3 -B build-support/linux_input_toolkit_qt_build.py --output "$out/dnd-qt-fixture" \
  --kit "$PWD/.tools/fresh-20261117/Qt/6.12.0/gcc_64"
```

原生套件（按需重建基线 `input`/`dnd`/`input-matrix`/`fuse` 见第三轮 §7.2；产物 mtime 必须晚于全部输入）：

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

每份原生日志必须且只能有一条 `native_test_summary`：`probe_exit=result=remaining_mounts=0`、`services_ok/cleanup_ok/mount_check_ok=true`；再核业务断言。

F 用户入口（`linux_hardware_acceptance.py`）：

```bash
python3 build-support/linux_hardware_acceptance.py status          # 空 env 可跑，只读日志摘要
OINK_HARDWARE_USER=1 python3 build-support/linux_hardware_acceptance.py input-collector
```

麦克风需要桥与私有会话内消费者同跑。消费者在代理侧以 runner 启（会话约 8 分钟，桥须在窗口内启动一次；桥节点重建后旧消费者不会再链接新桥，需新会话）：

```bash
python3 build-support/linux-native-test.py --timeout 510 -- \
  python3 build-support/linux_hardware_acceptance.py mic-consumer --seconds 480 \
  >> .release-tools/linux-probes/f-mic-consumer.log 2>&1
```

用户终端（`root` 换成本次 runner 输出的 `/tmp/oink-native-<id>`）：

```bash
PULSE_SERVER=unix:/tmp/oink-native-<id>/runtime/pulse-native OINK_HARDWARE_USER=1 \
  python3 /home/wgh/dev/cross_platform_app/oink/build-support/linux_hardware_acceptance.py \
  mic-bridge --enable-mic --device <pactl 源名> --seconds 20
```

FUSE 仍只在用户普通终端执行（代理不挂载；`NoNewPrivs=1` 下 setuid fusermount3 是既有阻塞）：

```bash
python3 /home/wgh/dev/cross_platform_app/oink/build-support/linux-native-test.py --timeout 12 -- \
  /home/wgh/dev/cross_platform_app/oink/.release-tools/linux-probes/fuse \
  > /home/wgh/dev/cross_platform_app/oink/.release-tools/linux-probes/fuse-user.log 2>&1
```

## 7. 已知坑与平台限制

- 私有音频栈**无会话管理器**：pulse 协议流（in-session 与 foreign 一致）永不自动链接，`--target` 无效、节点不自动建端口。唯一通路是 core pipewire socket（`oink-test` unrestricted）+ `pw-cli set-param PortConfig`（dsp/F32P/48000/MONO）+ `pw-link` 按声道显式连。
- `pw-cli`/`pw-link` 若继承交互 stdin 会挂起；必须 `stdin=DEVNULL` 且带超时。
- 收尾时读子进程 `stderr` 前先判空（`stderr=DEVNULL` 时是 `None`，读它会 `AttributeError`）；操作者主动 SIGTERM 的正常停止不得计为设备错误（`pw-cat` 下 SIGTERM 可能返回 1）。
- 未设 `WM_DELETE_WINDOW` 的裸窗被 WM `XKillClient` 会掐掉整个 X 连接且零日志；须 `XInternAtom`+`XSetWMProtocols` 并处理 ClientMessage(33)。
- 截止时间一律 `time.monotonic()`；墙钟/单调钟混用会让超时恒真。
- Qt xcb 未启用 detectable autorepeat：按住键的形态是成对 press/release（7/7）；GTK/zenity 为 7 presses/1 release。断言各按诚实模型，不互相套用。
- `.release-tools/linux-probes/dnd-roles`（11-17 旧产物）与新协议不兼容（`invalid report shape`）；必须用当批六源重建的 `.tools/fresh-20261117/bin/dnd-roles` 或新批次。
- `pkill -f <模式>` 会匹配并杀掉包含该模式的自身 shell；重启会话用后台任务管理，不叠加 pkill。
- Chromium 无视 XTEST 拖拽起始（无 dragstart、无 XdndSelection owner）；`browser_source_synthetic_blocked` 绊线保留，未来若放行会强制升级为 FAIL。
- nemo 源拖拽：图标真实坐标需截图实证；应用会 raise 自己的窗口压住 catcher，需 `move_window` 到空区；nemo 对 UTF8_STRING 返回裸绝对路径（真实原生格式），断言按实际契约。
- Firefox 目标只接受 `text/plain`（角色 targets 已补），浏览器文件目标走 File 对象通道。
- 私有 runner 的后台会话会被会话生命周期影响；起会话用后台任务并在日志里确认 `consumer_start` 后再交用户。
- Xephyr 宿主键盘/鼠标转发已被 runner 禁用；不得重新开启“方便测试”。

## 8. 红线与禁止事项

- 合成输入仅私有 Xephyr；严禁向宿主 `DISPLAY=:0` 注入、grab 真实键盘、接用户浏览器 profile/登录态。
- 私有 HOME/XDG/Xauthority/DBus/Pulse/PipeWire、认证、进程归属、超时与失败保留不可绕过；不能绕过 `--inside-scope`、`require_alive`、清理与挂载检查。
- 构建/原生限额 4 GiB、CPU200%（runner TasksMax 经批准为 1024）；原生串行，不以不限额重试处理卡顿。
- 系统安装/FUSE/物理设备/锁屏/麦克风交用户；不 sudo、不重试代理 FUSE、不启用 helper/uinput、不改用户组/udev/防火墙/X server 权限。
- 阶段 2 只改必要 `build-support/` 与实施记录；不动产品、工具链锁、CI。
- 不记录真实按键、剪贴板/音频正文、凭证；诊断只用测试随机数据与聚合数值。
- 不把一次重跑成功抹掉已出现失败；保留红证据、修根因、补回归再完整运行。
- 后续 commit/push/tag/release、Windows 安装或发布须新的明确授权。

## 9. 阶段门槛与下一步

阶段 2 必需条件：Qt/真实应用源目标矩阵、文件目录/INCR、代际恢复、真实 toolkit 输入、音频回归、FUSE 和用户硬件均有证据；任一必需项 FAIL/BLOCKED/缺证据都不能进入阶段 3。

当前仍缺的用户证据：

1. 物理按键：在 `input-collector` 窗口内实际按键后切换焦点，日志应出现非零 `key_press`。
2. 锁屏/会话切换：锁屏再解锁后核对 `focus_lost` 或新窗口采集。
3. 触摸板/触点与精细滚动：真实设备操作证据（无设备不等于平台不支持，保持 BLOCKED）。
4. 可选：物理鼠标执行 Chrome 源方向拖拽，解除 `browser_source_synthetic_blocked`。

以上补齐后复跑 §6 回归与阶段 2 总门槛。阶段 2 通过后，再按长期计划阶段 3–8 开展 Linux 产品实现；不得把探针、runner 或用户入口通过当成产品支持。

## 10. 接手第一步（新代理/新会话）

1. 只读确认现状：`git status --short --branch`、`git log -3`、`git diff --check`。A2–F 实现已随 `8dbc9ab`/`65279d1` 推送 `origin/main`，工作树应干净；不得丢弃、覆盖或重写这些历史提交，也不要在其上重放旧工作树快照。
2. 读本文件与 `doc/linux-x11/plan.md` 末尾的 F 波收口记录；旧交接只作历史快照，不要照旧安装全局 Qt5 或复用 11-17 旧二进制。
3. 依赖检查（不自动安装系统包）：`bash build-support/linux-deps.sh --check --native-tests --toolkit-tests`，需要时加 `OINK_QT_KIT="$PWD/.tools/fresh-20261117/Qt/6.12.0/gcc_64"`、`OINK_XRES_SYSROOT="$PWD/.tools/fresh-20261117/sysroot"`；缺失即停并告知用户。
4. 只读核验既有用户证据：`.release-tools/linux-probes/` 下的 `fuse-user.log`、`hardware-input-user.log`、`hardware-mic-user.log`、`f-mic-consumer.log`；不得覆盖、删除或要求用户重做已有证据。
5. 先跑离线全量（`discover -p 'test_linux*.py'`，当前 285 项）确认环境；原生套件按 §6 串行复跑，产物必须当批从当前源码重建且 mtime 晚于全部输入。
6. 向用户收集 §9 的物理证据（按键、锁屏/切换、触摸板/触点、精细滚动，可选 Chrome 源物理鼠标拖拽）；集齐前不启动产品阶段 3–8，不把 BLOCKED 重新分类为 PASS。
7. commit/push/tag/release、系统包安装、FUSE 挂载及任何新权限动作须用户新的明确授权；失败与红证据一律保留。

## 11. 交接记录

| 日期 | 代理 | 范围 | 结果/产物 |
|---|---|---|---|
| 2026-11-17 | Qoder | 三轮交接与 Qt 夹具验收 | 见 2026-11-17 各文件；代码 `2d2061a` 已推送 |
| 2026-11-20 | Qoder | A2/B/C/D/E 分波 + F 入口与用户实证 | 离线 285 项 OK；A2 新树验收、B 文本矩阵、C INCR/文件矩阵、D 两代恢复、E 8 行输入矩阵、FUSE/麦克风用户实证、物理输入部分证据；产品未动 |
| 2026-11-20 | Qoder | 提交与推送本轮交付 | 代码 `8dbc9ab`、文档 `65279d1` 已推送 `origin/main` 并核验远端 SHA；本文状态更新随后独立提交推送 |
| 待接手 | 后续代理 | 物理硬件用户证据补齐与阶段 2 总门槛 | 按 §6 复跑、§9 收口；完成后进入产品阶段 3–8 |
