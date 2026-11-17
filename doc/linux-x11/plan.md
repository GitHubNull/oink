# Linux X11 桌面完整适配计划

## Context

用户要求项目真正支持 Linux 桌面，能够正常运行并使用各项功能，而非仅消除编译错误或打开一个窗口。

已确认本轮边界：
- 基线为 **Linux Mint 22.3 / Cinnamon / Xorg / x86_64**，与用户的 **Windows 电脑**进行双向互通验收。
- 交付源码实现、可运行桌面应用及本地 **amd64 DEB + 可解压包**。
- 本轮不实现 Wayland；必须准确识别并解释限制，XWayland 不能冒充完整 X11 会话。
- **不修改 GitHub Actions/发布流程，不提交、推送、上传符号或发布产物。**
- 系统依赖安装、Windows 安装/配对和真实桌面手测需要用户参与；未经实测的项目保留为待验收。

当前证据：
- `apps/oink/src-tauri/Cargo.toml:37-43`、`src/main.rs:37-45`、`src/lib.rs`、`build.rs:5-6` 排除 Linux UI。
- `oink-input`、`oink-clipboard`、`oink-display` 的平台工厂没有 Linux 后端；音频播放和麦克风也有 unsupported 分支。
- 不能只批量扩大 cfg：`shell.rs:69-76` 缺少 Linux 窗口创建路径；`settings/model.ts:86` 把非 Windows 当 Mac；`InputTransform` 只有 Mac/Windows 配置；clip/dnd 接收端存在非 Mac 即 Windows 的分支。
- 网络、加密、配对、路由、内容命名空间、DSP、XDG 数据路径和部分 Unix 生命周期已可复用，不重写这些公共机制。
- 工具链锁定 Rust 1.99.0、Node 24.14.1、npm 11.11.0；当前 shell 所选工具链不匹配，尚未编译验证。已有 GTK3/WebKitGTK4.1/X11/Ayatana/libpulse，缺 ALSA/FUSE3 开发元数据；Xephyr、fusermount3 和可访问的 `/dev/fuse` 已存在。

## 实施原则

1. 保留现有 crate 边界，按领域增加 `linux` 模块，不另造全局平台框架，不加入用于绕过实现的产品 feature flag。
2. 按“环境与关键机制 → 后台/界面 → 原生能力 → 集成 → 安装与双机验收”推进。阶段门槛失败先解决，不以 mock、空列表、假成功或隐藏功能替代。
3. 初始化失败通过现有状态/错误模型呈现；窗口仍能说明依赖和权限问题。仅实际初始化成功的能力对外公告。
4. 不全仓机械替换 `not(windows)` / `not(macos)`；逐个区分公共 Unix 行为与平台专属行为，保留 Mac/Windows 路径。
5. 新增文件按职责拆分，每文件不超过 500 行；不顺带全仓重构。

## 1. 工具链、依赖与安全验证环境

- 核验实际工具路径，准备仓库锁定的准确版本，不静默降低 MSRV、更改版本约束或改用 nightly。锁定版本无法获得时暂停并报告。
- 增加 Linux 依赖检查/安装脚本，明确列出 Tauri、X11 扩展、XKB、ALSA、PulseAudio、FUSE3、字体/图形和 DEB 工具依赖；脚本可重复检查，提权安装交用户执行。
- 默认使用 PulseAudio API 兼容本机 pipewire-pulse，不无条件安装原生 PipeWire 开发依赖；仅在完整流/需求检测确实需要时补充。
- 新增隔离原生测试入口：独立 Xephyr/Xauthority、`OINK_HOME`、XDG 目录、DBus、私有音频服务器/socket 和 FUSE 挂载根。禁止连接失败后回落到宿主桌面或宿主音频。
- 新构建使用用户级资源限制及超时；Cargo jobs=2、测试并发=2。开发服务器/浏览器用于 UI 验证，但不替代 Tauri/WebKitGTK 原生验收。
- 不向真实 `DISPLAY=:0` 自动注入键鼠，不改 udev、组、X server 访问控制、防火墙，不以 root 运行应用。用户真实桌面的全局输入、锁屏和物理设备测试交用户操作。

关键位置：`rust-toolchain.toml`、根 `package.json`/`.npmrc`、各平台 crate 的 `Cargo.toml`；新增 `build-support/linux-deps.sh`、`build-support/linux-native-test.py`。仅新增实际需要的依赖，更新锁文件及已有许可证清单。

## 2. 优先验证三项决定成败的原生机制

### 2.1 输入拦截、来源识别与拖拽兼容

采用 XI2/XKB/XFixes/XTest 的 X11 后端方向，先用隔离测试验证具体 grab/replay 和注入设备方案：
- 普通模式必须原生放行，远控模式必须真正阻止本地交付；只观察 XI_RawMotion/XRecord 不能满足 `Disposition::Swallow`。
- 验证同步 grab/replay、远控主动抓取与 GTK/Qt 原生拖拽抓取共存；不得依靠反复解除/重抓造成输入漏出。
- 验证独立注入设备身份能否被应用正常接收并被捕获侧明确识别。不能把所有 XTEST 输入视为自己，也不能用全局时间窗吞掉用户或其他软件事件。
- 物理 HID↔XKB 键位、左右修饰键、AltGr、自动重复、媒体键、加速/原始位移、精细滚动及手势分别验证。
- 完整触点注入不假定由 XTest 自动支持；若必需最小权限 helper/uinput，先给出具体方案并另请授权，不能自行启用或把未实现手势改写为“已支持”。

### 2.2 XDND 原生拖拽保持与返回

验证跨边界 catcher、原 source/selection 的保持、远端 Begin/Drop、回到同一次源拖拽及 Cancel；回源不能伪造新的 Down/Begin。输入 fence 不得被 selection 读取或远端文件准备阻塞。分别验证 Nemo、GTK、Qt 和浏览器。

### 2.3 音频捕获、静音与虚拟麦克风需求

在私有音频服务验证 monitor 捕获、播放、源端静音后信号是否保留，以及虚拟麦克风消费者的开始/停止/cork/异常退出。排除本应用接收输出和虚拟输入形成的循环；不能预设 sink mute 对 monitor 无影响。

**阶段门槛：三项得到实测证据再扩展完整实现。若既定权限下无法达成契约，报告具体阻塞和最小变更，不能降低交付标准。**

## 3. Linux 入口、后台及桌面生命周期

- 启用 Linux Tauri 依赖、资源构建、main/lib 模块及真正的后台工厂。
- Linux UI 执行一次后台启动：ownership 检查 → 必要时启动 → PID/build/IPC 握手 → 订阅。慢启动有边界和明确错误，不产生第二套输入抓取。
- 复用 `AgentHost::start_platform_with`、`instance`、`check_backend_ownership`、`spawn_detached`、`follow`、`wait_previous_pair`、`reopen_entry` 和现有停止通路。
- UI/WebKitGTK 构建和窗口操作在 UI 主线程；后台 DnD 原生主循环位于后台进程主线程，普通 worker 通过队列投递，禁止以现有 Linux 直接执行闭包替代 GTK 调度。
- 单实例、托盘菜单/状态、主窗口、设置/配对/升级等窗口、面板定位、关闭后恢复、`--autostart`、停止和重新打开全部接通。
- 关闭窗口或 UI 消失不停止共享；显式退出清理输入/拖拽/音频/文件；后台崩溃不偷偷自动重启。
- 启动前检查真实会话类型，Wayland/无图形会话给出准确提示，不启用不安全的部分全局输入。

关键文件：
- `apps/oink/src-tauri/{Cargo.toml,build.rs,tauri.conf.json,src/main.rs,src/lib.rs}`
- `apps/oink/src-tauri/src/{app.rs,service.rs,shell.rs,native_menu.rs,native_controls.rs,system_locale.rs}`，新增 Linux tray/window 模块。
- `crates/oink-agent/src/{host.rs,backend/mod.rs,backend/launch.rs,node/assembly.rs}`，新增 `backend/linux.rs`。
- 复用 `crates/oink-foundation/src/paths.rs:87-100` 的 XDG 路径及 `OINK_HOME` 隔离。

## 4. 输入、显示与原生可视反馈

### 输入
- 在 `crates/oink-input/src/linux/` 实现经过第 2 阶段验证的 capture/inject/XKB/cursor/session 适配，接入 `lib.rs`。
- 必须实现生产使用的 `start_loop`，复用 `capture_loop::CaptureLoop` 的 wake/poll/deadline 和重入规则、`session_gate` 的 epoch、现有路由和注入清理。
- 隐藏→停靠→远控、隐藏状态下恢复坐标→Local→显示严格排序；完成状态以服务器处理后的观测为准。
- 覆盖快捷键/紧急恢复、相对模式、滚动/手势、按键映射、物理设备热插拔、暂停、断网、锁屏/会话切换、休眠恢复。未知锁定状态不得当作已解锁；只释放本应用持有的注入状态。
- 接入 logind/会话锁定观察、显示电源观察和允许的唤醒行为，不实现 Linux 登录前凭据控制。

### 显示及反馈
- 在 `crates/oink-display/src/linux/` 使用 XRandR 实现显示枚举、活动几何、镜像/旋转/缩放/负坐标/热插拔及显式排列事务。
- 复用 `native_configuration`、`apply_native_origins`：写前重读、陈旧版本拒绝、失败恢复并上报实际结果；普通同步不改系统布局。
- 接入 Cinnamon 壁纸读取与变化观察，不把不存在壁纸识别为显示失败。
- 为 `oink-locator` 的拖尾、定位、投影、通知分别补 Linux backend，复用已有 raster 和动画；修复 Linux stub 构造签名不匹配。
- 使用 ARGB 窗口及 Shape/XFixes 输入区域：装饰层穿透，通知交互区域可点，不抢焦点，不与 DnD catcher 抢占。
- 用 Pango/Fontconfig/Cairo 等系统字体路径补中文文字渲染，不以空白字体 fallback 验收。

关键位置：`crates/oink-input/src/lib.rs`、`crates/oink-agent/src/threads/{input.rs,inject.rs}`、`crates/oink-display/src/lib.rs`、`crates/oink-locator/src/{lib.rs,text.rs,projection/mod.rs,notification/mod.rs}` 及新增 Linux 子模块。

## 5. 完整剪贴板、文件和跨机拖放

- 在 `crates/oink-clipboard/src/linux/` 实现 CLIPBOARD selection 生命周期、XFixes 变化观察、TARGETS/TIMESTAMP/INCR、多表示形式、所有权变化和取消；PRIMARY 不擅自当作需要同步的用户复制。
- 支持文本、HTML、RTF、PNG、URL、文件/目录、隐私标记及 Linux 本机 MIME 形式。捕获元数据和正文读取分离，正确处理 owner 退出、延迟提供及自写防回环。
- 复用 `ClipboardBackend`、`Provider`、`Translator`、`ReaderGuard`、grants、namespace、received/cache，不另建文件传输协议。
- 完整实现 `DndBackend` 的 probe、hold/resume/finish、begin/cancel/source_content，接既有 `dnd/link_platform.rs` 和 `dnd/preparation.rs`；保留 generation、取消、三秒准备预算及原生完成确认。
- 远程文件使用**私有只读 FUSE3**桥接现有 `Namespace`：根发布不递归下载，目录分页，按版本/offset 读取，受限缓存，打开句柄和 native lease 管理生命周期。
- 挂载根只对用户开放，不使用 `allow_other`；路径、符号链接、名称冲突和逃逸检查复用并补测。仅本机接收程序看到挂载 URI，网络不得携带源端绝对路径。
- 错误不得伪造 EOF；断网/取消/磁盘满/配额/陈旧版本明确失败。退出取消并卸载，残留只能在验证归属后处理，不清理未知挂载。
- 补 Nemo“粘贴为文件”入口并复用现有转换逻辑；安装和开关关闭时不会留下失效入口。
- 大文件、目录及浏览器沙箱访问必须实测。不能靠先全量下载或只粘贴文件名冒充完整能力。

关键位置：`crates/oink-clipboard/src/{lib.rs,dnd.rs,content.rs,content/namespace.rs}`、`crates/oink-agent/src/{clip.rs,dnd.rs,dnd/link_platform.rs,dnd/preparation.rs,paste_file}` 及新增 Linux selection/content/dnd/virtual_files 模块。

## 6. 系统音频与虚拟麦克风

- 在 `crates/oink-audio/src/linux/` 增加 libpulse 异步枚举、捕获、播放、设备通知及控制适配；设备 ID 统一，不再用 CPAL 枚举结果配 unsupported 播放。
- 复用既有 claims、handoff、DSP、重采样、Opus、sink/session、recovery；实时回调不做文件、网络或无界分配。
- 支持系统音频发送/接收、多源混音、默认/指定输出、音量、静音、本地播放保留、设备切换/拔插及音频服务重启。
- 根据第 2 阶段结果实现能保留捕获信号的源端静音。需要私有 sink/流路由时使用显式可恢复租约，不擅改默认设备；记录原状态，恢复不得覆盖用户后续修改。
- 增加物理麦克风捕获和可被真实应用选中的虚拟 recording source。复用 `StartPermit`：仅用户启用且有效下游消费者存在时采集，最后消费者离开/需求未知/撤销/租约过期立即停止。
- 扩展虚拟输入排除、默认输入选择与恢复；内部馈送流不算消费者。不承诺纯 Pulse API 未观测到的原生 PipeWire 流已经被覆盖。

关键位置：`crates/oink-audio/src/{lib.rs,playback.rs,device_notifications.rs,microphone/mod.rs}`、`crates/oink-agent/src/audio/link/microphone.rs` 及新增 Linux capture/playback/devices/route/microphone 模块。

## 7. 协议、设置与桌面配套

- 为 `ClipPlatform` 和 native MIME 重建增加真实 Linux 分支，修复 clip/dnd/格式协商中所有二分平台假设；Unknown 不能据相等关系获得原生数据解码权限。
- 保留现有 v1 字段编号、HID 事件和传输协议。Linux↔Windows 使用已经定义的 portable formats、文件 namespace 和音频契约；新增字段/能力遵循已有扩展规则并有旧版本解码测试。
- `InputTransform`、路由/API 编解码、前端类型及映射编辑增加 Linux 目标；已有配置保留含义，默认不套用 Mac Command 语义。
- 根据真实能力修正所有设置和视图：布局、热键、输入映射、剪贴板/文件、音频、麦克风、配对、缓存、诊断、语言、托盘与登录项。Mac File Provider、Windows UAC/自动解锁/登录前服务保留明确的原平台边界。
- `oink-autostart` 使用 XDG autostart desktop entry，仅用户显式操作时登记，查询以磁盘/系统真实状态为准；安装和普通启动不自动开启。
- 复用本地诊断导出、IPC/snapshot 和隐私控制；补 Linux 原生崩溃信号的有界数值记录，使用现有 capsule/local 存储，不输出内存、按键、剪贴板或音频。信号处理遵守 async-signal-safe，不在其中锁 mutex/分配；与已有输入恢复终止处理正确衔接。仅在隔离子进程验证。
- Linux 更新走本地包管理：检查入口明确告知没有 Linux 在线 feed，不能无限等待或假称已是最新版；本地包升级先完成安全退出，再交系统安装器由用户授权。**本轮不建立在线自动更新服务。**

关键位置：`crates/oink-foundation/src/{clipboard.rs,content,input_transform.rs}`、`crates/oink-clipboard/src/{negotiation.rs,wire.rs}`、相关 agent/wire 转换、`apps/oink/src/{api/types.ts,settings/model.ts,settings/SettingsPage.tsx,window/Upgrade.tsx}`、现有语言文件；`crates/oink-autostart/src/lib.rs`、`crates/oink-agent/src/update.rs`、`crates/oink-crash/src/{lib.rs,capsule.rs,local.rs}` 及 Linux 实现。

## 8. 本地构建、打包与安装

- 扩展 `build-support/build.py` 与 `package.py` 的 Linux target/platform/host/角色/后缀/构建元数据，不把非 Mac 当 Windows，不拉 Windows service/credential provider 或 Sparkle。
- 新增 `packaging/linux/build.py` 及必要 package assets；配置 `packaging/identity.json` 的 Linux 元数据并保持现有产品身份。
- DEB 携带 UI、后台、CLI 和实际需要的辅助资源，完整嵌入前端，生成图标、desktop entry、正确运行依赖；不依赖源码目录、node_modules 或开发服务器。
- 可解压包保持相同目录/启动语义，清楚标注发行版及运行库基线，不称为任意 Linux 通用包。
- 输出到仓库外指定目录，记录架构、版本、源码 provenance、哈希和许可证；Linux 本地路径明确不调用符号上传或外部发布。
- 验证安装、普通用户启动、重复启动、升级/取消、退出和卸载；默认保留用户配置，不自启、不改防火墙、不删除未知数据。
- 更新现有 `README.md`、各语言 README 和 `apps/oink/README.md` 中的支持/构建说明，按实际验证结果表述；不新增无关设计文档。

## Verification：验收和完成标准

### 自动化检查（在依赖就绪并获准实施后执行）

分 crate 运行聚焦测试，再执行以下完整门禁；首轮失败记录为基线，修复与本次 Linux 支持有关的失败，不靠跳过测试获得通过：

```bash
cargo fmt --all -- --check
CARGO_BUILD_JOBS=2 cargo clippy --locked --workspace --all-targets -- -D warnings
CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 cargo test --locked --workspace
CARGO_BUILD_JOBS=2 cargo check --locked --workspace --release
CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 cargo test --locked --workspace --release --lib
npm run -w apps/oink typecheck
npm run -w apps/oink test -- --maxWorkers=2
npm run -w apps/oink build
npm run licenses:check
python3 -m unittest discover -s packaging/tests -v
python3 -m unittest discover -s build-support -p 'test_*.py' -v
```

- 复用 `tests/support` 的 FakeInput、内存剪贴板和 loopback；补 Linux 后端契约测试，而非只测试假后端。
- 覆盖现有 input lifecycle/capture_modes、`link_scenarios` 断联释放与重连、DnD slow source/取消/回源、clipboard dedupe/policy/received 安全测试。
- 检查 release cfg、原生 harness 和非 Linux 专属目标，不以宽泛 allow/skip 掩盖编译问题。ignored/device/slow 测试仅在明确隔离环境按用例启用，不直接裸跑 `--slow`。
- 更新/安装测试使用本地材料，诊断测试不发送真实报告。Windows 原生回归在用户测试机完成；未运行的 Mac 原生测试明确列为未覆盖。

### 实际功能矩阵

| 领域 | 必须取得的证据 |
|---|---|
| 桌面壳 | 开发服务器+隔离浏览器走通页面，Tauri/WebKitGTK 原生窗口另验；配对/设置/面板、中文字体、托盘、单实例、关闭/重开、显式退出无控制台错误 |
| 输入安全 | 双向跨屏、连按/组合键/中文输入、滚轮/手势、相对模式、紧急恢复；断网、暂停、崩溃、锁屏/解锁后无卡键和遗留抓取 |
| 布局与反馈 | 屏幕枚举、排列、缩放、旋转/热插拔、壁纸、拖尾/通知/投影真实显示与点击穿透；多屏硬件不足则标记待验收 |
| 剪贴板 | 双向文本/HTML/RTF/图片/URL、多格式、隐私标记、owner 变化、大数据 INCR、防回环 |
| 文件与 DnD | 双向文件/目录、延迟读取和哈希一致；Nemo、GTK、Qt、浏览器落地；原拖拽跨边返回、Escape、断网、磁盘满及取消无幽灵会话 |
| 音频 | Linux↔Windows 双向播放、混音、输出切换、静音/恢复、拔插/重启；真实应用读虚拟麦克风，无消费者时物理麦克风关闭 |
| 桌面集成 | 登录项真实开关、粘贴为文件、诊断导出/崩溃记录、本地升级与失败提示；OS 专属能力有准确说明 |
| 安装包 | 不启动开发服务器亦能运行；安装/升级/卸载不破坏配置，不自动上传或设置登录项 |

### 双机与稳定性

- 使用用户 Windows 电脑完成配对和双向验收；优先验证现有 Windows 包的 portable 协议互通，若确需匹配的新 Windows 构建，在该机本地准备并另行授权安装，不擅改 CI。
- 自动注入只在独立 X server；真实桌面由用户执行短手测，随后自行读取约定的无敏感日志/截图核对，不让用户重复描述已观察到的现象。
- 30 分钟混合负载：双向输入切换、文件传输、系统音频及麦克风需求变化；检查 CPU/RSS 趋势、队列积压、FD/线程/挂载、回环、卡键与声音恢复。
- 各项按“已通过 / 缺陷 / 平台专属不适用 / 待用户或硬件验收”分别记录。功能未实现或无实测证据不能标为通过；仅编译或 mock 通过不得宣称完整支持。
- 最终交付本地包路径、启动方式、真实验收结果及剩余限制，保留可供用户复查的开发服务器/界面；不主动提交或发布。

## 实施记录（2026-11-17）

- 已添加 `build-support/linux-deps.sh`，默认只读检查；`bash -n` 通过。此前缺 ALSA/FUSE3 开发包；最新真实 `--check --native-tests` 已全部通过，未由代理执行 sudo 或修改设备权限。
- Node 24.14.1 / npm 11.11.0 已下载至忽略目录 `.release-tools/node-v24.14.1-linux-x64/`，官方 SHA256 已核对，未修改全局默认版本。`npm ci` 成功；原始依赖有一条 `whatwg-encoding@3.1.1` deprecated 提示。
- Rust 镜像安装因 TUNA 404 失败；官方直连下载过慢，确认其未使用本地代理后停止该次下载，用命令级 http(s)_proxy=127.0.0.1:7890 从同一官方源恢复安装。安装成功，rustc/cargo 1.99.0、rustfmt 1.10.0-stable、clippy 0.1.99 已逐项实际运行验证，未修改锁或全局默认工具链。
- 前端原始基线 104 文件 / 1516 测试通过，类型检查及生产构建通过；存量 `act(...)` 测试提示、动态导入及大 chunk 告警已记录，未宣称零告警。
- 新增 `build-support/linux-native-test.py`：私有 Xephyr/Xauthority（no-host-grab/TCP 禁用）、DBus、XDG/OINK_HOME、无硬件 PipeWire/Pulse。私有服务正常启动/退出与命令超时已实测。修复 DBus 激活环境隔离、PipeWire 客户端配置、清理失败保留目录/继续回收、实际 cgroup 任务数核验。依赖脚本及运行器共 45 项单测通过。
- 新增 `build-support/linux_input_probe.c`，严格拒绝宿主环境，`-Wall -Wextra -Werror` 编译通过。隔离实测：原生键 2 次；同步拦截后本地 0 次、Replay 后 2 次；远控 Swallow 本地 0 次、解除后 2 次。独立 master 的 XTEST source 与普通注入分离且 core/XI2 目标均收到事件。原生 pointer grab 期间新抓取返回 AlreadyGrabbed；不能靠抢占原 source 实现 DnD。专属 XTEST slave 不能 reattach（X error129），保留 master 方案。
- `linux_dnd_probe.c`（409行）主会话复核并重跑通过：Drop/Cancellation 两支均仅一次ButtonDown、同一source context返回、4处原grab保持；Drop数据传递和结束各1次，Escape无drop/数据并在ButtonUp前解除grab。仅GTK同进程局部机制，跨进程、Qt/Nemo/浏览器和产品网络XDND仍未验收。
- `linux_audio_probe.py`（462行）主会话复核并重跑通过，真实私有PipeWire1.0.5：tone monitor RMS5792.56；sink mute时RMS0，解除恢复，说明生产不能直接sink静音。parec/libpulse真实消费、cork/uncork、退出均符合预期；cork后source仍可能RUNNING，不能靠source state开麦。pw-cat原生录音2条active link/67584samples、Pulse对象身份映射1个消费者，退出归零。无WirePlumber场景须显式路由；仅本环境证明原生消费Pulse可见性。
- `linux_fuse_probe.c` 已以严格告警编译；实际普通用户私有挂载被拒绝EPERM，已定位代理继承NoNewPrivs=1而fusermount3为setuid，/dev/fuse权限和开发包并非原因。探针现明确失败，不放宽安全设置；需普通用户系统终端执行同一隔离入口，不能将此项报通过。
- 原始 `cargo check --locked --workspace --all-targets` 基线（jobs2/CPU200%/MemoryMax8G/TasksMax256/命令级代理）已完成，exit101；输出 `/tmp/qoder-cli-1000/-home-wgh-dev-cross-platform-app-rustDingdong/9abd72e9-f298-4509-a51c-a135992c3a16/tasks/b8uu3z591.output`。首个阻断为oink-locator的5处Linux缺口：notification/mod.rs:353缺platform、:514缺scale_for；projection/mod.rs:126缺platform；projection/annotation.rs:125缺platform；lib.rs:395传health而非平台stub的:605 new()签名无参。基线还有Linux未使用项/同文件多target告警。尚未用stub或cfg跳过掩盖；后续须真实Linux surface实现，不把这5错视为全部剩余范围。原生手势/物理热插拔及跨机验收仍未开始。
- 已向用户交接普通系统终端 FUSE 验收命令：`python3 /home/wgh/dev/cross_platform_app/oink/build-support/linux-native-test.py --timeout 12 -- /home/wgh/dev/cross_platform_app/oink/.release-tools/linux-probes/fuse > /home/wgh/dev/cross_platform_app/oink/.release-tools/linux-probes/fuse-user.log 2>&1`。尚未收到执行确认；用户返回后自行读取此日志，不让用户重复描述或重新安装依赖。
- 当前8个新增文件均在build-support，每文件不超过500行，git未提交；Linux依赖及运行器单测45项最新复跑通过，无原生Rust产品实现变更。任务#3已完成、#4进行中，#5–7尚未开始，不能把机制探针当作产品验收。
- 尚未修改产品后端/UI、向真实桌面注入输入、构建 Linux 产品安装包或开展 Windows 互通。多数产品实现仍未开始，后续继续按阶段门槛推进。

## 续作实施记录（2026-11-17，基线 6703e88）

- 接手时 HEAD 为 `6703e88`、工作区干净；原探针已在 `3ba1e57`、计划/交接已在 `6703e88` 提交。上方及旧交接的“未提交”是历史快照。本次续作实施期间不提交、不推送、不发版；收口后用户明确要求提交并推送，未发版。
- 本轮已实跑原 45 项 Linux Python 测试，全部通过；基础 `linux-deps.sh --check --native-tests` 全部通过。新增 `--toolkit-tests` 单独检查 Qt5 Widgets/Nemo/Chrome/Firefox，实际只读包检查停在缺少 `qtbase5-dev`；安装仍由用户执行，不取消 Qt 验收要求。
- `linux-native-test.py` 新增 `--peer-x11`：两套私有 Xephyr/Xauthority/HOME/XDG/DBus/Pulse 环境位于同一限额 scope，主环境的 `OINK_NATIVE_TEST_PEER_ENV` 指向 0600 JSON 描述；两端均校验、停止并核查挂载。统一 `native_test_summary` 保留探针退出码、服务状态、清理/挂载核查与最终退出码，任何失败不能被前面的成功覆盖。
- `test_linux_native_evidence.py` 新增双端隔离、部分失败、退出证据和清理失败测试；原 31 项运行器测试保留。当前运行器 43 项、依赖脚本 18 项分别实跑通过。
- `linux_input_probe.c` 增加专属 master 的 core/raw 各两次交付强断言；双端运行器实际运行成功：core/raw 各2、source11区别于普通source5、XI2目标2次，Replay/Swallow与grab冲突仍成立；两会话退出和挂载清理成功。日志 `.release-tools/linux-probes/input-current.log`。这不代表物理 HID 或手势已验收。
- 旧 GTK 回归本轮曾在 state0 安全中止。数值诊断确认事件来自 Xephyr virtual keyboard（source6），不是 XTEST keyboard（source5），不是产品或拖拽回归。运行器现在仅在私有 display 内校验并禁用 `Xephyr virtual keyboard/mouse` 的宿主转发，保留 core/XTEST；不触碰真实桌面设备。新增身份不符/未实际禁用的负面测试。临时诊断代码已移除。
- 关闭私有转发后，旧 GTK Drop/Escape 实跑通过：各一次 Begin/End、一次Down/Up、同context返回、各4次原grab保持检查，退出无残留。日志 `.release-tools/linux-probes/dnd-forwarding-isolated.log`。原音频探针也实跑通过：monitor RMS5792.56，sink mute后RMS0，parec/cork/pw-cat需求开始/停止成立；日志 `.release-tools/linux-probes/audio-current.log`。两者 `native_test_summary.result=0`，仅是原机制回归。
- FUSE 探针已从当前源码以 `-Wall -Wextra -Werror` 重新构建。约定 `fuse-user.log` 尚无可读取证据，不能据此认定用户未运行；代理仍受 NoNewPrivs 限制，不重试绕过。用户普通终端继续使用 §实施记录中同一命令，新的运行器会在日志中记录最终退出/清理结果。
- 当前仍为任务 #4／阶段2进行中：跨进程/双端XDND、输入矩阵、音频保捕获静音与需求门控正在补充；Qt/Nemo/浏览器全矩阵、FUSE、物理HID/触点与真实麦克风未取得完整通过证据。产品代码、阶段3–8、安装包、Windows互通均未开始，不把运行器或探针通过当成产品支持。
- 后续本轮音频路由实测：首次因 `pactl -f json list modules` 不含 index 失败；私有服务实查确认后，改用 `pactl list short modules` 取得可靠编号，新增负面用例，42项纯测试通过。命令 `python3 -B build-support/linux-native-test.py --timeout 180 -- python3 -B build-support/linux_audio_route_probe.py` 最新退出0；日志 `.release-tools/linux-probes/audio-route-current.log`，summary failures=0、cleanup_failures=0，运行器服务/清理/挂载核查全真。播放支路静音后扬声器RMS0、发送捕获仍5792.56；1/2kHz频谱证明接收不混入发送、停止后无回环尾音；两个真实消费者、cork、部分退出、最后正常/异常退出、未知需求、许可撤销/到期均验证实际发送流启停，最终starts=stops=7。身份/当前值比较恢复、对象替换保护及默认设备不变通过；非原子CAS。纯PipeWire消费者与受控服务重启仍在补齐，真实麦克风/产品接入未验收。
- 后续本轮输入矩阵实测：新增6个C/头文件，每文件≤500行，`-std=c11 -O2 -Wall -Wextra -Werror` 编译及独立纯校验器通过。首次失败原因是新X客户端尚未分配client pointer；最小私有实验 `XIGetClientPointer → XQueryPointer → XIGetClientPointer` 实证返回从0/0变为1/2，修正为查询初始化后再断言。命令 `python3 -B build-support/linux-native-test.py --timeout 60 -- .release-tools/linux-probes/input-matrix` 退出0；日志 `.release-tools/linux-probes/input-matrix-current.log`：26项全部通过，每项core+XI2两模式，failed=0/incomplete=0/cleanup_ok=true，运行器result=0。左右修饰键、独立AltGr、媒体键、人工重复/服务端定时重复、准确相对位移及滚轮4–7成立；仅合成私有X11，不代表GTK/Qt/浏览器、物理HID、精细滚动、手势或产品触点协议路径已验收。
- 后续本轮跨进程XDND：7个新增角色/协议/场景/单测文件已完整静态审查；主代理严格编译链接并实跑29项纯单测通过。运行 `linux-native-test.py --timeout 120 -- python3 -B build-support/linux_dnd_scenarios.py --roles /home/wgh/dev/cross_platform_app/oink/.release-tools/linux-probes/dnd-roles --suite same-server`，6场景通过；双端加 `--peer-x11` / `--suite dual-server`，COPY/drop及Escape两场景通过。日志 `.release-tools/linux-probes/dnd-multiprocess-current.log` / `dnd-dual-current.log`，两个运行器result=0、服务/清理/挂载核查全真。源保持一次Begin/Down、返回原context/grab，双端源9次held检查，实际48字节selection经catcher读取→测试桥→proxy selection→target读取并校验SHA-256；无CLIPBOARD哨兵破坏、源删除、幽灵grab。慢selection期间控制ACK、目标迟到generation、源/目标held期间退出成立。仍缺Qt/Nemo/浏览器、文件/目录载荷、新源generation及peer/controller恢复；桥只是机制桥，不是产品网络；Wayland不在范围。
- 原生 PipeWire 消费者扩展已主验收：首次实际运行在 `native_consumer_process_identity_mismatch` 处失败且清理成功；私有最小实验确认 Node 的 `client.id` 指向 Client，PID 位于 Client 而非 Node。按真实结构先补红测试，再改为同一 registry 快照内 Node→Client→自有 PID 核验；缺失/歧义/外来 Client 均拒绝。日志 `.release-tools/linux-probes/audio-native-identity.log` 仅含数值身份。修复后全音频93项离线测试通过，完整 route 原生回归退出0。
- 最新 `.release-tools/linux-probes/audio-route-current.log`：原有静音/恢复、双信号隔离及全部消费者用例均回归通过；新增 pw-cat 注册无 link 不计需求、两条 active target link 后恰计一次、本地禁用不开采、启用后独立发送、最后退出真实 reap。最终 starts=stops=8、无模块/流/线程残留、默认设备不变。当前环境 pw-cat 存在 Pulse 镜像，明确记录 `native_only_evidence=not_exercised_pulse_mirrored`；未镜像消费者仅有离线覆盖，不冒充真实 native-only 实证。
- 新增 `linux_audio_restart_probe.py` / `test_linux_audio_restart_probe.py`，复用运行器创建第二套无硬件音频服务而非重启外层服务。命令 `python3 -B build-support/linux-native-test.py --timeout 120 -- python3 -B build-support/linux_audio_restart_probe.py` 退出0，日志 `.release-tools/linux-probes/audio-restart-current.log`。两代相同私有 socket/runtime、cookie 确实变化；第一代断连后真实 pw-dump 失败触发 unknown/停采/reap，旧身份与许可被拒绝，第二代只在新消费者证据下重新启采；两代 consumer/sender 均实测 RMS5792.56，每代2个服务/3个流进程完全回收，外层默认设备/epoch 不变，最终运行器服务/清理/挂载核查均通过。未放宽 `Session.require_alive`，仅使自有音频配置目录支持第二代复用。
- 慢 selection 控制耗时现有数值证据：`linux_dnd_scenarios.py` 新增严格单调时钟测量与4项单测，返回迟于500ms即失败，并确认 slow reply 尚未结束。500ms是探针控制预算；对照 `oink-routing/src/router/dnd/target.rs:67` 的3秒 preparation 与 `oink-agent/src/threads/inject.rs:75` 的5秒 ordinary-input 过期，不能称二者是产品固定 fence ACK 时限。当前8场景完整运行5次控制ACK为0.449–0.673ms，日志明确 `scope=test_role_control_not_product_fence`；产品 fence/网络端到端仍待后续产品集成验收。
- 真实宿主能力仅做只读 `DISPLAY=:0 xinput list --long`，未注入、抓取按键事件或改设备。日志 `.release-tools/linux-probes/host-device-capabilities.log`：当前设备暴露水平/垂直 XIScrollClass、increment120；未枚举到 XITouchClass/XIGestureClass。这仅说明当前连接设备的公开能力，不说明 Linux/Xorg 无触点能力，也不是物理滚动/手势/触点注入验收。
- 最终本轮门禁：`systemd-run --user --scope --quiet -p MemoryMax=4G -p CPUQuota=200% -p TasksMax=256 timeout 120 python3 -B -m unittest discover -s build-support -p 'test_linux*.py' -v` 实跑187项通过，exit0，日志 `.release-tools/linux-probes/linux-unit-current.log`。6个C产物（input/dnd/dnd-roles/input-matrix/input-matrix-test/fuse）从当前源码以 `-std=c11 -O2 -Wall -Wextra -Werror` 重新构建，exit0/无告警，C纯校验器通过，日志 `native-build-current.log`。`bash -n` 与基础依赖检查通过；toolkit检查仍缺 `qtbase5-dev`，未安装或绕过。
- 重新构建后串行回归：input、原GTK Drop/Escape、26项core+XI2输入矩阵、原audio，以及 `linux-native-test.py --peer-x11 --timeout 150 -- python3 -B build-support/linux_dnd_scenarios.py --roles /home/wgh/dev/cross_platform_app/oink/.release-tools/linux-probes/dnd-roles --suite all` 均退出0；后者8场景全部通过，日志 `dnd-all-current.log`。加上新route/restart共7份最新原生日志，逐份机器校验唯一 `native_test_summary`、probe_exit/result/remaining_mounts均0、services_ok/cleanup_ok/mount_check_ok均true。日志均位于 `.release-tools/linux-probes/`，无真实输入文本或音频正文。
- 本轮收口边界：`git diff --check` 通过，26个修改/新增脚本及C/头文件均≤500行；仅build-support与本计划有仓库变更，无产品/锁文件/CI/系统权限修改，未commit/push/tag/发布。约定FUSE用户日志仍无可读取证据，不重试代理挂载。阶段2仍未通过：Qt依赖及Qt/Nemo/浏览器完整源目标矩阵、文件/目录与大selection、新源generation/重连、真实GTK/Qt/浏览器专属master交付、物理HID/触摸板/触点/麦克风/锁屏和FUSE普通终端验收均未完成；这些包含未实现的软件工作，不全是用户操作阻塞。阶段3–8产品适配、安装包和Windows互通尚未开始。

## 第三轮续作记录（2026-11-17，基线 6b9d3bd）

- 接手只读复核：`main` / `6b9d3bd`，工作树干净；第二轮探针与交接已提交，旧记录中的“未提交”保留为历史快照。Rust 1.99.0、仓库 Node 24.14.1 可用；Nemo/Chrome/Firefox 命令存在，本轮尚未启动应用或运行原生探针。约定 `.release-tools/linux-probes/fuse-user.log` 不存在，只能说明暂无可读取证据，不据此认定用户未操作，不重试代理 FUSE 挂载。
- 用户批准继续阶段2波次 A–F；按 A→B隔离夹具→C文件/INCR→B完整矩阵→D恢复→E输入→F用户门槛推进。真实应用不具备测试源私有 generation 属性，拟以 XRes 服务器侧客户端身份结合进程启动身份/epoch 做独立来源校验，保留现有夹具强断言。`libxres1` 已安装，开发包缺失；本步只准备依赖门槛，尚未实现来源校验。
- 波次A前置：`build-support/linux-deps.sh` 的 `--toolkit-tests` 增加 `libxres-dev` 包与 `xres` pkg-config 检查，帮助文字同步；默认仍只读，不要求基础检查安装 toolkit。`test_linux_deps.py` 增加缺包、缺模块、仅安装缺失开发包的3项用例，强化 toolkit 可选性。安装路径仅由隔离 PATH 中的 sudo/apt-get 测试桩执行，未运行系统安装。
- 先补测试再改脚本：限额命令 `systemd-run --user --scope --quiet -p MemoryMax=4G -p CPUQuota=200% -p TasksMax=256 timeout 120 python3 -B -m unittest discover -s build-support -p 'test_linux_deps.py' -v`，改脚本前 exit1（缺少 XRes 包/模块检查导致7个断言失败，含子用例），日志 `.release-tools/linux-probes/deps-xres-red.log`；改后21项通过、exit0，日志 `deps-xres-green.log`。相同限额执行 `python3 -B -m unittest discover -s build-support -p 'test_linux*.py' -v`，190项全部通过、exit0，日志 `linux-unit-phase2-wave-a.log`；两份通过日志无告警。
- `bash -n build-support/linux-deps.sh` exit0；`bash build-support/linux-deps.sh --check --native-tests` exit0（`deps-base-wave-a.log`）；加 `--toolkit-tests` exit1，仅报 `qtbase5-dev`、`libxres-dev` 两包缺失（`deps-toolkit-wave-a.log`）。以上日志位于 `.release-tools/linux-probes/`，未覆盖前两轮证据。用户下一步在普通终端执行 `bash /home/wgh/dev/cross_platform_app/oink/build-support/linux-deps.sh --install --native-tests --toolkit-tests`，完成后代理只读复查。
- 当前仅依赖前置及其测试完成；Qt夹具/真实应用/文件目录/INCR/恢复/真实toolkit输入仍未实现，FUSE与物理硬件证据仍缺，阶段2不通过。未改产品、锁文件、CI、系统权限；未commit/push/tag/发布。依赖齐备后继续波次A的当前源码重建、基线原生回归及Qt夹具，不能以本步Python测试通过冒充原生能力通过。
- 用户决定不持续依赖 snow-apps 的状态：将通用 Qt 6.12.0 gcc_64 kit 复制到本项目忽略目录 `.tools/Qt/6.12.0/gcc_64`（1.7 GiB / 11037 文件），并从 Ubuntu `libxres-dev 2:1.2.1-1build1` 解包出 `.tools/sysroot`。qmake 报本地前缀；最小 Qt Widgets 程序以 offscreen 启动 exit0；qmake 产物 RUNPATH 与 `QT_DEBUG_PLUGINS` 均指向 oink 本地 kit；QtCore/插件未嵌入 snow-apps 文本路径；ICU 73 随 kit 自带；glibc 符号上限 2.34，低于宿主 2.39；XRes 头/库版本与系统 `libxres1 2:1.2.1-1build1` 一致，编译链接通过。
- 依赖门禁改为 `--toolkit-tests` 优先识别本地 Qt6 kit（qmake、Qt6WidgetsConfig.cmake、libqxcb.so、Qt6 版本）与本地 XRes sysroot（XRes.h、xres.pc、libXRes.so），缺失才回退系统 `qtbase5-dev`/`libxres-dev`；新增 Qt xcb 运行库包和 `libxres1` 显式检查，本地路径存在但不完整时失败而不静默回退。`OINK_QT_KIT`/`OINK_XRES_SYSROOT` 可覆盖路径，测试用隔离 PATH 与假 kit/sysroot 验证。
- 兼容性结论：本机系统另有 Qt6 6.4.2 运行库，和本地 6.12.0 同 SONAME；探针二进制 RUNPATH 指向本地 kit，运行器白名单环境不继承 `LD_LIBRARY_PATH`/`QT_PLUGIN_PATH`，因此不会误载系统 6.4.2；产品 Tauri/GTK 构建未加入 Qt 路径，不产生链接冲突。Qt 仅用于 build-support 探针，不进入产品；Qt 官方开源构建与仓库 GPL-3.0-only 兼容。
- 验证：`bash -n`、`git diff --check` 通过；依赖单测 27 项通过；真实 `--check --native-tests --toolkit-tests` exit0，`deps-toolkit-local.log` 显示本地 qt-kit 6.12.0 / xres-sysroot 1.2.1；完整 Linux Python 套件 196 项通过，日志 `linux-unit-phase2-wave-a2.log`。`.tools` 已由 `.gitignore` 排除，`linux-deps.sh` 229 行、`test_linux_deps.py` 443 行，均低于 500 行硬上限。
- 尚未完成：`.tools` 不随仓库分发，fresh clone/CI 仍需独立的固定版本获取脚本，本轮不把拷贝目录当作可复现交付；Qt 原生探针尚未实现，阶段2仍未通过。

### 波次 A 原生实现与回归（2026-11-17）

- 在既有限额内严格重建 input、dnd、dnd-roles、input-matrix、input-matrix-test、fuse 六个产物并记录 mtime/SHA-256；input-matrix 首次链接缺 `-lXau`，失败日志保留，补齐链接参数后零告警构建通过，未改输入代码。`verification-wave-a3.log`、`run-status-wave-a3.log` 记录六产物新鲜度、纯 validator exit0，以及串行 input/dnd/input-matrix/audio/audio-route/audio-restart/dnd-all 七份唯一成功汇总；包含26项输入、两代音频恢复、原8个XDND场景。FUSE仅编译，未代理执行挂载。日志均在 `.release-tools/linux-probes/`。
- 新增 Qt6 Widgets/QDrag 独立源/目标、非阻塞控制与构建/场景脚本；共享 `linux_dnd_wire.h` 和 `linux_dnd_native.h`，将GTK特有抓取检查移到 toolkit hooks，保留原强断言。Qt构建以 `pkg-config --define-variable=prefix=<本地kit>` 纠正官方pc文件的未重定位前缀，只对Qt查询生效；运行时读取实际进程maps，要求Qt6库及xcb插件均在本地kit目录，不接受目录前缀碰撞。未将Qt加入产品。
- 首次Qt COPY失败：`qt-matrix-wave-a-first.log`/`qt-matrix-wave-a-diagnostic.log` 报selection请求超时；`qt-selection-state.log`证明QDrag::exec已返回，但真实UTF8_STRING请求随后才到达。修复为保留Qt管理的异步QDrag事务，以真实XdndFinished及已协商目标/COPY/selection读取证据确认End；不再把exec返回当作原生事务结束。`qt-build-lifecycle.log`零告警通过，`qt-matrix-lifecycle.log`的Qt→Qt、Qt→GTK、GTK→Qt各Drop/Escape以及外来owner/旧generation两个拒绝场景，共8场景实测通过；单次Down/Begin/End、完整SHA-256、源删除0、CLIPBOARD哨兵不变、结束无抓取及正常QUIT/exit0/零stderr均检查。私有XWD转出的 `qt-fixture.png` 已查看，仅包含隔离夹具。
- 追加重复Down拒绝和7个真实Qt坏控制帧用例；纯测试206项通过（`linux-unit-qt-wave-a-final.log`），bash语法和diff空白门禁通过。插件目录碰撞先有红测（`qt-plugin-red.log`），再使用真实路径归属检查修复。
- 回归阻断记录：提取后旧GTK完整回归 `dnd-all-after-qt.log` 在target被主动终止后的Escape清理报 `GTK released grab`，exit1；此前四场景通过，双私有会话清理/服务/挂载检查通过。后接的最终Qt验收未启动。目标退出单场景旧版/当前版各3次均通过，完整序列当前版第二轮在双端Escape再次复现（`gtk-all-dnd-roles-qt-wave-2.log`），不能以重跑绿灯忽略失败。
- GTK根因实证：保留原失败断言的临时诊断 `gtk-grab-diagnostic-4.log` 记录 `end=1 button=1 server_grab=0 cache_before=1 cache_sync=1 cache_dispatch=0 rounds=3`；server_grab=0是独立XIGrab主动检查成功并立即释放，不是只读查询。对应GTK 3.24.41实现中，ungrab请求后还须异步roundtrip及idle回调删除GDK grab记录，`drag-end`与`gdk_display_sync`均不是其完成屏障。故增加报告字段 `toolkit_grabbed`，在End后至多等待500ms的真实toolkit状态归零，再执行原GTK断言及pointer/keyboard原生抓取检查；不发ButtonUp促成释放，不主动释放GTK grab，不嵌套泵事件。临时诊断已移除。4项离线时序回归先红（`gtk-cleanup-red.log`），涵盖延迟状态、永久占用、缺证据和原生抓取失败；聚焦DnD套件随后通过（`gtk-cleanup-green.log`）。最终原生验收结果见下条。
- 最终验收：`gtk-build-cleanup-final.log`、`qt-build-cleanup-final.log`严格零告警重建，产物mtime晚于全部输入源码；`gtk-cleanup-final-1.log`至`gtk-cleanup-final-5.log`连续五轮各8个旧GTK场景通过（40次），随后 `qt-matrix-wave-a-final.log`的8个Qt混合场景及7个坏控制帧全部通过。六份日志均逐一核验唯一汇总、`probe_exit=result=remaining_mounts=0`及services/cleanup/mount检查true；Qt九组正常退出记录（含截图组）均exit0且无stderr。最终私有截图 `qt-fixture-final.png` 已实际查看。`linux-unit-wave-a-verified.log`完整210项通过、`deps-wave-a-verified.log`本地依赖检查通过、bash语法/diff空白/所有变更源码≤500行通过；独立静态复核未发现时序修复阻断项。
- 本机Qt原生夹具验收完成；新增报告字段后旧二进制与新控制器不可混用，续作须按当前源码重建。可复现依赖获取脚本仍缺，真实应用矩阵/文件目录/INCR/新代恢复/专属master的真实toolkit输入尚未实现；FUSE约定日志仍不存在，物理硬件与真实麦克风仍缺证据。阶段2仍不通过，不进入阶段3–8，未提交/推送/tag/发布。

### 第三轮提交与任务移交（2026-11-17）

- 用户随后明确要求提交、推送到 GitHub 并编写供其他 AI 代理续作的交接文档。代码已提交为 `2d2061a7c01c13e07e43afb15f9dd62ca808c7cd`（`test(linux): validate Qt X11 drag fixtures and asynchronous grab cleanup`），成功推送 `origin/main`；独立 `git ls-remote origin refs/heads/main` 核验同一 SHA，代码推送后工作树干净。上文“未提交”保留为当时快照。
- 新增 [第三轮自包含交接](../handover/2026-11-17-linux-x11-phase2-qt-continuation.md)，同步交接索引；完整保留 A2/B/C/D/E/F 范围、逐项验收、真实代码缺口、失败/成功证据、限额重建与串行复跑命令。明确旧 Qt5 安装建议已过时、旧报告二进制不可复用、input-matrix 需链接 Xau、依赖和日志仅本机忽略目录存在。
- 再次核实 `apps/`、`crates/` 自 `d8b2932` 至代码提交无改动；Linux UI 仍拒绝启动，输入/显示/剪贴板仍 Unsupported，音频仍占位。当前不是“产品差一步”，阶段2仍不通过，阶段3–8尚未开始。
- 本次文档独立提交交付，不改版本、不打 tag、不发布、不改 CI 或系统权限；用户此次提交/推送授权不延伸为后续代理的持续授权。交接前停止实施 B–F，留给下一代理按批准顺序推进。
