# Linux X11 阶段 7 交接 · 协议、设置与桌面配套（第十一轮）

## 0. 一句话状态

**阶段 6（系统音频与虚拟麦克风）已完成 W1–W6 实现与 W7 门禁收口；阶段 1–6 全部仅本地提交，未推送、未 tag。** 真实 Cinnamon 会话的音频/麦克风验收已于 2026-11-27 大部分完成（F1/F2/F5、G1/G3/G4、H 通过；F3 部分通过；F4 设计隐藏；F6/G2 待条件/手工）；阶段 5 清单 A/B/D 已回填通过（C 拖放待用户手工）；阶段 7（协议、设置与桌面配套）未开始。

## 1. 任务表（阶段 7，全部未开始）

计划正文：`doc/linux-x11/plan.md` §7。范围与顺序建议：

| ID | 范围 | 状态 | 入口/证据 |
|---|---|---|---|
| P7-1 | `ClipPlatform`/native MIME 真实 Linux 分支，修所有二分平台假设；Unknown 不得凭相等关系获得解码权限 | 未开始 | `crates/oink-foundation/src/clipboard.rs`、`crates/oink-clipboard/src/{negotiation.rs,wire.rs}` |
| P7-2 | v1 字段编号保留；Linux↔Windows portable formats/文件 namespace/音频契约互通；新字段有旧版本解码测试 | 未开始 | wire 转换与协议测试 |
| P7-3 | `InputTransform`、路由/API 编解码、前端类型与映射编辑 Linux 目标；默认不套用 Mac Command 语义 | 未开始 | `crates/oink-foundation/src/input_transform.rs`、`apps/oink/src/api/types.ts` |
| P7-4 | 按真实能力修正所有设置与视图（布局/热键/输入映射/剪贴板/文件/音频/麦克风/配对/缓存/诊断/语言/托盘/登录项） | 未开始 | `apps/oink/src/settings/{model.ts,SettingsPage.tsx}`、`window/Upgrade.tsx` |
| P7-5 | `oink-autostart` XDG autostart desktop entry；仅用户显式操作登记；查询以磁盘真实状态为准 | 未开始 | `crates/oink-autostart/src/lib.rs` |
| P7-6 | 本地诊断导出、IPC/snapshot、隐私控制复用；Linux 原生崩溃信号有界数值记录（不输出内存/按键/剪贴板/音频）；仅隔离子进程验证 | 未开始 | `crates/oink-crash/src/{lib.rs,capsule.rs,local.rs}` |
| P7-7 | Linux 更新：明确告知无在线 feed；本地包升级先安全退出再交系统安装器由用户授权；**不建在线自动更新服务** | 未开始 | `crates/oink-agent/src/update.rs` |

## 2. 接手先做的只读核验

```bash
cd /home/wgh/dev/cross_platform_app/oink
git log --oneline -10                     # HEAD 应为阶段 6 W7 收口提交（42d1fe7 之后）
git status --short                        # 除文档外应干净
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
python3 -m unittest discover -s build-support -p 'test_linux*.py'   # 319 项
```

全量测试基线：29 条登记基线之外零新增（名单 `.release-tools/linux-probes/phase6-gate-2026-11-23/w7/gates/failed-names.txt`，12 条）。`npm run licenses:check` 需要 `cargo-about`，本机未安装；阶段 5/6 门禁均未含此项，如需引入先获用户批准安装。

## 3. 阶段 6 已完成行为（按层）

- **枚举/通知（P6-1）**：`oink-audio/src/linux/{enumerate,devices}.rs`，libpulse 异步 API（threaded mainloop）；设备 ID 统一 `pulse:sink:<name>`；符号默认 `@DEFAULT_SINK@` fail-closed；SINK/SOURCE/SERVER 订阅 + `service_generation()` 仅服务重启变化。
- **捕获（P6-1/3）**：`linux/capture.rs` monitor 捕获（48k/stereo、零分配回调、零音频正文日志）；worker 控制循环查 context+stream 状态；服务重启按 generation 失效重建。源端静音为登记平台 no-op（`capture_capability()=pulse_monitor_no_endpoint_mute`）。
- **播放（P6-2/3）**：`linux/playback.rs` `pulse:sink:` 播放；**write 回调必须先于 connect_playback 安装**；teardown 先 `set_write_callback(None)` 拆 Rc 环再 drop stream。
- **音量（P6-3）**：`volume/linux.rs` 端点（读实测回拍、写双 ACK、>100% 增益、外部修改经订阅 latch+wake 观测、fail-closed）。
- **麦克风（P6-5/6）**：`linux/mic_virtual.rs` 虚拟录音源（`oink_input` remap 源、幂等 ensure、外国模块守卫）；`linux/microphone.rs`+`mic_enumerate.rs`+`mic_demand.rs` 物理麦克风（显式 `pulse:source:`、绝不回退、拔插 invalidated、外国需求只读观察）。`StartPermit` 门控复用。
- **接线**：`oink-agent` `SystemAudio.capture_encoded`/`volume_endpoint`/麦克风 link；`audio/link.rs:2236-2287` cfg-free Status 重组装（27 字段 Linux 无缺口，W6 测试钉住）。
- 阶段 1–5 全部行为保持；W7 回归全绿（§4）。

## 4. 已验证证据（2026-11-23，证据根 `.release-tools/linux-probes/phase6-gate-2026-11-23/w7/`，gitignored）

```bash
cargo fmt --all -- --check                                           # rc=0
cargo clippy --locked --workspace --all-targets -- -D warnings       # rc=0，零告警（bounded 限额内）
cargo test --locked --workspace --no-fail-fast                       # 12/12 均为登记基线（不加 bounded：oink-agent rustc 峰值 >4 GiB 会 OOM 死循环）
cd apps/oink && npm run typecheck && npx vitest run && npm run build # 105 文件 / 1536 测试，build 绿
python3 -m unittest discover -s build-support -p 'test_linux*.py'    # 319 OK
```

- 门禁汇总：`gates-summary.txt`；日志在 `gates/`（含失败名单 `failed-names.txt`）。
- 阶段 5 探针回归（串行、私有 Xephyr）：剪贴板 11/11、文件 8/8、DnD 8/8、FUSE 4/4（`w2-clipboard.log`～`w5-fuse.log`）；DnD roles sha256 `c157533e...`（与阶段 5 同源同哈希，C 源未变）。
- 阶段 4 回归：输入 9/9（本轮一次通过，无焦点竞态）、遮罩 14/14、壁纸 14/14；阶段 3 W2 10/10、W3 14/14、壳探针、托盘中文 18/18 首轮全绿（`regression/`、`input-probe.log`、`overlay-probe.log`、`wallpaper-probe.log`）。
- 30 分钟混合负载：见 `gates-summary.txt` 负载节与 `load/`。
- 功能矩阵：`matrix.md`；真实会话清单：`real-session-checklist.md`（阶段 5 的 A–E + 新增音频 F/G/H）。

## 5. 产品代码现状（阶段 7 入口）

- `crates/oink-audio/src/linux/`：`mod/sys/enumerate/devices/capture/playback/microphone/mic_enumerate/mic_demand/mic_virtual/volume` 齐备；`cpal` 已收窄 macOS/Windows。
- `crates/oink-foundation`：剪贴板平台模型 Linux 臂在阶段 5 已建（`NativeFormat::LinuxAtom`）；`input_transform.rs` 的 Linux 目标为阶段 7 工作。
- `apps/oink`：设置页/类型/Upgrade 的 Linux 能力与文案修正为阶段 7 工作；托盘已含音频/麦克风子菜单（中文，托盘探针实证）。
- `crates/oink-autostart`、`oink-crash`、`oink-agent/src/update.rs`：Linux 臂未实现。
- 音频探针全套可复用：`linux_audio_{enumerate,notify,capture,playback,volume,microphone,route,restart}_probe.py` + 硬件验收 `linux_hardware_acceptance.py`（需 `OINK_HARDWARE_USER=1` 用户令牌）。

## 6. 环境手册

- 构建顺序：先 `cd apps/oink && npm run build`，再 `cargo build -p oink-ui --features custom-protocol`（**必须带 custom-protocol**：不带则 webview 加载 devUrl `http://localhost:1440`，无开发服务器时窗口永不现身——W7 回归曾因此红灯，根因是非 feature 构建被 cargo test 覆写）；`cargo build -p oink-agent --bins`；探针示例按需。二进制时间戳必须晚于 `dist/index.html`。
- 限额封装（每条 Bash 都要重新定义）：`bounded() { systemd-run --user --scope --quiet -p MemoryMax=4G -p CPUQuota=200% -p TasksMax=1024 timeout "$@"; }`；`cargo test` 全量不加 bounded（OOM 死循环），限额仅用于原生探针/受控构建。
- 显示：真实桌面验收 `DISPLAY=:0` 且由用户执行；原生探针 `DISPLAY=:0 python3 build-support/linux-native-test.py --timeout N -- <cmd>`，串行，私有 Xephyr 自动分配。
- C 探针：peers `$(pkg-config --cflags --libs x11)`（源文件必须在 flags 之前，--as-needed 会丢弃前置库）；roles 六源 + XRes sysroot 见阶段 5 交接 §6.3；`-lXext` 收尾。
- 网络：`https_proxy=http://127.0.0.1:7890`；git HTTPS 被 RST 用 `git@github.com:GitHubNull/oink.git`。
- 私有 PipeWire 栈关键事实（阶段 6 实证，勿再踩）：① 录音流 pw 节点名 = **客户端名**（客户端/流名统一 `oink-microphone`）；② 无 session manager 时流永不自动链接，探针层 PortConfig+pw-link 接线；③ 一流断开带走全部脉冲流节点；④ teardown 必须 disconnect 后有界等 `Terminated` 再 `mainloop.stop()`（否则 `pa_queue_free` abort）；⑤ write 回调先于 connect 安装；⑥ `object.register` 在 pw-dump 是 JSON 布尔。

## 7. 已知坑与平台限制

- Linux 源端静音不支持（monitor 捕获无端点租约）——登记，不改写。
- 私有栈（无 session manager）行为不预期出现在真实 Cinnamon；真实会话复现则单独上报。
- Xephyr 无合成器；观感证据须真实会话。触摸/手势 BLOCKED（F4）；Chromium 合成源（F5）/Firefox 文件源（F6）登记保持。
- 500 行单文件硬门禁（新文件必须加入 `test_linux_dnd_scenarios.py` 名单）。
- W2 类探针对宿主残留敏感：手测起过的 `target/debug/oink`/`oink-backend` 必须清干净，否则 `no_residual` 检查红灯（pkill 用 `[k]` 技巧防自匹配）。

## 8. 用户红线与禁止事项（继续生效）

不 sudo、不安装系统包、不改 CI/GitHub Actions；不 tag、不发 Release；本地提交不推送（推送/合并/发版须另行明确授权）；不记录真实按键内容、剪贴板正文、音频正文、凭证（证据只记结构化状态/尺寸/计数/SHA-256/RMS/nonzero）；保留红证据；原生限额 4 GiB/CPU 200%/TasksMax 1024；严禁向宿主 `DISPLAY=:0` 注入合成键鼠或 grab 真实键盘；音频日志只记电平/RMS/计数；真机危险操作只验证状态。

## 9. 验收标准（DoD）与待用户确认项

- DoD：门禁全绿（基线外零新增）+ 探针全绿 + 30 分钟负载绿 + 证据汇总/matrix/清单更新 + 本地收口提交。
- 2026-11-27 状态：阶段 6 真实会话 F1/F2/F5、G1/G3/G4、H 通过；F3 部分（听感手工）；F4 设计隐藏待可视复核；F6 缺第二发送端、G2 需物理拔插；阶段 5 A1–A7/B1–B5/D1–D2 通过（B3/B5 可视/D3 待手工），C1–C4 拖放需用户手工，E 退出清理由 H 覆盖。证据：`.release-tools/linux-probes/{phase5,phase6}-gate-*/w7/real-session-2026-11-26/`。
- 待用户：C 节跨机拖放手工；F3 听感；G2 物理拔插；F6 第二发送端；D3/B3/B5 可视手工；硬件麦克风验收 `OINK_HARDWARE_USER=1 python3 build-support/linux_hardware_acceptance.py mic-bridge --enable-mic --device <pactl source 名>`。

## 10. 交接记录

| 时间 | 代理 | 范围 | 结果 |
|---|---|---|---|
| 2026-11-23 | Qoder（第十轮） | 阶段 6 W1–W7 | 系统音频/麦克风实现完成；门禁与原生探针全绿；30 分钟负载完成；待真实会话验收；本地提交 `b22f6c6..42d1fe7` + W7 收口提交；证据 `.release-tools/linux-probes/phase6-gate-2026-11-23/w7/`；本文档 |
| 2026-11-27 | Qoder（第十一轮） | 真实会话验收（阶段 5 A/B/D + 阶段 6 F/G/H） | A1–A7（xclip/浏览器/LibreOffice）、B1/B2、D1/D2、F1/F2/F5、G1/G3/G4、H 通过；F3 部分（转发音量/静音/持久化自动化，听感手工）；H 缺陷修复（虚拟麦 stop 清理、FUSE 停止竞态）提交本地；C1–C4、G2、F6、B3、D3、B5 可视待手工/条件；回填见两清单 §回填 |
