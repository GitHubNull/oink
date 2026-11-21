# Linux X11 阶段 6 交接 · 系统音频与虚拟麦克风（第十轮）

## 0. 一句话状态

**阶段 5（完整剪贴板、文件与跨机拖放）已完成 W1–W6 实现与 W7 门禁收口；W1–W7 全部仅本地提交，未推送、未 tag。阶段 6（系统音频与虚拟麦克风）未开始。** 真实 Cinnamon 会话的剪贴板/文件/Nemo/拖放验收项仍待用户执行（清单见 §9）；音频栈的私有 PipeWire 通路已探明（§6.4），可直接复用阶段 2 的音频探针套件。

## 1. 任务表（阶段 6，全部未开始）

计划正文：`doc/linux-x11/plan.md` §6。范围与顺序建议：

| ID | 范围 | 状态 | 入口/证据 |
|---|---|---|---|
| P6-1 | libpulse 异步枚举、捕获、播放、设备通知及控制适配；设备 ID 统一，不再用 CPAL 枚举结果配 unsupported 播放 | 未开始 | `crates/oink-audio/src/{lib.rs,playback.rs,device_notifications.rs}`；新增 `src/linux/` |
| P6-2 | 复用既有 claims、handoff、DSP、重采样、Opus、sink/session、recovery；实时回调不做文件、网络或无界分配 | 未开始 | `crates/oink-audio/src/{claims.rs,handoff.rs,dsp.rs}` |
| P6-3 | 系统音频发送/接收、多源混音、默认/指定输出、音量、静音、本地播放保留、设备切换/拔插及音频服务重启 | 未开始 | 阶段 2 探针 `linux_audio_probe.py`、`linux_audio_restart_probe.py`、`linux_audio_route_probe.py` 可复用 |
| P6-4 | 源端静音保留捕获信号；私有 sink/流路由用显式可恢复租约，不擅改默认设备；记录原状态，恢复不得覆盖用户后续修改 | 未开始 | `endpoint_mute.rs`、`idle.rs` |
| P6-5 | 物理麦克风捕获 + 可被真实应用选中的虚拟 recording source；复用 `StartPermit`（仅用户启用且有有效下游消费者时采集） | 未开始 | `crates/oink-audio/src/microphone/mod.rs`、`crates/oink-agent/src/audio/link/microphone.rs`；阶段 2 `linux_hardware_acceptance.py`（mic-bridge/mic-consumer） |
| P6-6 | 虚拟输入排除、默认输入选择与恢复；内部馈送流不算消费者；不承诺纯 Pulse API 未观测到的原生 PipeWire 流已覆盖 | 未开始 | 同上 |

现状（如实）：`crates/oink-audio` 无 `linux/` 目录，`playback.rs`、`microphone/capture.rs` 等仍走 CPAL；Windows 侧为 `windows_devices.rs`/`windows_source_devices.rs`。Linux 音频能力在 `status` 中如实为不可用，属预期。

## 2. 接手先做的只读核验

```bash
cd /home/wgh/dev/cross_platform_app/oink
git log --oneline -10                     # HEAD 应为阶段 5 W7 收口提交（88d6566 之后）
git status --short                        # 除文档/证据外应干净
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
python3 -m unittest discover -s build-support -p 'test_linux*.py'   # 309 项
```

不得跳过全量测试门禁：只需“29 条登记基线之外零新增”。基线名单：`doc/linux-x11/plan.md` 阶段 3 W4 节与 `.release-tools/linux-probes/phase5-gate-2026-11-21/w7/gates/failed-names.txt`。

## 3. 阶段 5 已完成行为（按层）

- **剪贴板（P5-1/2/3）**：Linux 平台模型（`NativeFormat::LinuxAtom`、`atoms.rs` 原子表、`linux_custom` denylist）；CLIPBOARD 所有权取得/放弃/owner 退出清理；TARGETS/TIMESTAMP/INCR 原始 ICCCM 服务与延迟提供；XFixes 变化观察；自写防回环（marker + change_count）；多格式（UTF8/TEXT/HTML/RTF/PNG/CUSTOM/隐私标记）。入口 `crates/oink-clipboard/src/linux/{mod,atoms,binding,capture,publish,selection,serve,watch,x11,sys}.rs`；agent 原生目标选择跟随宿主平台（`crates/oink-agent/src/clip.rs`）。
- **文件（P5-2/7）**：文件列表捕获/服务、隐私门控、延迟读、text/uri-list 与虚拟文件协商（`linux/{uris,virtual_files}.rs`）；错误不伪造 EOF，断网/取消/陈旧版本显式失败。
- **跨机拖放（P5-4）**：`DndBackend` 全方法接线（probe/hold/resume/finish/begin/cancel/source_content）；XDND 接收与源端；generation 隔离、取消、3 秒准备预算、原生完成确认。入口 `crates/oink-clipboard/src/linux/dnd/{mod,own,foreign,run,source,target,window}.rs`。F5（Chromium 源合成拖拽）/F6（Firefox 文件源）保持登记，不改写。
- **FUSE 桥（P5-5）**：私有只读 FUSE3 挂载桥接 `Namespace`；根不递归、分页、版本/offset、受限缓存、句柄/lease、卸载清理；无 `allow_other`；路径/符号链接/逃逸/冲突检查；网络不带源端绝对路径。入口 `crates/oink-clipboard/src/linux/fuse/{mod,fs,mount,handles,links,cache,fake}.rs`。
- **Nemo 粘贴为文件（P5-6）**：`paste_file/linux.rs` 生成 Selection/None 两个 `.nemo_action`（Extensions=any:），打包态检测（`oink` + 同目录 `installed.json`），Linux CLI dispatch 接线（`apps/oink/src-tauri/src/main.rs` 门控）。
- 阶段 3/4 的输入、显示、可视反馈、托盘、生命周期均保持；阶段 4 回归探针在 W7 全部复跑通过。

## 4. 已验证证据（2026-11-22，证据根 `.release-tools/linux-probes/phase5-gate-2026-11-21/w7/`，gitignored）

```bash
cargo fmt --all -- --check                                           # rc=0
cargo clippy --locked --workspace --all-targets -- -D warnings       # rc=0，零告警（bounded 限额内）
cargo test --locked --workspace --no-fail-fast                       # rc=101，12/12 均为 29 条登记基线
cd apps/oink && npm run typecheck && npm test && npm run build       # 1536/1536，build 绿
python3 -m unittest discover -s build-support -p 'test_linux*.py'    # 309 OK
```

- 门禁汇总：`gates-summary.txt`；日志在 `gates/`（含失败名单 `failed-names.txt`）。
- 阶段 5 原生探针（串行、私有 Xephyr）：剪贴板 11/11、文件 8/8、DnD 8/8、FUSE 4/4（`w2-clipboard.log`、`w3-files.log`、`w4-dnd.log`、`w5-fuse.log`）。DnD roles 二进制必须用当前源码重建（`bin/dnd-roles`，sha256 `c157533e...`；11-17 旧批次的 213e26bb 早于阶段 5 线协议扩展，不得混用）。
- 阶段 4 回归：输入 9/9（红证据 `input-probe.log` 为已知 WM 焦点竞态，绿跑 `input-probe-green.log`）、遮罩 14/14、壁纸 14/14；阶段 3 W2 10/10、W3 14/14、壳探针、托盘中文 18/18（`regression/`、`overlay-probe.log`、`wallpaper-probe.log`）。托盘首轮仅会话服务挂载未及时拆解（复跑绿），红轮不视为产品缺陷。
- 30 分钟混合负载：见 `gates-summary.txt` 负载节与 `load/`（本轮复用 `linux_phase4_load.py` 作共跑背景）。
- 功能矩阵：`matrix.md`（隔离通过/待验收/BLOCKED 分层）；真实会话清单：`real-session-checklist.md`（剪贴板 A、文件/FUSE B、拖放 C、Nemo D、稳定性 E）。

## 5. 产品代码现状（阶段 6 入口）

- `crates/oink-audio`：无 `linux/` 子模块；`playback.rs`、`microphone/capture.rs` 经 CPAL；Windows 设备枚举在 `windows_devices.rs`/`windows_source_devices.rs`。阶段 6 建 `src/linux/{mod,enumerate,capture,playback,devices,route,microphone}.rs`，libpulse 异步 API，设备 ID 统一。
- 复用层已就绪：`claims.rs`（claims/handoff）、`dsp.rs`（DSP/重采样）、`handoff.rs`（sink/session/recovery）、`endpoint_mute.rs`、`idle.rs`、`capture_permission.rs`、`StartPermit`（麦克风门控）。
- agent 侧：`crates/oink-agent/src/audio/link/microphone.rs` 与 `audio/` 线程模型跨平台已有，接 Linux 后端即可。
- 阶段 2 音频证据可复用：`build-support/linux_audio_probe.py`（系统音频）、`linux_audio_restart_probe.py`（服务重启恢复）、`linux_audio_route_probe.py`（路由）、`linux_hardware_acceptance.py`（mic-bridge 用户令牌 / mic-consumer 反向守卫）；单测 `test_linux_audio_*.py`。

## 6. 环境手册

- 构建顺序：先 `cd apps/oink && npm run build`，再 `cargo build -p oink-ui --features custom-protocol --bin oink`；`cargo build -p oink-agent --bins`；探针示例按需。二进制时间戳必须晚于 `dist/index.html`。
- 限额封装（每条 Bash 都要重新定义，shell 状态不保留）：
  `bounded() { systemd-run --user --scope --quiet -p MemoryMax=4G -p CPUQuota=200% -p TasksMax=1024 timeout N "$@"; }`。
  oink-agent 测试 profile 在 4 GiB 下单 job 构建，原生构建需 `env CARGO_BUILD_JOBS=1`。
- 显示：代理 shell 可能带私有 Xephyr 的 `DISPLAY`；真实桌面验收须 `DISPLAY=:0` 且由用户执行。原生探针一律 `DISPLAY=:0 python3 build-support/linux-native-test.py --timeout N -- <cmd>`，串行。
- C 探针 flags：peers 只需 `$(pkg-config --cflags --libs x11)`；阶段 4/5 探针需 `x11 xi xtst xfixes xext`；roles 六源重建见 §6.3。
- 网络：`https_proxy=http://127.0.0.1:7890`；git HTTPS 被 RST 时用 `git@github.com:GitHubNull/oink.git`。

### 6.1 XRes sysroot（本机头文件已被清理）

本机只剩 `libxres1` 运行库（无 `-dev`、无 `XRes.h`；旧 `OINK_XRES_SYSROOT` 已不在）。roles 重建先把 `libxres-dev` 解包到本地 sysroot（不安装系统包）：

```bash
cd /tmp && mkdir xres-deb && cd xres-deb && apt-get download libxres-dev
SYS=/home/wgh/dev/cross_platform_app/oink/.release-tools/xres-sysroot
mkdir -p $SYS && dpkg-deb -x libxres-dev_*.deb $SYS && cd /tmp && rm -rf xres-deb
# 链接时 -lXext 必须放在 pkg-config 输出之后（--as-needed 下 Xi 对 Xext 的依赖要最后一次解析）
cc -std=c11 -O2 -Wall -Wextra -Werror -I$SYS/usr/include -o <out>/dnd-roles \
  build-support/linux_dnd_roles.c build-support/linux_dnd_native.c build-support/linux_dnd_selection.c \
  build-support/linux_dnd_incr.c build-support/linux_dnd_stream.c build-support/linux_dnd_file.c \
  $(pkg-config --cflags --libs gtk+-3.0 x11 xi xtst xext) \
  -L$SYS/usr/lib/x86_64-linux-gnu -lXRes -lXext -Wl,-rpath,$SYS/usr/lib/x86_64-linux-gnu
```

产物 mtime 必须晚于全部输入；sha256 与源码批次绑定，写进当轮证据。

### 6.2 负载/探针清单（阶段 5 已建，直接复用）

- `linux_phase5_clipboard_scenarios.py --probe P --peer PEER`（11 场景）
- `linux_phase5_files_scenarios.py --probe P --peer PEER --files-probe FP --files-peer FPEER`（8 场景）
- `linux_phase5_dnd_scenarios.py --probe P --roles R`（8 场景）
- `linux_phase5_fuse_scenarios.py [--probe] [--big]`（4 场景，不占 Xephyr）
- 共跑背景：`linux_phase4_load.py --ui ... --backend ... --cli ... --overlay ... --duration 1800 --pause-every 300 --out <dir>`

### 6.3 私有 PipeWire 栈（阶段 6 关键事实）

`linux-native-test.py` 的私有 PipeWire 1.0.5 **没有任何 session manager**（`libpipewire-module-session-manager` 在 1.0.5 是空操作桩）。pulse 协议流永不自动链接；唯一已验证跨边界通路 = core pipewire socket（`PIPEWIRE_REMOTE=oink-test`，module-access unrestricted）+ `pw-cli set-param <id> PortConfig '{...}'` 强制 dsp 端口 + `pw-link <输出端口> <输入端口>` 显式连。pw-cat 命名节点用 `--properties='{ node.name = X }'`；未链接的 `pw-cat -r` 读端永远阻塞（不是静音）。会话销毁后 `pw-dump` 返回空 stdout 且 rc=0，调用前必须判空。自建节点有注册延迟：等对端出现时必须同时轮询双方节点 id，均非 0 再 link。宿主侧进程读真设备须剥离 `PULSE_SERVER/PIPEWIRE_RUNTIME_DIR/PIPEWIRE_REMOTE`。2026-11-20 端到端实证：宿主 440Hz 播放节点 → PortConfig+pw-link → 会话内录音节点，Link active、RMS −11.7 dB、nonzero 23960/24000。改路由行为后跑 audio/audio-route/audio-restart 三套件回归。

### 6.4 构建磁盘

`Cargo.toml` 已关 dev incremental 并限 `debug=1`；oink-agent 测试在 4 GiB 内须 `CARGO_BUILD_JOBS=1`。全量重编前先 `cargo clean -p` 相关包。

## 7. 已知坑与平台限制

- **XI2 事件掩码是协议位域**：阶段 4 已修正，勿重蹈。
- **WM 焦点竞态**：探针启动窗口后焦点可能短暂停在 WM 帧/其他窗口（如 1152）。红跑先判别环境竞态、保留红证据、复跑；不要直接改产品代码。
- **会话服务挂载拆解延迟**：托盘/壳探针结束时私有 runtime 下的 gvfsd-fuse/portal 挂载可能尚未拆解，cleanup_ok=false 但 checks 全绿——先核 `remaining_mounts` 归属，释放后复跑，不当作产品缺陷。
- **Xephyr 无合成器**：观感/动画/阴影不得用 Xephyr 当证据；Xephyr 偶发 “Cannot write display number to fd 4” 直接重跑。
- **测试基线**：29 条存量失败跨阶段真实存在，只允许“零新增”，禁止 skip/删断言；基线是上限不是配额，基线名转绿合法。
- **500 行门禁**：`build-support/test_linux_dnd_scenarios.py` 维护名单，新文件超限即拆。
- **F4/F5/F6 登记不改写**：触摸/手势 BLOCKED（硬件不可得）、Chromium 源合成拖拽、Firefox 文件源，红证据与绊线保留。
- **XTEST 边界**：严禁向宿主 `DISPLAY=:0` 注入；私有 Xephyr 内 XTest 可用；Chromium 无视 XTEST 拖拽起始（source 方向 BLOCKED）。
- **Wayland**：拒绝启动为当前设计。

## 8. 用户红线与禁止事项（继续生效）

- 不 sudo、不安装系统包（apt-get download + dpkg-deb -x 到本地 sysroot 不算安装）、不改 CI/GitHub Actions；不 tag、不发 Release。
- **推送授权不延续**：阶段 3（经用户确认部分）、4、5 的本地提交均未推送；后续推送、合并、发版必须另行取得用户明确授权。
- 不记录真实按键内容、剪贴板正文、音频正文、凭证；证据只记结构化状态/尺寸/计数/SHA-256。
- 保留红证据；不以单测替代交互证据。
- 构建/原生限额 4 GiB、CPU 200%、TasksMax 1024；原生探针串行。
- **严禁**向宿主 `DISPLAY=:0` 注入合成键鼠或 grab 真实键盘；真实桌面手测交用户。
- 音频阶段禁止把真实音频内容（录音正文）写入日志；只记电平、RMS、nonzero 计数等结构化量。

## 9. 验收标准（DoD）与待用户确认项

阶段 6 DoD：

1. libpulse 异步枚举/捕获/播放/通知/控制在私有 PulseAudio/PipeWire 栈有真实证据；设备 ID 统一。
2. 系统音频双向、多源混音、默认/指定输出、音量/静音、本地播放保留、设备切换/拔插、服务重启恢复均有探针或套件证据。
3. 源端静音保留捕获信号；私有路由用显式可恢复租约，恢复不覆盖用户后续修改。
4. 物理麦克风 + 虚拟 recording source 可被真实应用选中；`StartPermit` 门控（仅用户启用且有下游消费者）有证据。
5. 门禁：fmt、零告警 clippy、全量测试零新增（29 条基线内）、Python 离线全量、阶段 4/5 原生探针回归仍绿。
6. 音频正文不入日志；真实桌面（Cinnamon + 真实硬件音频设备）验收由用户执行。

待用户确认（阶段 5 遗留，不阻塞阶段 6 开工）：

- 阶段 5 真实会话清单（剪贴板 A1–A7、文件/FUSE B1–B5、拖放 C1–C4、Nemo D1–D3、稳定性 E）：`phase5-gate-2026-11-21/w7/real-session-checklist.md`。
- 阶段 4 遗留：真实 Cinnamon 输入/显示/反馈观感、多屏排列、双机 Remote 链路（Windows 测试机）。
- 触摸/手势：BLOCKED（F4，硬件不可得），不改写。

## 10. 交接记录

| 日期 | 代理 | 范围 | 结果 | 产物 |
|---|---|---|---|---|
| 2026-11-22 | Qoder（第九轮） | 阶段 5 W1–W7 | 剪贴板/文件/DnD/FUSE/Nemo 实现完成；门禁与原生探针全绿；阶段 4 回归复跑绿；30 分钟负载完成；待真实会话验收 | 本地提交 `b406a2d..88d6566` + W7 收口提交；证据 `.release-tools/linux-probes/phase5-gate-2026-11-21/w7/`；本文档 |
