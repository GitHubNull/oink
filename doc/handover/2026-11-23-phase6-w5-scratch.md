# 阶段 6 W5（麦克风与虚拟录音源）续作清单

状态：W1–W4 已收口（本地提交未推送：W3 `97b1662`、W4 `debcb8e`）。W5 设计已调研完毕，实现未开始。
工作树干净。证据根 `.release-tools/linux-probes/phase6-gate-2026-11-23/w<K>/`。

## 已核定的 API 事实（勿再查）

- libpulse-binding 2.30.1：
  - `Context::set_default_source(name, cb)`（context/mod.rs:399）——私有栈不支持（W1 实证 set-default 系列 unsupported），真实宿主可用；`switch_default_input` 走它，ACK false 报清晰错误。
  - `introspect().load_module(name, argument, cb FnMut(u32))` / `unload_module(index, cb FnMut(bool))` / `get_module_info_list`（私有栈可加载模块，W1 notify 探针实证 generation 1→5）。
  - `get_source_output_info_list`、`get_source_info_by_name/list`；SourceOutputInfo 有 `source: u32`（索引）、`corked: bool`、`proplist`（取 `application.process.id`）。
  - `ChannelVolumes::default().set(channels, Volume)`、`avg()`；`Volume::NORMAL`=65536。
- 虚拟麦克风配方（探针已验证）：`module-null-sink sink_name=oink_mic` → `module-remap-source source_name=oink_input master=oink_mic.monitor`。plan 要求 properties 挂 `oink.uid`。
- 线程契约/私有栈坑全部在 `~/.qoder/projects/-home-wgh-dev-cross-platform-app-oink/memory/reference-private-audio-pipewire-quirks.md`（播放回调先装、Rc 环拆法、READY 门控、流断开杀节点等）。

## oink-audio 新增（每文件 ≤500 行）

1. `linux/mic_virtual.rs`（~180 行）：`ensure() -> Result<VirtualOutput, String>` 幂等。
   - 已有 1 个 `oink_input` source → 复用（残留复用，不 unload）；≥2 → `VirtualOutput::Ambiguous(devices)`，绝不 unload 他人模块。
   - 0 个 → load 两个模块，保存 module index 为自有（teardown 只 unload 自有）。
   - Ready 返回 `{ render_id: "pulse:sink:oink_mic", recording_name: "Oink Microphone" }`（对齐 macOS capture.rs virtual_output 形态；agent runtime 用 render_id 做本地播放 Sink）。
2. `linux/microphone.rs`（~300 行）：
   - `inputs()`：source 列表排除 `.monitor`；`default_input()`：server default_source 走 enumerate.rs 的 `concrete()`（符号默认 fail-closed）；`input_candidates()`：physical = 非 `oink_*` 非 monitor，built_in 取 proplist `device.form_factor == "internal"`（缺省 false）。
   - `Capture::start(id, permit, encoder)`：解析 `pulse:source:`；loopback 守卫（id 以 `LINUX_INPUT_UID` 结尾 / name == `oink_input` / `.monitor`）拒绝；查 source info 得原生 rate/channels；`connect_record` 原生 rate f32le（≤32 声道校验），START_CORKED → READY → uncork（私有栈探针层接线）；read 回调 peek → 帧平均单声道 → `resample::Resampler`（复用现有）→ 480*2 块 → `encoder.supply`；CaptureInfo backend `"pulse_record"`；`stop()` 断流，Drop 同上。`error()`/`device_invalidated()`/`stop_after_device_invalidation()` 与 capture.rs 同构。
   - `DemandMonitor`：`with_wake(Arc<Fn>)`/`revision()`/`sample() -> Demand`（对齐 windows.rs:264 契约）。自有连接订阅 SOURCE_OUTPUT|SOURCE；sample() 内解析 `oink_input` 索引（`get_source_info_by_name`），遍历 source outputs：三重过滤 = 非本进程 PID（proplist `application.process.id`）∧ 非 corked ∧ source 指向 oink_input。事件 bump epoch + wake；设备消失 → Demand.error。
   - `switch_default_input(expected, replacement)`：校验 replacement 在 candidates 中且 physical 非 loopback；`context.set_default_source` + 等 ACK。
3. `linux/route.rs`（~200 行）：照 `macos_mute.rs` 结构。
   - `PulseEndpointBackend` impl `recovery::EndpointBackend`：`read` = sink info（muted + avg/65536 f32，>100% 可表）；`mute`/`restore_mute` = `set_sink_mute_by_name`；`permanently_removed` 恒 false（pulse 不区分拔插与删除，注释写明）。
   - **自有写事件自滤**：pulse 订阅事件分不清来源。ownership_lost() = dirty latch ∧ `read() != expected`（expected = 租约写入时的状态）；restore_mute 写前重查 latch（macOS 同样语义，endpoint_mute::Lease 已强制）。
   - `MuteLease`/`recover(dir)` 镜像 macos_mute（`media_pause::before_restore()` 已有 Linux 臂则接，无则 no-op 登记）；跨进程锁用 fs2 flock 独立锁文件（照 lock_tests 模式）。
   - Linux capture 的 `mute_source` 仍 no-op（§5 已登记：sink 静音杀 monitor；endpoint 租约挂的是本地播放支路端点，由上层决定）。
4. `microphone/mod.rs`：cfg 臂 `any(macos, windows)` → 含 linux；新增 `pub const LINUX_INPUT_UID: &str = "org.oink.microphone.input.v1"`；`InputDevice::is_forwarding_loopback` 追加 LINUX_INPUT_UID；`selection.rs:44` 的显式 loopback 检查同样加；`permission()` Linux 返回 `SystemControlled`；Linux 导出 `Capture/default_input/input_candidates/inputs/switch_default_input/virtual_output/DemandMonitor`。
5. `linux/mod.rs`：`pub(crate) mod mic_virtual; pub(crate) mod microphone; pub(crate) mod route;`（名字冲突：crate 已有顶层 `microphone` 模块——linux 子文件命名 `linux/mic.rs` 或 `linux/microphone.rs` 内 `pub use`，注意 `crate::microphone` 与 `crate::linux::microphone` 路径并存无碍）。

## oink-agent 放开

- `audio/link/microphone.rs` + `microphone/runtime.rs`：所有 `#[cfg(any(target_os = "macos", windows))]` 臂加 linux；`runtime.rs:62` 的 `MAC_INPUT_UID` 判断改 `is_forwarding_loopback()` 或追加 LINUX_INPUT_UID。runtime.rs:862+ 测试用 coreaudio: 前缀构造——保留（测试数据），如需 Linux 臂测试仿照。
- agent `CaptureStream` trait（audio.rs）Linux 臂 W2 已接 capture；确认 microphone Capture 走 runtime 不经该 trait。

## 原生探针与验收

- 新增示例 `linux_probe_microphone.rs` + 探针 `build-support/linux_audio_microphone_probe.py`（纳入 500 行名单）：私有栈内 mic-bridge 式——探针层 PortConfig+pw-link 接产品 mic record 节点；tone 经 oink_input remap 进 mic 源验证捕获电平；DemandMonitor 用保活 parec 消费者验证 active/epoch；虚拟源 ensure 二次幂等、Ambiguous（手工再建一个 oink_input 验证）不 unload。
- `build-support/linux_hardware_acceptance.py`：mic-bridge=用户令牌模式（`OINK_HARDWARE_USER=1`，**推给用户跑**，不代跑）、mic-consumer=反向守卫仅 scope 内。
- 回归：audio 套件、audio-route 套件；全门禁（fmt/clippy/test jobs=1 no-fail-fast 对 12 条基线/Python 离线）。
- plan.md 追加 W5 实施记录；memory quirks 视新增坑补充。

## 禁区提醒（交接约束仍全部生效）

不 sudo/不装系统包/不改 CI/不推送不 tag；音频正文不入日志；原生探针串行 bounded() 4G/200%/TasksMax 1024；DISPLAY=:0 不注入；硬件验收令牌交用户。
