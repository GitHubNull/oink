# Linux X11 阶段 2 机制门槛：任务转交（2026-11-17，第二轮）

> 读者：下一个接手本仓库的 AI 编程代理。
> 权威计划与实施记录：[../linux-x11/plan.md](../linux-x11/plan.md)；阶段 3–8 的完整范围仍以它为准，本文件不重设目标。
> 第一轮快照：[2026-11-17-linux-x11-port.md](2026-11-17-linux-x11-port.md)，只读，不覆盖。
> 本文件自包含第二轮（基线 `6703e88`）以来的状态、证据、缺口与下一步小波次计划；接手不需要本目录之外的历史对话。

## 0. 一句话状态

阶段 2（关键机制）**仍未通过**。第二轮已实测：GTK 跨进程/双端 XDND 8 场景、26 项合成输入矩阵、私有音频保捕获静音/需求门控/两代服务重启、187 项 Linux Python 单测、6 个零告警 C 重建。仍缺：Qt/Nemo/Chrome/Firefox 源目标矩阵、文件/目录与大 selection/INCR、新源 generation 与 peer/controller 恢复、真实 toolkit 输入交付、物理 HID/触摸板/触点/真实麦克风、FUSE 普通终端证据。产品代码（阶段 3–8）尚未开始。本轮实施期间无提交、推送、tag 或发布；收口后按用户明确要求提交并推送，未发版。

## 1. 基线与工作树（接手先复核）

| 项目 | 值 |
|---|---|
| 仓库 | `/home/wgh/dev/cross_platform_app/oink` |
| 分支 / HEAD | `main` / `6703e88` |
| 产品代码基线 | 与 `d8b2932` 相同；`3ba1e57`、`6703e88` 只增加探针与文档 |
| 工作树（交接快照） | 7 个 tracked 修改（5 个 `build-support` + `plan.md` + 本目录 README 索引）+ 21 个 `build-support/` 未跟踪 + 本文件；无产品/锁/CI 改动；交接时未提交，随后按用户明确要求提交并推送 |
| OS / 会话 | Linux Mint 22.3 (Zena)，Cinnamon，X11，x86_64；原生测试只用私有 Xephyr，不接宿主 `:0` |
| 工具链 | Rust 1.99.0（`rust-toolchain.toml` 锁定）；Node 24.14.1 / npm 11.11.0（仓库内 `.release-tools/node-v24.14.1-linux-x64/`） |
| 计划规模 | `doc/linux-x11/plan.md` 226 行；第一轮交接 483 行 |

修改的 tracked 文件：

```text
build-support/linux-deps.sh
build-support/linux-native-test.py
build-support/linux_input_probe.c
build-support/test_linux_deps.py
build-support/test_linux_native_test.py
doc/linux-x11/plan.md
doc/handover/README.md  # 仅新增本文件索引行
```

交接快照中未跟踪的 21 个文件全部在 `build-support/`（另有本交接文档）：4 个音频探针/策略、2 个音频测试、9 个 DnD 角色/协议/场景/测试、6 个输入矩阵 C/头/测试。最大为 `test_linux_native_test.py` 495 行、`test_linux_audio_restart_probe.py` 489 行、`test_linux_audio_native_consumer.py` 458 行，其余均更小，全部 ≤500 行。第一轮交接里“8 个未跟踪文件”是当时快照；**不要删除任何未跟踪文件**。

```bash
cd /home/wgh/dev/cross_platform_app/oink
git status --short
git log --oneline -3
```

## 2. 任务总表

长期任务编号沿用第一轮交接：

| ID | 范围 | 状态 | 证据 / 下一步 |
|---|---|---|---|
| #1 | 核查 Linux 桌面支持现状 | 完成 | 第一轮交接 §5；产品平台二分结构未变 |
| #2 | 制定 X11 完整适配与验收计划 | 完成 | `doc/linux-x11/plan.md` |
| #3 | 准备 Linux 工具链与依赖检查 | 完成 | Rust/Node 锁定版本可用；基础依赖 20 项通过；`--toolkit-tests` 仅缺 `qtbase5-dev` |
| #4 | 验证隔离 X11 输入、拖拽和音频机制 | **进行中** | 输入矩阵、GTK 8 场景 DnD、音频 route/restart 已过；Qt/Nemo/浏览器、文件/目录、INCR、新源代际/重连、真实 HID/触点/麦克风、FUSE 未过 |
| #5 | 实现 Linux 桌面生命周期和原生后端 | 未开始（被 #4 门槛阻塞） | 通过后按计划 §3–6 实现，不用假后端消错 |
| #6 | 完成平台协议、设置与本地安装包 | 未开始 | 依赖 #5 |
| #7 | 完成 Linux Windows 双机与安装验收 | 未开始 | 依赖 #6；需用户 Windows 测试机 |

已批准续作计划的六步进度：

| 步骤 | 状态 | 说明 |
|---|---|---|
| 1. 固定测试入口/退出证据/依赖检查 | 完成 | 双端运行器、统一 `native_test_summary`、toolkit 检查已落地 |
| 2. 跨进程/双端 XDND 与真实应用矩阵 | 部分 | 同服务器 6 场景 + 双服务器 2 场景通过；真实应用矩阵未完成 |
| 3. 输入交付与触点边界 | 部分 | 26 项合成 core+XI2 通过；真实 toolkit 交付与物理触点未完成 |
| 4. 保捕获静音与消费者门控 | 完成（机制范围） | 私有合成音频；真实麦克风/产品接入不属于本轮机制证据 |
| 5. FUSE 与真实硬件验收 | 阻塞等用户 | 无可读取 `fuse-user.log`；代理 `NoNewPrivs=1`，不重试、不绕过 |
| 6. 收口并进入阶段 3–8 | 未开始 | 实施记录已追加；阶段 2 未通过，不进入产品阶段 |

## 3. 已验证证据（第二轮）

### 3.1 单测与构建门禁

```bash
systemd-run --user --scope --quiet -p MemoryMax=4G -p CPUQuota=200% -p TasksMax=256 \
  timeout 120 python3 -B -m unittest discover -s build-support -p 'test_linux*.py' -v
```

- 结果：`Ran 187 tests ... OK`，exit 0。日志 `.release-tools/linux-probes/linux-unit-current.log`。
- 组成（本轮实跑）：运行器 43、依赖脚本 18、音频 93、DnD 场景/控制 33。
- 6 个 C 产物（`input`、`dnd`、`dnd-roles`、`input-matrix`、`input-matrix-test`、`fuse`）从当前源码以 `-std=c11 -O2 -Wall -Wextra -Werror` 重建，exit 0、零告警；`input-matrix-test` 纯校验器通过。日志 `.release-tools/linux-probes/native-build-current.log`。
- `bash -n build-support/linux-deps.sh` 通过；基础依赖检查 20 项 OK，日志 `deps-current.log`。toolkit 检查仅缺 `qtbase5-dev`，日志 `deps-toolkit-current.log`。

### 3.2 跨进程与双端 XDND

命令：

```bash
python3 -B build-support/linux-native-test.py --peer-x11 --timeout 150 -- \
  python3 -B build-support/linux_dnd_scenarios.py \
  --roles /home/wgh/dev/cross_platform_app/oink/.release-tools/linux-probes/dnd-roles --suite all
```

8 个场景全部 PASS（日志 `dnd-all-current.log`，`scope=all`，`sessions=2`）：

```text
same-server-return-reenter-drop
same-server-return-reenter-escape
same-server-return-reenter-slow-drop
same-server-slow-selection-late-generation
same-server-target-exit-held
same-server-source-exit-held
dual-server-test-bridge-copy-drop
dual-server-test-bridge-escape
```

关键事实：

- 源只发生一次 Begin/Down；返回源保持同一 context 与原 grab；无幽灵 pointer/keyboard grab。
- 48 字节正文真实经过源 selection → catcher 读取 → 测试桥 → proxy selection → target 读取，并校验 SHA-256；CLIPBOARD 哨兵所有权不变；源删除回调为 0。
- 双端桥只是**机制模拟**，不是产品网络传输；目标正文不是预置或经控制 IPC 直接交付。
- 5 次慢 selection 控制 ACK 实测 448538–672639 ns，严格小于 500 ms 探针控制预算；日志标记 `scope=test_role_control_not_product_fence`。产品只有 3 秒 preparation 预算（`crates/oink-routing/src/router/dnd/target.rs:67`）和 5 秒 ordinary-input 过期（`crates/oink-agent/src/threads/inject.rs:74-75`），不存在独立 fence ACK 常量。
- 旧 GTK 同进程 Drop/Escape 回归仍通过（`dnd-current.log`）。

明确未实现（场景脚本常量 `NOT_IMPLEMENTED`，也是下一步范围）：

```text
product network transport / actual monitor-edge detection
Qt, file managers, browsers, Wayland, INCR and large selections
recovery/reconnect after peer-server death or controller death
new source drag generation after restart (destination late-generation fencing is tested)
```

### 3.3 输入矩阵与设备能力

- `input-matrix` 在私有 Xephyr 中 26 项全部通过，每项 core+XI2 双模式；`failed=0 incomplete=0 cleanup_ok=true`，运行器 result=0。日志 `input-matrix-current.log`。
- 覆盖左右修饰键、独立 AltGr、媒体键、人工重复与服务端定时重复、相对位移、滚轮 4–7；专属 master 的 XTEST 来源与普通输入可区分。
- 宿主仅做只读 `DISPLAY=:0 xinput list --long`，未注入、未抓取事件、未改设备。日志 `host-device-capabilities.log`：当前设备暴露水平/垂直 `XIScrollClass`（increment 120），未枚举到 `XITouchClass`/`XIGestureClass`。这**不等于** Linux/Xorg 没有触点能力，也不是物理滚动/手势/触点验收。
- 旧 `input` 回归通过（`input-current.log`）。

### 3.4 音频：保捕获静音、需求门控、服务重启

- 路由/门控：日志 `audio-route-current.log`（238 行），`summary failures=0 cleanup_failures=0 exit=0`。静音播放支路后扬声器 RMS=0、发送捕获仍 RMS=5792.56；1 kHz 发送与 2 kHz 接收频谱隔离，停止后无回环尾音。两个真实消费者、cork、部分退出、最后正常/异常退出、未知需求、许可撤销/到期均按实际流启停，最终 `starts=8 stops=8`，无模块/流/线程残留，默认设备不变。
- PipeWire 消费者身份：首次原生失败为 `native_consumer_process_identity_mismatch`；最小实验确认 **Node 上没有 `application.process.id`，PID 在其 `client.id` 指向的 `PW:Client` 上**。修复为同一 registry 快照内 Node→Client→自有 PID 校验；缺失/歧义/外来 Client 均拒绝，并先补红测试。日志 `audio-native-identity.log` 仅含数值身份。
- pw-cat 在本环境存在 Pulse 镜像，明确记录 `native_only_evidence=not_exercised_pulse_mirrored`；未镜像 native-only 只有离线测试覆盖，**不冒充真实 native-only 实证**。
- 服务重启：日志 `audio-restart-current.log`。两代使用同一私有 socket/runtime，cookie 确实变化；第一代断连后真实查询失败触发 unknown/停采/reap，旧身份与许可被拒绝；第二代只在新消费者证据下重新启采。两代 RMS 5792.56；每代服务/流进程完全回收；外层默认设备与 epoch 不变；未放宽 `Session.require_alive`。
- 旧 `audio` 回归通过（`audio-current.log`）。

### 3.5 日志机器校验

本轮 7 份原生回归日志均位于 `.release-tools/linux-probes/`：`input-current`、`dnd-current`、`input-matrix-current`、`audio-current`、`audio-route-current`、`audio-restart-current`、`dnd-all-current`。逐份校验：唯一 `native_test_summary`，`probe_exit=0`、`result=0`、`remaining_mounts=0`、`services_ok=true`、`cleanup_ok=true`、`mount_check_ok=true`；`dnd-all-current` 为 `sessions=2`，其余 `sessions=1`。日志不含真实按键文本、剪贴板/音频正文。

## 4. 未验证与阻塞项

| 项目 | 状态 | 说明 |
|---|---|---|
| Qt 夹具 | 阻塞在依赖 | 缺 `qtbase5-dev`；用户执行安装命令后代理只跑检查，不安装、不用 `--no-sandbox` |
| Nemo/Chrome/Firefox | 未验证 | 命令存在；必须用私有 profile/仅本地测试内容，不接用户会话 |
| 文件/目录载荷 | 未实现 | 当前 XDND 只传 UTF8_STRING 测试正文 |
| 大 selection/INCR | 未实现 | 需要真实分块读取与哈希校验 |
| 新源 generation | 未实现 | 只有目标侧迟到 generation 拒绝已验证 |
| peer/controller 恢复 | 未实现 | 源/目标进程退出已测；服务/控制器死亡后的重连未测 |
| 真实 toolkit 输入交付 | 未验证 | 26 项只证明合成 X11 事件与 core/XI2 身份 |
| 物理 HID/触摸板/触点/锁屏 | 待用户 | 需要真实硬件与桌面操作；不得自动注入宿主 |
| 真实麦克风/真实应用选虚拟输入 | 待用户 | 私有合成音频不能冒充 |
| FUSE 普通终端 | 待用户 | 约定日志 `fuse-user.log` 当前无可读取证据；代理不重试挂载 |
| 产品网络 DnD | 未实现 | 双端桥明确不是产品传输 |
| 产品音频后端接入 | 未开始 | 阶段 6 |
| 阶段 3–8、DEB/可解压包 | 未开始 | 依赖阶段 2 通过 |
| Windows 双机 | 未开始 | 依赖安装包；需用户测试机 |
| Wayland | 明确不做 | 启动时必须准确提示限制，XWayland 不算完整 X11 |

## 5. 产品代码现状（未变，按 `path:line` 复核）

| 领域 | 位置 | 现状 |
|---|---|---|
| Tauri 依赖 | `apps/oink/src-tauri/Cargo.toml:37-43` | `tauri`/`tauri-build` 仅 macOS/Windows |
| Tauri 构建 | `apps/oink/src-tauri/build.rs:5-6` | 非 mac/win 直接返回 |
| Tauri 入口 | `apps/oink/src-tauri/src/main.rs:37-45` | 非 mac/win 打印“只支持 macOS 与 Windows” |
| 窗口调度 | `apps/oink/src-tauri/src/shell.rs:69-76` | 缺 Linux GTK/WebKitGTK 主线程路径 |
| 后台启动 | `apps/oink/src-tauri/src/service.rs:48-102` | `dispatch_args`/`start_once` 主要 Windows |
| 运行/托盘 | `apps/oink/src-tauri/src/app.rs:249`、`:596` | `run()` 平台 cfg；托盘仅 Windows |
| locale | `apps/oink/src-tauri/src/system_locale.rs:2-13` | 仅 mac/win |
| Agent 工厂 | `crates/oink-agent/src/backend/mod.rs:13-25` | 仅 `macos`/`main_windows` |
| Agent 启动 | `crates/oink-agent/src/backend/launch.rs:204` | 非 mac/win unsupported |
| Agent host | `crates/oink-agent/src/host.rs:110-160` | `start_platform_with` 可复用 |
| 输入工厂 | `crates/oink-input/src/lib.rs:440-446` | Linux 返回 `Unsupported` |
| 显示工厂 | `crates/oink-display/src/lib.rs:105-112` | Linux 返回 `Unsupported` |
| 定位 5 错 | `crates/oink-locator/src/notification/mod.rs:353`、`:514`；`projection/mod.rs:126`；`projection/annotation.rs:125`；`lib.rs:395`、`:605` | Linux `platform`/`scale_for`/`annotation_unit` 缺失、`new()` 签名不匹配 |
| 定位文字 | `crates/oink-locator/src/text.rs:100-106` | 系统文字仅 mac/win |
| 剪贴板工厂 | `crates/oink-clipboard/src/lib.rs:360-368` | Linux 返回 `Unsupported` |
| 平台枚举/字符串 | `crates/oink-foundation/src/clipboard.rs:35-39`；`crates/oink-clipboard/src/wire.rs:97-106` | 只有 Mac/Windows/Unknown |
| 音频播放 | `crates/oink-audio/src/playback.rs:18-21` | 非 mac/win 使用 `unsupported` |
| 音频捕获 | `crates/oink-audio/src/lib.rs:255-277` | `capture_capability()` 返回 `"unsupported"`，Linux `Capture` 空壳 |
| 麦克风许可 | `crates/oink-audio/src/microphone/mod.rs:118-130` | `StartPermit` 双门语义（本地启用 + 新鲜消费者）；阶段 6 按此接入 |
| 自启动 | `crates/oink-autostart/src/lib.rs:182-196` | Linux `UnsupportedPlatform` |
| 崩溃 | `crates/oink-crash/src/lib.rs:8,12` | 仅 mac/win 模块 |
| 构建目标 | `build-support/build.py:19` | 仅 Apple/Windows target |
| 打包 | `build-support/package.py:22`、`:51-53` | `--platform` 仅 `macos|windows` |
| 前端平台 | `apps/oink/src/settings/model.ts:86`、`App.tsx:81`、`KeyMap.tsx:27`、`arrange/ProfileHotkeyPreview.tsx:18`、`settings/SettingsPreview.tsx:12` | 非 Windows 当 Mac |

阶段 2 已取证的产品契约（实现时必须沿用）：

- DnD：`crates/oink-clipboard/src/dnd.rs:292-467` 的 `DndBackend` 生命周期；3 秒 preparation 预算见 `crates/oink-routing/src/router/dnd/target.rs:67`；5 秒 ordinary-input 过期见 `crates/oink-agent/src/threads/inject.rs:74-75`。
- 触点：`InputEvent::TouchpadSample` 是本地样本，但 `contacts::Frame` 经 `crates/oink-protocol/src/v1/registry_entries.rs:26` 的 `0x020b` / `oink.input.contacts/1` 可靠传输，`crates/oink-routing/src/router/contacts.rs:174-179` 会发送；不能误称触点不走网络。
- 音频：`StartPermit` 双门；消费者必须按 registry 身份 + 当前状态/active link 判定，不能只看 source RUNNING 或名称。

## 6. 下一步小波次计划（阶段 2 剩余门槛，约 1500 字）

建议顺序 A→B/C→D→E→F：A 先等用户装 Qt 包；B 与 C 可在 A 之后并行准备夹具，但原生测试仍串行；D 依赖现有 role/generation 协议；E 依赖 B 的应用夹具；F 依赖用户日志/硬件。每波结束把命令、退出码、日志路径和结论追加到 `doc/linux-x11/plan.md`，再开下一波，避免多套隔离桌面同时占资源。

**波次 A：Qt 依赖与原生夹具（先等用户装包）。** 用户在自己终端执行 `bash build-support/linux-deps.sh --install --native-tests --toolkit-tests`（含 `qtbase5-dev`、`nemo`；代理不 sudo）。装好后代理只跑 `--check --toolkit-tests`，再新增 `build-support/linux_dnd_qt_probe.cpp` 与对应 Python 汇总测试：Qt Widgets 源/目标、`QDrag` 原生 XDND，覆盖 COPY/drop、Escape/cancel、离开后返回源、拖拽中的 grab 保持。验收要求源 context 身份、Begin/End 各一次、落点正文哈希、无幽灵 pointer/keyboard grab；任何沙箱/依赖失败记为 BLOCKED，不加 `--no-sandbox`、不复用用户配置。

**波次 B：真实应用矩阵。** 在私有 Xephyr 中分别验证 Nemo（文件管理器源/目标）、Chrome、Firefox；浏览器使用新建私有 profile，只访问本地测试内容，不上传文件。每项记录源/目标角色、Drop/返回/Cancel 实测，输出矩阵行（PASS/BLOCKED/FAIL）。仅 GTK 通过不能代表矩阵完成。

**波次 C：文件/目录与大 selection/INCR。** 扩展角色协议，在 XDND selection 中提供 `text/uri-list` 与临时文件/目录；目标实读后只记录大小与 SHA-256，源删除回调必须为 0。增加大于常规缓冲的正文（如 1 MiB）触发 INCR 分块读取，断言分块顺序、总字节和哈希；CLIPBOARD 哨兵保持不变。不得预置目标正文或经控制 IPC 绕过 selection。

**波次 D：代际与重连。** 新增源进程重启后的新 generation 场景、peer server 死亡与 controller 死亡后的恢复场景；旧清理/迟到回调不得结束新会话，旧 epoch 身份必须被拒绝，新会话资源独立且退出后无残留 grab/进程/挂载。沿用现有 `seq/generation` 控制协议与运行器双端环境，不新增第二套注入通道。

**波次 E：真实 toolkit 输入交付与设备边界。** 用专属 master + 自建 XTEST 向真实 GTK、Qt、Nemo、Chrome/Firefox 窗口交付按键、修饰键、滚轮与相对位移，断言应用实际收到且普通输入不被误吞；继续只读采集 `XITouchClass`/`XIGestureClass`/`XIScrollClass` 能力，区分“Xephyr 不暴露”和“宿主不支持”。物理 HID、触摸板/精细滚动、触点、锁屏/切换交用户短步骤；helper/uinput 未获授权前不实施，也不把快捷键映射冒充触点支持。

**波次 F：用户门槛与收口。** FUSE 先只读复查 `.release-tools/linux-probes/fuse-user.log`；有有效日志直接核验全文/offset、EROFS、卸载成功、无残留、exit 0，缺证据再给普通用户终端命令。真实麦克风与真实应用选择虚拟输入按用户硬件步骤执行。每波完成后把命令、退出码、日志路径和结论追加到 `doc/linux-x11/plan.md`，保留旧记录；阶段 2 全部必需项有证据后才判定通过，未通过就列出缺项并停住，不进入阶段 3。

**保留的 #5–#7 完整范围：**

- #5：Linux Tauri/GTK 生命周期、后台工厂、ownership/单实例/IPC/托盘/关闭重开；真实输入/显示/定位/通知/投影；剪贴板/Namespace/FUSE/DnD；libpulse 音频与虚拟麦克风。不是只修 `oink-locator` 的 5 个编译错误。
- #6：协议/平台枚举、Linux 设置与语言、XDG 自启动、崩溃/诊断、本地更新、DEB 与可解压包；不依赖开发服务器/源码目录运行。
- #7：安装/升级/卸载、Windows 双向互通、30 分钟混合负载；无卡键、幽灵 grab、回环、声音不恢复。

每阶段先核验当时代码再实施；不以 mock、空 stub、扩大 cfg、隐藏功能或跳过断言“先跑起来”。

## 7. 环境与命令手册

### 7.1 工具链

```bash
cd /home/wgh/dev/cross_platform_app/oink
export PATH="$PWD/.release-tools/node-v24.14.1-linux-x64/bin:$PATH"
node --version   # v24.14.1
npm --version    # 11.11.0
rustc --version  # 1.99.0
cargo --version  # 1.99.0
```

`CARGO_HOME=/home/wgh/dev_env/rust/cargo`、`RUSTUP_HOME=/home/wgh/dev_env/rust/rustup`；已装好的 1.99.0 不要重装、降级或改全局默认工具链。下载需要 `https_proxy=http://127.0.0.1:7890` 与 `no_proxy=localhost,127.0.0.1`。

### 7.2 依赖检查

```bash
bash build-support/linux-deps.sh --check --native-tests
bash build-support/linux-deps.sh --check --native-tests --toolkit-tests
```

基础检查应通过；toolkit 当前提示缺 `qtbase5-dev`，由用户在终端执行：

```bash
bash build-support/linux-deps.sh --install --native-tests --toolkit-tests
```

### 7.3 编译探针（当前源码、零告警）

```bash
mkdir -p .release-tools/linux-probes
cc -std=c11 -O2 -Wall -Wextra -Werror -o .release-tools/linux-probes/input \
  build-support/linux_input_probe.c $(pkg-config --cflags --libs x11 xi xtst)
cc -std=c11 -O2 -Wall -Wextra -Werror -o .release-tools/linux-probes/dnd \
  build-support/linux_dnd_probe.c $(pkg-config --cflags --libs gtk+-3.0 x11 xi xtst)
cc -std=c11 -O2 -Wall -Wextra -Werror -o .release-tools/linux-probes/fuse \
  build-support/linux_fuse_probe.c $(pkg-config --cflags --libs fuse3)
cc -std=c11 -O2 -Wall -Wextra -Werror -o .release-tools/linux-probes/dnd-roles \
  build-support/linux_dnd_roles.c build-support/linux_dnd_native.c \
  build-support/linux_dnd_selection.c $(pkg-config --cflags --libs gtk+-3.0 x11 xi xtst)
cc -std=c11 -O2 -Wall -Wextra -Werror -o .release-tools/linux-probes/input-matrix \
  build-support/linux_input_matrix.c build-support/linux_input_matrix_events.c \
  build-support/linux_input_matrix_keys.c build-support/linux_input_matrix_pointer.c \
  $(pkg-config --cflags --libs x11 xi xtst)
cc -std=c11 -O2 -Wall -Wextra -Werror -o .release-tools/linux-probes/input-matrix-test \
  build-support/linux_input_matrix_test.c
```

### 7.4 隔离原生入口（串行运行，不并发）

```bash
python3 -B build-support/linux-native-test.py --timeout 60 -- .release-tools/linux-probes/input
python3 -B build-support/linux-native-test.py --timeout 60 -- .release-tools/linux-probes/dnd
python3 -B build-support/linux-native-test.py --timeout 60 -- .release-tools/linux-probes/input-matrix
python3 -B build-support/linux-native-test.py --timeout 120 -- python3 -B build-support/linux_audio_probe.py
python3 -B build-support/linux-native-test.py --timeout 180 -- python3 -B build-support/linux_audio_route_probe.py
python3 -B build-support/linux-native-test.py --timeout 120 -- python3 -B build-support/linux_audio_restart_probe.py
python3 -B build-support/linux-native-test.py --peer-x11 --timeout 150 -- \
  python3 -B build-support/linux_dnd_scenarios.py \
  --roles /home/wgh/dev/cross_platform_app/oink/.release-tools/linux-probes/dnd-roles --suite all
```

说明：运行器外层自动加用户级 `systemd-run --scope`（4 GiB/CPU 200%/Tasks 256），要求私有 Xephyr/Xauthority/DBus/Pulse/PipeWire/HOME/XDG；失败保留目录，正常清理。`--peer-x11` 启动第二套完全独立会话（同一 scope），主环境通过 0600 JSON 提供 peer 描述。不要绕过 `--inside-scope`，不要把运行器指向宿主 `DISPLAY`。

### 7.5 FUSE 用户终端（未执行）

```bash
python3 /home/wgh/dev/cross_platform_app/oink/build-support/linux-native-test.py --timeout 12 -- \
  /home/wgh/dev/cross_platform_app/oink/.release-tools/linux-probes/fuse \
  > /home/wgh/dev/cross_platform_app/oink/.release-tools/linux-probes/fuse-user.log 2>&1
```

执行后代理直接读取日志核验，不要让用户描述现象。代理环境 `NoNewPrivs=1` 导致 setuid `fusermount3` 被禁，这是已知阻塞；不重试、不借其他入口绕过、不放宽安全设置。

### 7.6 Rust 与前端（阶段 2 通过后使用）

```bash
export https_proxy=http://127.0.0.1:7890 http_proxy=http://127.0.0.1:7890 no_proxy=localhost,127.0.0.1
systemd-run --user --scope --quiet -p MemoryMax=8G -p CPUQuota=200% -p TasksMax=256 \
  env CARGO_BUILD_JOBS=2 cargo check --locked --workspace --all-targets

export PATH="$PWD/.release-tools/node-v24.14.1-linux-x64/bin:$PATH"
npm run -w apps/oink typecheck
npm run -w apps/oink test -- --maxWorkers=2
npm run -w apps/oink build
```

完整门禁见计划 §Verification；Cargo jobs=2、测试线程=2，前端历史基线为 104 文件/1516 测试通过，但存在存量 `act(...)`、动态导入和大 chunk 告警，最终要求零告警并区分存量/本轮。

## 8. 已知坑与平台限制

1. **禁止向真实 `DISPLAY=:0` 注入键鼠/XTEST**：曾导致用户键盘失效；自动输入只在私有 Xephyr。
2. **Xephyr 会向宿主转发 virtual keyboard/mouse**：运行器已仅在私有 display 内校验并禁用它们，保留 core/XTEST；新环境若出现 `source=6` 事件，先查这是不是宿主转发，不要误判产品/拖拽回归。
3. **新 X 客户端没有默认 ClientPointer**：`XIGetClientPointer` 初始返回 0/0；先做一次 `XQueryPointer` 初始化再断言（输入矩阵已按此修复）。
4. **PipeWire 身份在 Client 上**：Node 无 `application.process.id`；PID 在 `client.id` 指向的 `PW:Client`。按 Node→Client→PID 同快照校验，缺失/歧义/外来身份 fail-closed。
5. **pw-cat 可能有 Pulse 镜像**：存在时记录 `not_exercised_pulse_mirrored`；未镜像 native-only 只有离线覆盖，不能宣称已实测。
6. **`pactl -f json list modules` 无 index**：清理用 `pactl list short modules` 差集；不要按 JSON 字段猜编号。
7. **source RUNNING ≠ 有消费者**：cork 后 source 仍可能 RUNNING；开麦看 registry 身份、active target link 和当前状态，不只看名称/状态。
8. **sink mute 会静音 monitor**：生产不能直接依赖 sink 静音；保捕获静音必须走独立播放支路并验证发送端 RMS。
9. **GTK 源拖拽 grab 不能被抢占**：已有 pointer grab 时新 grab 返回 `AlreadyGrabbed`；回源保持同一 context，不能伪造第二次 ButtonDown。
10. **500 ms 是测试控制预算**：产品只有 3 s preparation 与 5 s ordinary-input 过期；不要把探针回执写成产品 fence 时限。
11. **双端 DnD 是机制桥**：源 selection 实读→测试桥→proxy selection→target 实读；不是产品网络传输，也没有显示器边缘检测。
12. **Xephyr 不暴露手势**：宿主只读能力也不等于完整硬件验收；物理触点/手势/精细滚动必须用户参与。
13. **FUSE 与 `NoNewPrivs`**：代理内挂载 EPERM 是已知阻塞，不是缺包；不要绕过或要求 root。
14. **原生测试串行**：两个隔离桌面/音频服务同时跑会争资源；不要并发启动多组原生套件。
15. **单文件 ≤500 行**：新增/拆分源文件按职责拆分；不要顺带全仓重构。
16. **不要用 `not(windows)` 当 Linux**：逐模块区分公共 Unix 与平台专属；保留 macOS/Windows 路径。
17. **Node 版本**：系统 Node 可能是 24.22.1；门禁必须用 `.release-tools` 的 24.14.1。
18. **代理与网络**：cargo/git/gh 下载需要显式代理 env；HTTPS GnuTLS 握手被 RST 时可用显式 `git@github.com:...` SSH URL，但本轮不推送。
19. **用户浏览器/桌面**：只复用已有标签；不新开用户标签、不碰真实登录态；UI 自动化用隔离实例。
20. **提交/发布**：未获用户明确要求，不 commit、不 push、不打 tag、不发布、不上传符号。

## 9. 用户红线（逐条遵守）

- 不向真实 `DISPLAY=:0` 注入键盘/鼠标/XTEST 事件；UI 自动化只用隔离环境。
- 不改 udev、用户组、X server 访问控制、防火墙；不以 root 运行应用。
- 不静默改 Rust/MSRV/Node/npm 锁、不改全局默认工具链；锁定版本不可得时暂停并报告。
- 不修改 GitHub Actions/发布流程；不提交、推送、上传符号或发布产物（除非用户明确要求）。
- 不输出真实按键、剪贴板内容、音频内容、凭证；不截图敏感数据。
- 不先全量降低实现标准（mock、空 stub、返回 `None`、隐藏功能）来“先跑起来”。
- 不删除探针/负面测试文件来通过门禁；需要排除时改忽略配置并说明。
- 不拖死开发机：新构建先限额；后台任务等通知，不轮询/sleep。
- 不把未实测的项目标为“已通过”；仅编译或 mock 通过不能宣称完整支持。
- 需要用户扫码/物理授权/真实桌面操作时，推到该步停下等用户，不代做、不提前消耗有效期。
- 自动输入仅限私有 Xephyr；保持私有 HOME/XDG/DBus/Pulse/PipeWire/Xauthority、超时、限额、归属校验和失败保留。
- 系统依赖、真实 HID/麦克风/锁屏与 Windows 操作由用户执行；helper/uinput 另行给最小方案并请求授权，未经授权不实施。
- 新增及拆分源文件均 ≤500 行；不以扩大 cfg、隐藏功能或跳过断言代替实现。

## 10. 验收标准（DoD）与待用户事项

阶段 2 通过标准：

1. 第 3 节全部证据可复跑；第 4 节中所有“未实现/未验证”项完成或明确记录为阻塞/平台限制，不能把缺项归因于环境而不说明。
2. Qt/Nemo/Chrome/Firefox 矩阵有逐项源/目标角色和 Drop/返回/Cancel 结果；文件/目录、大 selection/INCR、新源 generation、peer/controller 恢复有数值/哈希证据。
3. 输入真实 toolkit 交付有应用侧观测；物理 HID/触摸板/触点/锁屏有用户证据或明确契约差距。
4. 音频机制保持 route/restart 回归通过；真实麦克风/真实应用使用虚拟输入由用户验收。
5. FUSE 普通终端日志满足全文/offset、EROFS、卸载、无残留、exit 0；未取得就保持阻塞。
6. 全程无真实输入/剪贴板/音频正文输出，无宿主注入、无权限/udev/组/防火墙/CI/工具链修改。
7. 实施记录追加到 `doc/linux-x11/plan.md`；未获授权不提交/推送/发布。

阶段 2 通过后，按 `doc/linux-x11/plan.md` §3–8 继续 #5–#7，不以机制探针通过代替产品验收。

| 用户事项 | 状态 | 说明 |
|---|---|---|
| 安装 `qtbase5-dev` 等 toolkit 依赖 | 待执行 | `bash build-support/linux-deps.sh --install --native-tests --toolkit-tests` |
| FUSE 普通终端验收 | 待执行 | 命令见 §7.5；代理读取 `fuse-user.log` |
| 物理 HID/触摸板/触点/锁屏 | 待硬件 | 代理给短步骤，用户操作，代理读日志/截图 |
| 真实麦克风/真实应用选虚拟输入 | 待硬件 | 私有合成音频不能替代 |
| Windows 测试机互通 | 待安排 | 优先现有 Windows 包；新构建需授权；不改 CI |
| 提交/推送/发版 | 未授权 | 需用户明确指令 |

## 11. 交接记录

| 日期 | 代理/会话 | 范围 | 结果 | 产物 |
|---|---|---|---|---|
| 2026-11-17 | 第一轮会话 | 工具链/依赖、隔离运行器、输入/DnD/音频/FUSE 探针、Rust 基线 | 机制探针多数通过；FUSE 待用户；产品未开始 | 第一轮交接、`doc/linux-x11/plan.md` |
| 2026-11-17 | 第二轮会话（基线 `6703e88`） | 双端运行器证据、GTK 跨进程/双端 DnD、输入矩阵、音频 route/restart、日志门禁 | 8 场景 DnD、26 项输入、187 单测、6 个零告警重建通过；Qt/真实应用/文件载荷/INCR/重连/FUSE/硬件仍缺；阶段 2 未通过 | 21 个新增 `build-support/` 文件、plan.md 追加记录、本文件 |

**下一次交接时：**

1. 在 `doc/handover/` 新建 `YYYY-MM-DD-<范围>-<主题>.md`，不要覆盖本文件或第一轮快照；更新 `doc/handover/README.md` 索引。
2. 实施记录追加到 `doc/linux-x11/plan.md`，保留历史。
3. 先核实 HEAD、`git status`、依赖检查和最新日志，再复述状态；旧行号可能变化，用 `path:line` 重新核对。
4. 只有用户明确要求才提交；转交文档本身也遵守同一规则。

## 附录：关键文件与日志

| 文件 | 用途 |
|---|---|
| `build-support/linux-native-test.py` | 隔离入口；`--peer-x11` 双端会话；统一退出/清理证据 |
| `build-support/linux-deps.sh` | 基础与 `--toolkit-tests` 依赖检查/用户安装 |
| `build-support/linux_input_probe.c` | grab/replay/swallow、独立 master、grab 冲突 |
| `build-support/linux_input_matrix*.c/.h` | 26 项键位/指针矩阵与纯校验器 |
| `build-support/linux_dnd_probe.c` | 旧 GTK 同进程 Drop/Escape 回归 |
| `build-support/linux_dnd_roles.c/.h`、`linux_dnd_native.c`、`linux_dnd_selection.c` | 独立源/catcher/proxy/target/sentinel 角色 |
| `build-support/linux_dnd_protocol.py`、`linux_dnd_scenarios.py` | 控制协议、场景编排、控制回执预算 |
| `build-support/linux_audio_probe.py` | 原音频机制回归 |
| `build-support/linux_audio_route_policy.py`、`linux_audio_route_runtime.py`、`linux_audio_route_probe.py` | 路由策略、运行时、需求门控实测 |
| `build-support/linux_audio_restart_probe.py` | 两代私有音频服务重启与租约失效 |
| `build-support/test_linux*.py` | 187 项 Linux Python 单测 |
| `.release-tools/linux-probes/*.log` | 本轮证据；`fuse-user.log` 待用户生成 |
