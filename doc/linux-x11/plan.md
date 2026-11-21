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

## 第四轮续作记录（2026-11-17，基线 19bc294）

- 接手核实 `main` / `19bc294`、工作树干净，Rust 1.99.0、仓库 Node 24.14.1；`bash -n build-support/linux-deps.sh`、`bash build-support/linux-deps.sh --check --native-tests --toolkit-tests`、`git diff --check` 均 exit0，Qt 6.12.0 / XRes 1.2.1 本地依赖通过。约定 `fuse-user.log` 不存在，仅表示无可读取证据，不推断用户未操作；不代理挂载。
- 用户批准按新鲜基线→A2→B隔离/文本→C→B完整矩阵→D→E→F续作；仍不提交、推送、发布、不改产品/工具链锁/CI/系统权限。新批次 `.release-tools/linux-probes/continuation-20261117-a/` 保留构建、测试、SHA-256、mtime和每步命令/exit，入口为该目录 `verify_baseline.py`；构建/测试保持4GiB/CPU200%/Tasks256，原生串行。此条记录时任务正在执行，尚无本轮原生通过结论；A2固定来源核实同步进行，B–F尚未实施。
- 新鲜基线首轮：七产物严格编译、原210项离线测试通过；input在原replay断言失败，`continuation-20261117-a/input.log`保留红证据，后续原生未启动。XRes焦点诊断证明原override-redirect窗口在首批输入后失焦给Metacity（`input-focus-diagnostic.log`）；改为WM管理窗口并有界等待真实映射，不减弱重放/吞键/专属master断言。新增4项离线映射测试先全部失败（`input-window-red.log`），修复后全过（`input-window-green.log`）；正式严格构建后的`input-managed-final.log`原机制全部通过且唯一清理汇总成功。完整基线在新批次`continuation-20261117-b`重跑，不能用单项绿灯替代。
- A2新增固定来源清单、安全libarchive逐entry提取及获取入口，Qt官方qtbase/ICU与Ubuntu匹配XRes开发/运行包固定SHA-256及大小；下载仅HTTPS有界curl，deb只读data，不运行安装脚本，目标树内XRes链接，组件自有stage原子发布、完整清单复核、并发锁、已有目录不覆盖。最初依赖测试的entries同名冲突是测试辅助函数问题（`deps-archive-tests.log`）；修正参数名后资源上限真实被测。新增生命周期测试揭示非object清单未经验证导致AttributeError（`deps-lifecycle-red.log`），补类型检查后50项依赖测试通过（`deps-lifecycle-green.log`），含并发子进程、篡改、中断与重跑。未放宽任何原用例。
- A2正式入口在全新`.tools/fresh-20261117`从固定来源下载并完成两组件安装（`continuation-20261117-b/deps-fresh-install.log`，exit0）；没有复用旧拷贝或修改旧kit/sysroot。
- 完整基线批次`continuation-20261117-c`：七产物严格零告警构建、新鲜度/SHA-256记录、纯矩阵校验与串行 input/dnd/input-matrix/audio/audio-route/audio-restart/dnd-all(双会话)/qt-matrix 全部exit0，每份唯一`native_test_summary`的probe_exit/result/remaining_mounts=0及services/cleanup/mount检查true；完整离线套件240项通过（`continuation-20261117-b/linux-unit-full.log`）。input在修复后批次C一次通过，且修复后二进制另经私有环境连续7轮稳定通过（`focus-stability-1..7.log`）。
- input根因补充：managed窗口后仍有WM聚焦竞态，`focus-reproduction-2.log`证明Metacity把焦点留在自有frame窗口（XRes PID归属metacity），replay正确执行但目标0事件。正式修复在映射就绪后新增焦点就绪阶段：先轮询等待WM焦点值稳定（有界、事件驱动、失败关闭），再显式`XSetInputFocus`并要求服务器确认焦点落在目标窗口；不放宽任何重放/吞键/专属master断言。3项新离线回归先用旧行为桩取得红灯（`input-focus-red.log`），实现恢复后7项全过（`input-focus-green.log`）。
- A2新树原生验收：新kit qmake前缀自重定位报告6.12.0且`QT_INSTALL_PREFIX`指向新树；`OINK_QT_KIT`/`OINK_XRES_SYSROOT`覆盖下`linux-deps.sh --check --native-tests --toolkit-tests` exit0（`deps-fresh-check.log`）；`ldd`证实libXRes.so.1与libQt6*均解析到新树内部；XRes诊断探针在私有会话内以新树XRes真实返回client PID。新kit构建Qt角色后Qt/GTK矩阵8场景（qt↔qt、qt→gtk、gtk→qt各Drop/Escape及拒绝旧代/外来owner）全部PASS、工具进程exit0（`fresh-qt-build.log`、`fresh-qt-matrix.log`，唯一汇总true）。正式入口对新树`--check-only`复核exit0（`deps-fresh-recheck.log`）。**A2记为PASS**。
- 本轮收尾：临时诊断源（input-replay-diagnostic.c、input-focus-trace.c及实现备份patch）已清理，诊断二进制、红/绿日志与两批次status.jsonl保留在忽略目录；`git diff --check` exit0，变更/新增源码均≤500行（input探针342行、archive 222、fetch 250及三个测试文件164–248行），未改产品/锁文件/CI/系统权限，未提交/推送/tag/发布。
- 阶段2剩余（未开始）：B真实应用隔离/身份/文本矩阵、C文件与INCR及B完整矩阵、D恢复、E真实toolkit输入、F用户硬件/FUSE收口；FUSE约定日志仍不存在，物理硬件与真实麦克风仍无证据。阶段2整体仍不通过，不进入产品阶段3–8。

### B波续作记录（2026-11-17，续 19bc294）

- B波范围：私有 runner 内真实应用（Firefox、nemo 文件管理器）作为 XDND 一端，验证进程隔离、XRes+epoch+generation 应用身份分支与文本载荷，不弱化任何 fixture 私有检查。浏览器沙箱未关闭（Chrome 的 `--no-zygote` 要求 `--no-sandbox`，被禁），Chrome 行暂以 BLOCKED 记，待用户就 TasksMax=256 冲突决策。
- 资源墙定位与化解（限额内）：Firefox 套件 GLib `creating thread` EAGAIN 连击，实测 scope 线程稳态210–217、启动尖峰253，顶到 TasksMax=256。削减需求而非提高限额：`NO_AT_BRIDGE=1` 禁 at-spi；dbus-daemon 空激活目录阻断 portal 生成；Firefox user.js 关闭预分配进程（真实 pref 名自 omni.ja 提取：`dom.ipc.processPrelaunch.fission.number`/`.lowmem`）。新增 `wait_task_headroom()` 有界线程余量门（`test_linux_app_wave.py` 3 项离线回归）。未动限额、未不限额重试。
- XRes 三重根因（C 与 python 两侧同病）：(1) 必须先 `XResQueryVersion` 协商，否则客户端库静默拒绝查询；(2) Xlib Status（1=成功）与 X11 `Success==0` 语义相反，原 `status == Success` 恒假；(3) Xephyr 对 `XResQueryClientIds` 返回 Status 0 但结果完整填充，须以"输出已填充"为成功判据、失败关闭。修复后私有会话内 C 与 ctypes 两侧均真实返回 client PID（`app-diag-xres*.log` 证据链）。
- 应用源身份：`XdndSelection` owner 可能是 override-redirect 代理窗口、不在应用窗口树内，故身份以 XRes 服务器端 client PID ∈ 应用进程树 + 根窗口 session epoch 原子 + 控制器写 generation 原子三重校验（`app_identity` 独立分支，不弱化 `selection_identity`）。wire 协议新增 `ARM_APP=18` 与报告 `app` 字段；`Role.command` 对 ARM_APP 同步代际跟踪。
- Firefox 目标方向空数据根因：角色源只提供 UTF8_STRING，Firefox 请求 `text/plain` 被拒（python `XConvertSelection` 对拍实证）。`linux_dnd_roles.c` targets 增 `text/plain`，`selection_get` 按请求 target 回填。修复后 48 字节随机载荷经页面 `received_sha256` 与角色 digest 逐字节比对通过。
- Xephyr XDND 落点不稳定根因：快速 8 步传送路径后 release，Firefox  sometimes 完全不处理 drop（页面 dragenter/dragover 仪器证实无事件，截图证实指针与拖放图标在 #dst 内）。释放前加小幅真实 motion 对（ settle ）后 4/4 稳定通过；机制未知但不绕过任何断言，真实用户路径本含连续 motion。nemo 目标行同样修复。
- nemo 根因：私有 dbus 无激活目录时 gvfsd 不可激活，nemo 主循环空转永不建窗（strace 实证非阻塞、gvfsd socket 连接失败）。runner `start_dbus` 改为仅放行 `org.gtk.vfs.Daemon`/`org.gtk.vfs.Metadata` 两个激活项（卷监视器仍阻断，portal 仍不可生成），新增离线回归 `test_dbus_activation_exposes_only_gvfs_core`。nemo 窗类实为 `Nemo`（非 `nemo`），修正匹配。
- B波实测：Firefox 三行（source-text-drop、target-text-drop、source-reject-forged-pid）`app-firefox-30.log` 全 PASS；nemo 两行（startup-identity、target-text-contract）`app-nemo-2.log` 全 PASS（contract 行为 selection_requests=2 且树变化，文本落为文件）。
- browser_source_text 身份接收循环偶发超时根因（`app-firefox-29/32.log` 红证据保留）：`begin_app_drag` 为让页面收到按下而把全屏应用窗口 raise 到最上，目标角色窗口被压到其下，应用源的 XdndEnter 永不到达（visits 恒 0）；接受循环前显式 `XRaiseWindow` 目标角色并留稳定时间后 `app-firefox-33/34.log` 连续两轮全行 PASS。接受循环另加 settle motion 对与 15s 有界预算（Xephyr XdndPosition 突发丢失同源）。
- 经用户批准（2026-11-17），runner TasksMax 有界 256→1024（内存 4GiB/CPU200% 不变、仍串行、仍私有 Xephyr；理由：真实 Chromium 线程需求远超 256，且其诊断要求关闭沙箱被安全约束禁止，无法减重）。`require_resource_limits`、systemd-run 参数、headroom 门 TASKS_MAX 及两处离线边界测试同步更新，39 项离线测试通过。
- Chrome 四行实测（`app-chrome-1/2/3.log`）：target-text-drop PASS（GTK 角色源→Chrome 页面，48 字节载荷 verbatim，Chromium 请求 selection 两次，断言改 1..4 有界）；三行 source 方向（text-drop/reject-forged-pid/reject-late-generation）BLOCKED——Chromium 无视 XTEST 拖拽起始：trusted press+60px motion 到达页面（probe-mousedown/mousemove 仪器证实）但无 dragstart、无 XdndSelection owner（`app-diag-chrome*.log` 已清理、结论入档）。以带绊线的 `browser_source_synthetic_blocked` 记录：若未来 Chromium 放行合成拖拽起始，该行进 FAIL 强制升级为真实源测试。物理鼠标 counterpart 属 F 波用户手测范围。Chromium 作目标接受外部 XDND 正常。
- **B波结论（任务#5）**：Firefox 三行、nemo 两行、Chrome 一行实测 PASS，Chrome 三行 BLOCKED（平台限制，已登记）；红证据（`app-firefox-29/32.log`）保留。临时诊断脚本/二进制已清理，日志与构建产物保留。未提交/推送/tag/发布。
- 协议变更后原生回归：`dnd-all-after-appwave.log` 8 场景全 PASS（双会话，唯一汇总 probe_exit/result/remaining_mounts=0、清理/服务/挂载全真）；`qt-matrix-after-appwave.log` 首次 FAIL 属预期——Qt 角色按第三轮警告须按当前源码重建（旧 wire 协议不兼容），`linux_dnd_qt_build.py` 以新 kit 重建 `dnd-qt-current` 后 `qt-matrix-after-appwave-2.log` 8 场景及 7 坏控制帧全 PASS、工具进程 exit0 零 stderr。完整离线套件 discover 全部通过（skipped=5 为 Sentry 凭据相关）。
- 阶段2剩余：C文件与INCR及B完整矩阵（含 Chrome 决策）、D恢复、E真实toolkit输入、F用户硬件/FUSE收口。阶段2整体仍不通过，不进入产品阶段3–8。

### C波续作记录（2026-11-17，续 19bc294，INCR/流通道层）

- 协议扩展：`linux_dnd_wire.h` PAYLOAD_MAX 128→8MiB，新增 INCR_THRESHOLD(256K)/INCR_CHUNK_MAX(64K)/STREAM_CHUNK_MAX(64K)/STREAM_QUEUE_MAX 与 9 个报告计数字段（incr_handshake/chunks/bytes/done/sent/sent_bytes/abandoned、bridge_chunks/bytes/bad），Command 增 FETCH_STREAM/LOAD_STREAM；`linux_dnd_protocol.py` FIELDS/枚举/PAYLOAD_MAX 同步，新增 STREAM_HEADER(44 字节 LE)/MAGIC/TERMINATE/MIME 常量。
- 新增 `linux_dnd_incr.c/.h`（发送状态机+被动接收观测，185/12 行）与 `linux_dnd_stream.c/.h`（socketpair 继承 FD 二进制通道，186/16 行）。角色集成：command() 控制数组缩至 LINE_MAXIMUM/2+4（PAYLOAD_MAX 增大后原 `values[PAYLOAD_MAX+4]` 在 GTK 与 Qt 两侧均为 64MB 栈数组，Qt 首个命令即 SIGSEGV，gdb 实证 QSocketNotifier 栈内溢出）；LOAD 内联 ≤128 字节；DELAY 限 ≤INCR_THRESHOLD；selection_get 拒大体积（INCR filter 独占）；tick 挂 incr_poll；main 调 incr_init/stream_init；source 角色 48 字节自动体可被流载荷在 begin 前替换（proxy 不许覆盖已载体）。
- INCR 发送方根因修复（1MiB 实证恒复现）：末块与零长终止块同发时后者覆盖前者，GTK 读到空属性提前收尾、恒丢末块。改为末块发出后挂 `final_sent`，等接收方 delete 末块再发零长块。GTK 接收经 per-screen IPC widget 窗口取数（非目标 widget 窗口），观测按"pending 期间首个 INCR 类型 NewValue"动态锁窗/atom，同块 NewValue 有重复须 Delete 沿后才计新块（1MiB+123 实证 handshake=1/chunks=21/bytes 精确/done=1）。
- python 侧：Role 构造建 socketpair 经 OINK_STREAM_FD+pass_fds 传入；`send_wire` 原语；`bridge_payload`（catcher FETCH_STREAM→校验帧头（magic/epoch/generation/offset 连续/块界/TERMINATE 仅末块）→proxy LOAD_STREAM，收代理延迟 R）；`stream_feed`（LOAD_STREAM 推任意载荷入 source/proxy）；LOAD_STREAM 返回 STREAM_DEFERRED 哨兵时 command() 跳过同步报告。
- 场景 `linux_dnd_incr_scenarios.py`（157 行）四行全 PASS：`incr-source-catcher-1048699`（源→catcher，INCR 收/发双侧证据+digest 比对+escape 清理）、`incr-source-catcher-bridge-327687`（源→catcher INCR→FD 桥→proxy 载入 digest 一致）、`incr-proxy-target-1048699`（proxy→target INCR drop）、`stream-load-rejects-oversize-and-stale`（超限 total 拒 2、旧代拒 1）。控制 ACK 预算行 118–650µs（预算 500ms）。
- 堆叠坑登记：metacity 下 XRaiseWindow 压不动堆叠（实证 raise 后源窗仍在下）；proxy 与 source 同槽位（100,220），按压类场景改为"proxy 推迟到源 drag 结束后再建"（桥接无需窗口交互）规避，不依赖 WM raise。
- 回归：GTK `dnd-all`（双会话，13 个 PASS 行/轮）+ INCR 套件连续五轮全绿；Qt 角色按新 wire.h 重建（顺带修 64MB 栈数组）后 qt-matrix 8 场景（qt↔qt、qt→gtk、gtk→qt 各 Drop/Escape + 拒绝旧代/外来 owner）全 PASS。离线 discover 248 项全过（env 断言放行 OINK_STREAM_FD、opcode 数动态化、新文件纳入 500 行门禁）。
- **C波 INCR/流通道层结论**：≥1MiB 真实 selection 双方向（源→catcher、proxy→target）取得握手/块数/字节/offset/终止完整证据；FD 二进制桥接双向实证；负面用例（超限/旧代）收口。临时诊断脚本/二进制已清理。
- C波剩余（未开始）：文件 URI staging（授权根实读、peer staging、offset/遍历、空文件/嵌套目录/Unicode/路径逃逸/符号链接）与文件矩阵（nemo/Firefox/Chrome × 文件/目录）、取消/owner死亡/暂存失败等负面层。阶段2整体仍不通过，不进入产品阶段3–8。

### C波续作记录（2026-11-20，续上节，文件 staging 层）

- 协议扩展：`linux_dnd_wire.h` FIELDS 增 file_requests/file_reads/file_entries/file_bytes/file_bad/staged/published 七项，Command 增 FILE_READ/FILE_STAGE/FILE_VERIFY（24 个）；MIME_UTF8/TEXT/URI_LIST/OCTET 常量入 wire.h；`linux_dnd_protocol.py` 同步 FIELDS/枚举，`bridge_payload` 放行 mime≤OCTET 并原值中继，`stream_feed` 增 mime 参数。
- 新增 `linux_dnd_file.c/.h`（275/16 行）：FILE_READ（source 按授权根 $OINK_NATIVE_TEST_ROOT/files 实读，O_NOFOLLOW+realpath 前缀包含防逃逸，pread 循环 sha256+size 稳定性双读校验，>PAYLOAD_MAX 软拒 2）；FILE_STAGE（proxy 独有：staging/tmp→fsync→盘上重哈希对 digest→一次原子 rename 发布到 published/ 子树，失败清理，发布后 payload_clear 供下一文件）；FILE_VERIFY（catcher/target offset pread 重哈希或确定性排序目录遍历，符号链接列 L 不跟随）。路径字节经控制行数字传输（≤255，禁反斜杠/空/dot 组件/控制字节，角色启动自建 0700 files 根）。
- 根因修复三处：①staging fd O_WRONLY 上 pread 重哈希必 EBADF（改 O_RDWR）；②file_read 变更校验误拿文件 digest 对过期报告字段（改为对实读 body 重哈希）；③角色启动 digest 字段未初始化（struct 零值≠空摘要，main 补 payload_report，payload_report 去掉空 body 特判恒为真 sha256）。
- 语义约束实证：proxy 不覆盖已载体（stream_load `body_length && is_proxy→2`），故桥接载入的 blob 直接 stage，后续文件须等 stage 消费后重喂；FILE_STAGE 只发布 published/ 子树（resolve_new 对 published/<rel>）。
- 场景 `linux_dnd_file_scenarios.py`（约 360 行）全 PASS：`file-text-drag-same-server`（Unicode 文本文件源实读→真实 drag→catcher 实读+FD 流字节比对→回源→target drop，get=2 精确）、`file-stage-dual-server`（300KB+ blob 源实读→INCR→catcher FETCH_STREAM→桥→proxy 载入→stage→peer 发布→target 校验 digest/磁盘字节独立比对+嵌套目录发布→双角色遍历清单字节/digest 交叉一致）、`file-empty-roundtrip`（零字节读写+空摘要；限制登记：零字节负载无法发起真实 drag，不走 XDND 正文通路）；拒绝面 11 例全过（逃逸/dot组件/反斜杠/缺失=角色硬拒退出，目录/超限/角色错配=软拒 status 2 存活，发布目标冲突=硬拒，peer 符号链接出根 verify=硬拒）。
- 回归：GTK scenarios+INCR+file 三套件连续五轮全绿（15/15  summary result=0）；Qt 角色按新 wire.h 重建后 qt-matrix 8 场景全 PASS；离线 discover 248 项全过。
- **C波文件 staging 层结论**：授权根实读→XDND 正文唯一通路→peer 独立 staging→发布→真实打开/offset/遍历全链路证据闭环；源零删除、CLIPBOARD 哨兵不变、拒绝面完整。C波剩余：文件应用矩阵（nemo/Firefox/Chrome × 文件/目录，Chromium 源方向预期 BLOCKED 带绊线）与取消/owner死亡在文件层的负面变体。阶段2整体仍不通过，不进入产品阶段3–8。

### C波续作记录（2026-11-20，续上节，文件应用矩阵）

- 新增 `linux_dnd_app_file_scenarios.py`（约 300 行）：GTK 角色源/目标 × 真实应用（nemo/Firefox/Chrome）文件与目录行。页面夹具 `linux_dnd_app_page.html` drop 报告扩展 `file_digests`（File 对象 name+size+正文 sha256）；`linux_app_x11.py` 增 `move_window`（XMoveWindow+XSync，纯新增不改既有行为）。
- nemo 目标两行 PASS（nemo-target-file-drop / nemo-target-directory-drop）：uri-list 经授权根 FILE_READ 实读入源体→真实 drag→nemo 真请求 selection 2–3 次→nemo 真实复制文件/目录到下落目录，落盘字节与 PAYLOAD 逐字节比对一致。
- nemo 源行（nemo-source-file-drop）根因三连：①图标坐标盲猜（窗口+60,+130）从未命中——`xwd -root` 私有工件裁剪截图实证首个图标中心在窗口 (236,128)，改坐标后 nemo 真发起拖拽（override-redirect 拖拽图标窗成 XdndSelection owner，XRes PID 认证通过）；②应用源 press 时自 raise 全屏窗，metacity 下 XRaiseWindow 压不回堆叠，catcher 槽位 (615,330) 全被 nemo 窗 (239,107,800x550) 覆盖，XdndEnter 永不到达——`move_window` 把 catcher 移到 (1090,600) 屏幕空区（角色 r.x/r.y 每次 report 经 gdk_window_get_origin 重算，无 stale）后通过；③nemo 对 UTF8_STRING selection 请求回**裸绝对路径**而非 file:// URI（其真实原生源格式），断言改为"body 含真实文件路径"并登记格式限制；FILE_VERIFY 对实文件 offset 重哈希 digest 精确比对通过。
- 浏览器目标两行 PASS（firefox/chrome-target-file-drop）：GTK 源 file:// uri-list→浏览器把 URI 转为真实 File 对象交付页面（types=['Files'] 或 []，uri 字段空），页面 file_digests 正文 sha256 与 PAYLOAD 独立比对一致（channel=File）；这是 Firefox/Chrome 对本地文件拖放的真实原生通道。
- 浏览器源文件行登记 BLOCKED 带绊线：chrome-source-file-drop 与 Chrome 文本源同因（Chromium 无视 XTEST 拖拽起始，`browser_source_synthetic_blocked` 绊线同效）；firefox-source-file-drop 为本波未实现真实 Firefox 文件拖拽源（页面夹具仅 text/plain）。
- 回归：B 波文本矩阵全量重跑 10 行结论不变（6 PASS + 4 Chrome BLOCKED，页面夹具扩展无回归）；离线 discover 248 项全过。本层未改 C 角色共享行为（仅 harness 纯新增方法与页面/场景文件），五轮 GTK+Qt 全量回归结论沿用上一轮。
- **C波文件应用矩阵结论（任务#6）**：矩阵 7 行收口（nemo 三行 PASS、Firefox/Chrome 目标行 PASS、Chrome 源 BLOCKED 平台限制登记、Firefox 源 BLOCKED 未实现登记）；文本矩阵 10 行维持。C 波全部完成。阶段2剩余：D 恢复、E 真实 toolkit 输入、F 用户硬件/FUSE 收口。阶段2整体仍不通过，不进入产品阶段3–8。

### D波续作记录（2026-11-20，续上节，代际恢复层）

- 协议扩展：`linux_dnd_wire.h` Command 增 GENERATION(24)（监督者给 source/proxy 分配新代，begin 时发布 generation atom）；`linux_dnd_roles.c` 与 `linux_dnd_qt_control.cpp` 镜像门 `op==GENERATION && is_source && !argc && generation > r.gen && !r.begin && !r.down`（GTK+Qt+python 三处同步）；ARM/ARM_APP/DISARM 三处增 `r.drop = 0` 代际重置（保持一代一次 drop 不变式，复用角色可跨代交付）；`linux_dnd_protocol.py` 枚举与代际跟踪元组同步。
- 新增 `linux_dnd_recovery.py`（约460行，500 行门禁内）：scenario 装饰器注册表、逐场景 FAIL row+traceback、场景间 `x11.release()` 防卡键；helper：`process_token`（ppid+starttime 联合身份）、`Faults` 故障许可注册表（注册记 (pid,starttime)，kill 先校验身份后消费许可，refuse 负控恒拒）、`expect_eof`（EOF+回收+关 FD 后才允许重启）、`stale_refused`（旧代 STATUS 恒 1）、`x11_session`（跨 peer 会话临时改 os.environ DISPLAY/XAUTHORITY）。
- 五场景全 PASS（RECOVERY_DONE rows=5 failures=0，双会话）：D1 source_restart_two_generations（idle 杀源→EOF→剪贴板负控→source2 GENERATION 2→start→ARM gen2→stale×2→两代真实交付）；D2 peer_reconnect_two_generations（gen1 源→catcher→bridge→proxy→target(peer) 全链→ESCAPE+UP 释放→杀 source+catcher→重建 gen2，target 幸存者重 ARM+stale 拒→gen2 交付；peer 侧旧 proxy 窗用 x11_session+move_window 挪开）；D3 controller_eof_mid_drag（拖中杀 target→EOF→源 ESCAPE 孤儿取消→cleanup(cancel, returned=0)→source.close() retire→双重启 gen2 交付）；D4 fault_nested_xephyr（存活主会话内自有故障用嵌套 Xephyr：xauth :999 占位→-displayfd→拿到号后 add :N+remove :999→认证连接→注册故障杀→主会话存活+剪贴板验证；stray sleep 负控恒拒）；D5 new_generation_start_failure（缺二进制 FileNotFoundError、/bin/true impostor EOF、sentinel+catcher 健康检查组合）。
- 根因三登记：①ESCAPE 取消的 GTK drag 遗留 32x32 override-redirect 拖拽图标窗在 press 点（不随 source 窗口销毁）——拖中杀角色须退役老 source 进程才回收；②XTEST 按钮状态 server 全局、客户端死亡不释放——SIGKILL 带键死角色卡 button 1 吞后续 press，杀前必 UP+wait raw_up；③gen2 身份恒拒根因——source begin 发布自己的 r.gen=1，catcher 升代后比对失败，故须监督者 GENERATION 命令预分配新代。
- 离线：`test_linux_dnd_recovery.py`（151 行）8 项（FakeRole socketpair、process_token/Faults 注册杀/拒/PID 复用/expect_eof 生死两组）；两文件入 500 行门禁名单。
- 回归：GTK scenarios+INCR+file 三套件连续五轮全绿（15/15 rc=0、每轮唯一 native_test_summary result=0）；Qt 角色按当前源码（GENERATION 门+drop 代际重置）重建后 qt-matrix 8 场景全 PASS（29 个 PASS 行、坏控制帧负例全过、runner 汇总干净）。
- **D波结论（任务#7 D 部分）**：源重启、peer 重连、controller 恢复、迟到回调攻击新拖拽、新代启动失败逐项覆盖；两代均有真实 selection 交付且旧身份被拒绝；故障只允许已登记精确 PID/启动身份/代际；先取消/reap 旧代确认 FD/设备/挂载释放再创建新代；外层 require_alive 与清理汇总保持严格。E toolkit 输入（任务#7 E 部分）未开始。阶段2整体仍不通过，不进入产品阶段3–8。

### E波续作记录（2026-11-20，续上节，toolkit 输入层）

- 路由二进制 `linux_input_toolkit_route.c`（359 行）：从输入矩阵提取专属 master 路由/注入，usage `route <viewable-xid>`；require_isolated 前缀/display 数字/Xauth/Pulse 校验（exit 2）；`XIAddMaster("Oink-Toolkit-<pid>", True, True)` 建专属 master 对+XTEST slaves（命名唯一性校验）；目标窗 viewable 校验；`XISetClientPointer` 返回不可靠，一律 `XIGetClientPointer` 回验；metacity 焦点 settle（20ms×2 轮询）；`XISetFocus`+`XkbSetDetectableAutoRepeat`；stdin 行命令 `cap/map/key/btn/rel/abs/pos/focus`，查询命令各回一行 JSON；EOF 释放 held 键钮、恢复 client pointer、`XIFloating` 移除 master、xerror 检查。
- 编排 `linux_input_toolkit_scenarios.py`（391 行）+ `linux_input_toolkit_qt_row.py`（159 行）+ Qt 回调夹具 `linux_input_toolkit_qt.cpp`（72 行，`linux_input_toolkit_qt_build.py` c++17 -Werror 构建）+ 浏览器可信事件页 `linux_input_app_page.html`；共享 harness `linux_app_x11.py` 纯新增 `key(keycode, down)`（XTestFakeKeyEvent 封装）。八个矩阵行全 PASS（`INPUT_TOOLKIT_DONE rows=8 failures=0`，wave-6 日志）：
  - `zenity-entry-text`：zenity --entry 真实 GTK 应用，Shift 和弦输入 `Ab3-z`，stdout 逐字节精确比对。
  - `zenity-server-repeat`：按住 'a' 0.9s 得 7 presses/1 release（detectable autorepeat 模型）。
  - `normal-input-comparison`：默认 master 打 'x' + 专属 master 打 'y' 得 `xy`，普通输入不误吞、不独占。
  - `firefox-trusted-input` / `chrome-trusted-input`：真实浏览器内 keydown/keyup/wheel 全 `isTrusted=true`。
  - `nemo-navigation-scroll`：nemo 真实窗口滚轮三向 xwd 截图两两不同（像素证据）。
  - `nemo-key-navigation`：Ctrl+L→输入 `/tmp`→Return，窗口标题变 `tmp`。
  - `qt-native-callback-delivery`：QTextEdit 原生回调验证扫描码/修饰键/AltGr/媒体键/四向滚轮符号/服务端重复。
- 平台交付模型根因（12 轮诊断结论，已入记忆）：本 Xephyr 上 XI2 设备焦点（XISetFocus）对管理窗口不产生 FocusIn、键落 root；XTEST 键钮实际按 core PointerRoot focus+指针位置交付，窗口须在指针下；输入矩阵当年成功因其目标为 override-redirect 且指针在窗上。自研 GTK 夹具（gtk_window+text_view）对全部 XTEST 事件免疫（真实 GTK 应用正常），根因未查清，夹具弃用，GTK 证据改用真实应用（更强证人）。
- Qt 重复模型：Qt xcb 未启用 detectable autorepeat，按住 'a' 得 7 press+7 release 成对（与 GTK/zenity detectable 形态 7 presses/1 release 不同）；断言按非 detectable 诚实模型（presses≥2 且 releases==presses 且末事件为 release）并记录差异。
- 修复留痕：wave-4 唯一红行 qt 重复断言过严→改诚实模型；wave-5 暴露 qt_row 成功路径 `row` 回调未导入（旧路径只走行级 FAIL 从未到达）→补导入；红证据保留于 wave-4/5 日志，未抹除。
- 关闭顺序约束：XIRemoveMaster 移除专属 master 时 toolkit XI2 记账可能引用已删设备（Qt SIGSEGV），必须先关应用再关 router（finally 顺序固定）。
- 只读采集 XIScrollClass/XITouchClass/XIGestureClass 能力（cap 命令）；物理 HID/触摸板/锁屏留 F 波。每客户端事件选择（XSelectEvent per-client），xev -id 旁路观察不干扰 GDK。
- 离线：`test_linux_input_toolkit.py` 8 项重写（guard/banned-token/路由结构/行名/页夹具/500 行门禁/路由零告警编译）；`test_linux_dnd_scenarios.py` 500 行门禁名单追加 6 个 E 波文件；全量 `discover -p 'test_linux*.py'` 264 项全 OK。
- 回归义务：E 波改共享 `linux_app_x11.py`（纯新增方法），GTK scenarios+INCR+file 三套件各一轮原生回归（`e-regress-*.log`）：13+7+14 行全 PASS、零 FAIL、每会话 `native_test_summary` probe_exit/result/remaining_mounts 全 0、cleanup/services/mount_check 全真。首轮回归曾误用 `.release-tools/linux-probes/dnd-roles`（11-17 旧二进制，报告形状不匹配恒报 invalid report shape），换 canonical `.tools/fresh-20261117/bin/dnd-roles`（与当前源码同步重建、systemd-run 限额内编译零告警）后全绿；该旧二进制为历史产物，不再用于跑 suites。此前 D 波五轮 GTK 连续回归与 Qt 重建矩阵结论不受影响。
- **E波结论（任务#7 E 部分）**：专属 master 路由、真实 GTK/Qt/浏览器输入交付、不误吞、修饰键/AltGr/媒体键/服务端重复、相对位移、双向滚轮逐项有证据；F 用户硬件门槛待办。

### F波续作记录（2026-11-20，续上节，用户硬件门槛与收口）

- FUSE：`.release-tools/linux-probes/fuse-user.log` 不存在（已只读核查），无既有证据。按当前源码（`linux_fuse_probe.c` SHA-256 `e08e5ec8…ad300`，自 11-17 未变更）在 systemd-run 限额（4G/CPU200%/Tasks256）内零告警新鲜重建，产物 SHA-256 `6aae95cf…834a`（mtime 2026-11-20 11:52:50）。§7.5 命令已交用户普通终端执行（代理不挂载；NoNewPrivs=1 下 setuid fusermount3 是既有阻塞，不换工具重试）。**用户已执行，代理只读核验通过**：`fuse-user.log` 唯一行 `private_fuse: content_and_offset=ok write_errno=30 expected=30`（EROFS=30 符合预期）+ 唯一 `native_test_summary` probe_exit/result/remaining_mounts 全 0、cleanup/services/mount_check 全真。FUSE 用户实证收口。
- 硬件入口 `linux_hardware_acceptance.py`（238 行）+ 独立测试（98 行，12 项全过）：
  - 默认 `status` 模式只打印操作说明与三份约定日志（`hardware-input-user.log`/`hardware-mic-user.log`/`fuse-user.log`）的存在性与尾部摘要，任何环境（含空 env）可运行、不触硬件。
  - `input-collector`：用户令牌 `OINK_HARDWARE_USER=1` + 拒绝私有探针 scope（`OINK_NATIVE_TEST_ROOT` 存在即拒）；ctypes Xlib 建自有 360x160 窗口，仅对**本窗口**选择 Key/Button/Motion/FocusChange 事件（每客户端 mask，非全局监听），只聚合五类计数；FocusOut 即停（锁屏/会话切换如实记为 `focus_lost`）；不解码 keysym/keycode、不注入不 grab；JSON 行追加写约定日志。
  - `mic-bridge --enable-mic --device <pactl 源名>`：同令牌+显式启用+设备名三重门；校验 `PULSE_SERVER` 必须是私有 runner socket；**真桥方向**：`parec` 用剥离私有 socket 的宿主 env 读用户指定真设备（`host_capture_env` 单测锁定），PCM 直送 `pacat --server=<私有 socket> --device=oink_test` 进私有 sink，中途仅即时计算 RMS dBFS 与字节计数写日志，不落任何音频正文；设备错误如实记 `device_error` 与双端退出码/错误尾部。
  - 纯函数可测：`Collector.feed`（计数/停采语义/未知事件忽略）、`rms_db`（满幅≈0dBFS、静音−inf、半幅≈−6dB）。
- 用户实测暴露两处入口缺陷已修：①采集窗未设 `WM_DELETE_WINDOW`，WM 关窗走 `XKillClient` 整连接被掐（用户实测 `X connection to :0 broken` 且零日志）——补协议+ClientMessage 优雅停（`window_closed`）并回归 13/14 项；②mic 桥初版误把 `parec` 指向私有 socket（读不到宿主设备）——改真桥方向并加 env 剥离单测（现 14/14 过）。
- 麦克风桥根因链（用户首跑 `playback_exit=1 流错误：超时` 后定位）：私有音频栈**无会话管理器**，pulse 协议流永不自动链接——in-session 与 foreign 一样（实测 sink-input/source-output 恒 0）；pacat/pw-cat `--target` 请求无人执行；节点无会话管理器时不建端口；正确配方（复用音频套件 route() 实证）= `pw-cli set-param PortConfig`（dsp 模式强制建端口）+ `pw-link` 按声道显式连。foreign 客户端走 **core pipewire socket**（`oink-test`，module-access 对该 socket unrestricted，pw-dump 实测 foreign 权限 r-xm、节点可建、链接可建）而非 pulse socket。已还原 linux-native-test.py 的 PULSE_CONFIG（session-manager 模块为 1.0.x 空操作桩，无效且属共享 harness，不保留）。
- 桥定稿 `linux_hardware_acceptance.py`（391 行）：`mic-bridge` = 宿主 `parec`（env 剥离私有 socket）读真设备 → RMS 旁路计数 → 宿主 `pw-cat -p` 经 core socket 建 `oink-mic-bridge` 节点 → PortConfig+pw-link 入 `oink_test` sink；`mic-consumer`（反向守卫，仅私有 scope 内运行）= 会话内 `pw-cat -r` 节点显式链接桥节点，RMS 行入 runner 日志。合成正弦端到端实证：foreign 桥节点→会话内消费者链路 active、消费者 RMS −11.7dB、nonzero 23960/24000。真机麦克风首跑已采到真实 RMS（−18.6/−32.2/−25.3/−36.3 dB，随说话起伏），差最终一次桥+消费者同跑收口。
- 软件缺口如实记录：物理 HID/触摸板/触点/锁屏/精细滚动仍 BLOCKED 待用户实证；contacts `0x020b` 产品契约未声称端到端完成。
- 离线全量 `discover -p 'test_linux*.py'` 276 项全 OK（新增 12 项）；两新文件入 500 行门禁名单。
- 收口状态：A/A2/B/C/D/E 全部有证据；F 软件入口就绪，FUSE/物理输入/麦克风三类用户步骤已交付等待，用户完成后代理主动读取约定日志核验。阶段 2 汇总与交接文档更新待用户步骤证据到齐后一并收口。
- **F 最终收口（同日续，用户硬件证据到齐）**：
  - 物理输入：`hardware-input-user.log` 唯一 `input_stop reason=window_closed`——button_press/release 10/10、motion 262、key_press/release 0/0、focus_out 0。按钮与移动交付有实证；本轮采集窗内按键计数为 0，物理按键交付仍缺证据，不推断通过。
  - 真实麦克风最终同跑（私有会话 `oink-native-6h3png1r`，桥先起、会话内消费者轮询后链接）：桥 `mic_start`→`mic_route links=1`（bridge 21→sink 16）→`mic_rms` 起于 −15.6 dB 随说话起伏→`mic_stop reason=timeout capture_exit=0 playback_exit=0`；消费者 `consumer_route links=1`（bridge 21→consumer 19）→持续 `consumer_rms`（nonzero≈2045/2048）→`consumer_stop exit=0 linked=1`；runner 唯一汇总 result/probe_exit/remaining_mounts=0、services/cleanup/mount_check 全真。真实麦克风→私有会话显式链接端到端收口。
  - 收尾缺陷两处（用户实测暴露，均先红后绿、红证据保留）：③`playback` 以 `stderr=DEVNULL` 启动却在收尾读 `.stderr` → `AttributeError`（该次运行 `mic_stop` 未写出）；新增 `_stderr_tail` 空流安全并回归。④超时路径把自身 SIGTERM 收尾误记为设备错误（旧运行 `mic_stop reason=device_error capture_exit=0 playback_exit=1`）；新增 `_stop_process` 归一化操作者主动停止为 0、仅自退非零码计失败并回归。
  - 回归：离线全量 `discover -p 'test_linux*.py'` 285 项 OK（41.2s，含硬件入口 21 项）；原生三套件 `e-regress-dnd-all/incr/file.log` 在 D/E 收口复跑全绿（summary 全 0 位、services/cleanup/mount_check 真，sessions=2 为 peer 所需），此后未改原生共享代码；F 仅新增独立入口与测试。`git diff --check` 通过，产品 `apps/`、`crates/` 零改动；未 commit/push/tag/发布。
  - 阶段 2 判定：A/A2/B/C/D/E/FUSE/麦克风均有证据；仍缺物理按键（本轮 0）、锁屏/切换、触摸板/触点、精细滚动用户实证（BLOCKED），Chromium 合成拖拽起始为平台限制 BLOCKED（绊线保留），contacts `0x020b` 未声称端到端。**阶段 2 仍未通过，不进入产品阶段 3–8**。本轮汇总与交接见 [2026-11-20-linux-x11-phase2-af-waves.md](../handover/2026-11-20-linux-x11-phase2-af-waves.md)。
  - 提交与推送（用户明确授权，仅限本轮交付）：代码 `8dbc9ab432ce3ca7d626ecf46764cd1b96245041`（`test(linux): add phase 2 application, transfer and recovery probe waves`，42 个 `build-support/` 文件）与文档 `65279d1e7706a583254a4947c4c319a0903b0a22`（`docs(linux): record phase 2 wave evidence and hand over hardware gates`）已推送 `origin/main`，`git ls-remote origin refs/heads/main` 核验同一 SHA；未推送 `upstream`、未 tag/Release，`apps`/`crates` 零改动。提交前把 589 行的 `linux_dnd_app_scenarios.py` 拆为 `linux_app_page.py`（142 行，PageServer/digest_words/page_matches/begin_app_drag 纯移动）+ 458 行主文件并保持导入兼容，500 行门禁名单补入本轮全部新源码（含两个 HTML 页面），离线全量 285 项复跑 OK；该拆分未改原生共享 C 代码。

### F 波补证记录（2026-11-20 晚，续上节，物理按键与锁屏证据）

- 范围：只补 F2 剩余物理项与总门槛回归；按用户决策，本机无触摸板/触摸屏（`xinput` 仅 USB 键鼠与 PiKVM），触摸/触点/精细滚动登记 BLOCKED（硬件不可得，不放宽门槛）。FUSE/麦克风不重做。
- 新批次 `.release-tools/linux-probes/phase2-gate-20261120/`（不覆盖历史证据）：bounded（4G/CPU200%/TasksMax1024）零告警重建，`dnd-roles` SHA-256 `213e26bb0b0ba2b9bade3f1b12d0e8d4a8677d735cf5452ea82254e195de63cb` 与 canonical 一致，`input-toolkit-route` `8af9ad1c…4992`，`dnd-qt-fixture` `5765b670…fe35`（Qt 6.12.0）。四套件串行：`dnd-all.log` 13 PASS（sessions=2）、`incr.log` 7 PASS、`input-toolkit.log` `rows=8 failures=0`，summary 全 0/真；`file.log` 首跑红（漏 `--peer-x11`，`staging suite requires runner --peer-x11`）保留，按 `--peer-x11` 复跑 `file-peer.log` 14 PASS（sessions=2）。
- 采集器修复三连（先红后绿，红证据保留）：
  1. `--seconds` 永不触发：`started=time.time()` 与 `time.monotonic()` 混比；新增 `deadline_expired` 并改单调钟（红/绿 `collector-deadline-red/green.log`）。
  2. 启动即 `X Error ... BadMatch ... X_SetInputFocus`（用户实测）：WM 管理窗在 MapNotify 前不可取焦；改为有界 2s 等 MapNotify（`XCheckTypedWindowEvent`）+ 非致命 `XSetErrorHandler` + 循环内重试取焦，`input_start` 增 `mapped`。
  3. 按键恒 0/焦点事件恒无（用户多轮 0/0）：X11 **事件掩码是协议位域，不是事件号**——`1<<KeyPress`(2) 实际选中 ButtonPressMask、`1<<FocusOut`(10) 选中 Button3Motion，键事件从未被订阅；改 `KeyPressMask/KeyReleaseMask/ButtonPressMask/ButtonReleaseMask/PointerMotionMask/StructureNotifyMask`。焦点改轮询自身窗口（0.2s），`focus_lost_is_final` 1s 阈值过滤 WM 启动抖动，失焦写 `input_stop reason=focus_lost`。红 `collector-keys-smoke2.log`（focus_self=true 但 0 键）→ 绿 `collector-keys-smoke3/4.log`（XTEST 7/7、4/4，切窗即停）。
- 可用性：日志逐行 `flush`；窗口标题每秒显示 `keys/buttons/focus`；stdout 5s progress；不记 keysym/keycode。拆分 `linux_hardware_mic.py`（233 行），入口 `linux_hardware_acceptance.py` 329 行，测试 25 项；500 行门禁名单补新文件。
- 用户最终实证（`hardware-input-user.log`，started_epoch 1795184648.75 = 22:24:08）：`input_start mapped/focus_self=true`；progress keys 0→8→19→32→34、buttons 5→17、motion 183 且 focus_self 恒 true；`input_stop reason=focus_lost key_press=34 key_release=35 button_press=17 button_release=17 motion=183 focus_out=1`。
- 回归：离线全量 289 项 OK（41.1s）；四套件如上；`git diff --check` 通过；`apps/`、`crates/` 零改动；未 commit/push/tag。
- 判定：F2 按键 ✅、锁屏/切换 ✅；触摸/触点/精细滚动 BLOCKED（无硬件）；Chromium 源合成拖拽平台限制 BLOCKED（绊线保留）。**阶段 2 仍未通过，不进入产品阶段 3–8**。

### 阶段 2 书面例外收口（2026-11-20，用户决策）

- 用户决策（第五轮交接 §6 路径 B）：**书面例外**。F4 触摸板/触摸/精细滚动因本机无触摸硬件（`xinput` 仅 USB 键鼠与 PiKVM，无触摸板/触摸屏）登记 BLOCKED，不作为阶段 2 阻塞项；F5 Chromium 源合成拖拽起始（平台限制）与 F6 Firefox 文件源（未实现）保持登记，红证据与绊线保留。
- 其余门槛均有证据：A/A2/B/C/D/E 机制、GTK/Qt/浏览器输入矩阵、FUSE 私有挂载、真机麦克风、物理按键（`hardware-input-user.log`：key 34/35、button 17/17、motion 183）、锁屏/切换。
- 阶段 2 门槛据此**以书面例外关闭**；进入产品阶段 3（Linux 桌面壳接入），实施计划与波次见本轮实施记录（阶段 3 结束时补记）。本次例外不放宽 F5/F6 的登记，也不改变后续阶段用真实证据收口的义务。

## 阶段 3 实施记录（2026-11-20 ～ 2026-11-21）

### 入口、后台与桌面生命周期（已推送 C1–C3）

- C1 `84a3b82`：启用 Linux Tauri 依赖与 `main`/`lib` 模块，接入 UI 入口与启动骨架；Wayland/无图形会话拒绝启动并给出准确提示。
- C2 `77a3602`：真实 X11 input/display 后端与 GLib 主循环；输入 `capture_available()` 如实为 false（完整语义属阶段 4），XTEST 注入、`XQueryPointer` 光标；显示用 XRandR 枚举与监听。
- C3 `72e22a2`：Linux 关主窗 → `prevent_close` + `hide`，共享不停止；二次启动经 `show_window` 重显；`oink-agent reopen` 停止旧对并拉起新对；窗口尺寸契约（Main 1040×680+24、min 820×560、Panel 360×520）有门禁测试。
- 证据：`w1/`、`w2/session3`（10/10）、`w3/session2`（14/14）；红证据 `w2/diag1,diag2`、`w3/session1` 保留。

### 本轮 W0–W7（C4 本地提交，未推送）

- W0 基线：HEAD `c5b94e0` 上 clippy 为 71 errors + 1 manifest warning，`cargo fmt --check` 通过；失败集归档 `baseline-*`。
- W1 Linux 托盘：新增 `apps/oink/src-tauri/src/tray_linux.rs`（SNI 探测、TrayIcon 生命周期、可见性、面板开关、光标锚点）。共享 `build_menu(panel_label)` 复用后端菜单模型；面板项四语言标签由 `native_controls::panel_label` 提供并在 `app.rs` 拦截 `native-panel`；`commands.rs` 的 `set_status_icon` 接线。无 watcher 时仅 warn 并跳过，主窗不受影响。修复 `NameHasOwner` 回复按元组读取（原先裸 `bool` 恒为 false，有宿主也会判缺失）并补单测。
- W2 前端 Linux 全分支：`App.tsx` 的 chrome、返回、导航组与托盘开关按 `!== mac` 处理；`PackageDrop`/`Upgrade` 对 Linux 拒绝 dmg/msi 拖放并保留手动发布页；设置 schema 新增 `not: Platform`，Linux 隐藏虚拟麦克风；pasteAsFile 文案改用“文件管理器”；`SettingsPreview` 支持 `?platform=linux`；`app.css` 中/日/韩字体栈补 Noto CJK 与 Droid 回退；新增 Linux 平台用例。`npm run typecheck`/`test`/`build` 全绿（1536 tests）。
- W3 零告警：71 errors + 1 manifest warning → 0（`cargo clippy --locked --workspace --all-targets -- -D warnings` rc=0）。按平台 cfg 门控或窄作用域 allow；`oink-clipboard` 把 `lib.rs` 主体拆到 `body.rs` 并经 `native_linked_promise.rs` 双入口 include，消除同路径多 target 的 manifest warning，保留 macOS 主线程测试。新增探针/助手加入 500 行门禁。
- W4 测试基线：修复 watchdog fixture 竞态（deferred-arm：注册探针后再起表），7 条红 → 17 ok / 0 failed，断言未弱化。全量 `cargo test --locked --workspace --no-fail-fast`：112 target，rc=101，29 条失败全部为存量（10 剪贴板依赖、8 多节点/流、3 DnD、6 `oink-clipboard`、2 `oink-network`）；后 8 条在 HEAD `2114c19` 用独立 worktree 对照复现；本轮引入 0，不 skip、不删断言。`--autostart` 链路为 `handle_autostart_arg` → `LOGIN_LAUNCH_ENV` → `launched_at_login` → `shows_on_start`，后者有单测；XDG 登录项注册留阶段 7。
- W5 托盘探针：新增 `build-support/linux_sni_watcher.py`（私有总线实现 StatusNotifierWatcher + dbusmenu 驱动）与 `linux_phase3_tray_probe.py`（Phase A 无宿主降级；Phase B 注册、菜单、面板开/关/重开、菜单显示主界面）。私有 Xephyr 内 18/18 checks；W2/W3 回归 10/10、14/14（`w4/regression/`）。
- W6 中文渲染：私有 Xephyr `zh_CN.UTF-8` 下托盘探针 18/18（`w6/native`），主窗/面板截图中文正常；设置页 9 个 Linux 分区（general/appearance/layout/input/clipboard/audio/shortcuts/tools/diagnostics）无 tofu；`fc-match` 命中 Noto Sans CJK SC，回退 Droid Sans Fallback（`w6/rendering-notes.txt`）。真实 Cinnamon 会话验收由用户执行（2026-11-21 确认托盘图标、菜单、面板开/关/重开、关窗重显、退出无残留全部通过；记录 `w7/real-session-acceptance.txt`），代理未代启动、未注入。
- W7 文档与 C4：本节与第七轮交接文档；C4 仅本地提交，未推送、未 tag。2026-11-21 后续用户要求提交并推送：`9de8223` 与验收记录 `a2ab43c` 已推送 `origin/main`；该次授权不延伸至后续代理。

### 已知限制（阶段 3 边界）

- 托盘依赖 `org.kde.StatusNotifierWatcher`；无 SNI 宿主静默降级（warn），不阻断功能。
- 面板定位初版 = 光标锚点（libappindicator 不提供托盘矩形），高度内容测量；精细化留后续阶段。
- 输入完整语义（grab/回放/XKB/相对/快捷键/锁屏/热插拔）属阶段 4；当前 `capture_available=false` 如实上报。
- 剪贴板/文件/DnD、音频/虚拟麦克风属阶段 4–6；F4 BLOCKED、F5/F6 保持登记。
- XDG autostart 注册属阶段 7；打包/签名/安装包升级属阶段 8（Linux 已隐藏拖放安装包入口，保留手动发布页）。
- Wayland/XWayland 会话拒绝启动为当前设计。

### 门禁与证据（本机，证据目录被 gitignore）

- `cargo fmt --all -- --check` 通过；clippy rc=0；全量测试为 29 条登记基线；前端 typecheck/test/build 绿；离线 `discover -p 'test_linux*.py'` 289 项 OK；托盘探针 18/18（中英各一轮）、W2/W3 回归全绿。
- 证据根：`.release-tools/linux-probes/phase3-gate-20261120/`（`baseline-*`、`w1`、`w2`、`w3`、`w4`、`w6`；最终门禁与探针复跑在 `w7/`）。
- 交接：`doc/handover/2026-11-21-linux-x11-phase3-closure.md`。

## 阶段 4 实施记录（2026-11-21）

### 波次与代码（W1–W5，本地提交，未推送）

- W1 `f5b78e5`：阶段 4 骨架。`crates/oink-input/src/linux.rs`（561 行）拆为 `linux/{mod,capture,inject,keymap,devices,event,hotkeys,cursor,session}.rs`；`crates/oink-display/src/linux.rs` 拆为 `linux/{mod,enumerate,arrangement,wallpaper}.rs`；新增 `crates/oink-locator/src/x11/{mod,shm,shape,probe}.rs` 与 Linux 后端骨架；新增隔离探针 `linux_phase4_input_probe.c`、`linux_phase4_overlay_probe.c`、`linux_phase4_wallpaper_probe.py` 并纳入 500 行门禁名单。29 文件 +2991/−762。
- W2 `08750ea`：真实 XI2 观察线程。`linux/capture.rs` 建线程/wake/poll，`event.rs` 翻译键/按钮/移动/滚动 valuator，`devices.rs` 热插拔重枚举，`inject.rs` 注入台账，`sourceid` 过滤自身注入。9 文件 +1461/−121。
- W3 `7a3a18a`：抓取与生命周期。`capture/modes.rs` 同步被动/主动抓取、回放/吞掉、`AlreadyGrabbed` 恢复；`hotkeys.rs` 注册/锁定位展开/暂停仅保留恢复组合；`session.rs` logind → Cinnamon → XScreenSaver 锁屏观察（未知≠解锁）；`cursor.rs` 隐藏/遮罩/停靠恢复；`display_power.rs` 观察并接入 `capture_loop.rs` 的 epoch 门。新增 `build-support/fake_logind_service.py`。18 文件 +2225/−337。
- W4 `da2edc3`：`linux/arrangement.rs` 排列事务（快照、陈旧版本拒绝、无操作快速返回、失败回滚并重读回报）；`linux/wallpaper.rs` Cinnamon 壁纸读取/缩略图/变化观察，GSettings keyfile 安全路径。5 文件 +1116/−8。
- W5 `831d929`：`oink-locator` Linux 后端。`linux.rs` 平台装配，`projection/linux.rs`、`notification/linux.rs`、`text_linux.rs`（Pango/Cairo 中文与 DPI），`x11/{shm,shape}.rs` 修复 `XShmSegmentInfo` 需 Box 固定的问题；depth-32 ARGB、override-redirect、XFixes 输入 shape、Expose 重绘、通知点击与 Cinnamon 动效开关。13 文件 +1263/−41。
- 新增/拆分文件均 ≤500 行（最大 `linux/capture.rs` 446、`x11/mod.rs` 438、`wallpaper.rs` 424）。
- W6 `cf5a2fa`：混合负载驱动与叠层示例、全量门禁与 30 分钟负载、证据矩阵、真实会话清单、阶段 5 交接；同一提交在 `Cargo.toml` 关闭 dev incremental 并限 `debug=1`，控制本机构建磁盘/内存占用。仅本地提交，未推送。

### W6 门禁（当前树，HEAD `831d929` + W6 工作区）

- `cargo fmt --all -- --check` rc=0；`cargo clippy --locked --workspace --all-targets -- -D warnings` rc=0（1m56s，含新负载示例，零告警）；`cargo check --locked --workspace --release` rc=0（2m07s，零告警）。
- `cargo test --locked --workspace --no-fail-fast` rc=101：111 target，2110 passed / 19 failed / 48 ignored；19 条全部落在阶段 3 登记的 29 条基线内，本轮新增 0；另有 10 条基线本次未复现，属波动，不声称已修复。失败名单 `gates/failed-names.txt`。
- 前端 `npm run typecheck && npm test && npm run build` 全绿（105 文件 / 1536 测试）；离线 `discover -p 'test_linux*.py'` 289 项 OK。
- 产物重建：`target/debug/oink`（custom-protocol）、`oink-agent`/`oink-backend`、`examples/phase4_overlay_load`，时间戳均晚于前端 `dist`。

### 原生探针（串行、私有 Xephyr、限额 4 GiB / CPU 200% / TasksMax 1024）

- 输入语义 9/9（`input-probe.log`）；红证据 `input-probe-red1.log` 保留：`passive_grab_replay` 失败时 WM 焦点仍在 1152 窗口、目标窗口 4194305 未获焦，复跑焦点正确后重放交付 1/1。判定为探针环境焦点竞态（该 C 探针验证 XI2 服务器语义，不经过产品代码）。
- 遮罩语义 14/14（`overlay-probe-w6.log`）；壁纸 14/14（`wallpaper-probe-w6.log`、`wallpaper-w6/`）。
- 阶段 3 回归：W2 10/10、W3 14/14、壳探针起窗存活、托盘中文轮 18/18（面板 360×520，菜单中文完整）。日志在 `regression/`。

### 混合负载（真实 UI/后端 + 叠层驱动，同一私有 Xephyr）

- 新增 `build-support/linux_phase4_load.py`（≤500 行，纳入门禁名单）与 `crates/oink-locator/examples/phase4_overlay_load.rs`（拖尾/投影/通知混合、每 10s 采样 RSS/CPU/FD/线程/窗口数/Shift 状态、每 60s 状态轮询与 XRandR 签名、暂停周期、通知与 root 输入）。红冒烟 `load-smoke-red1.log` 保留：驱动把 CPU tick 存成 int，第二轮采样 `'int' object is not subscriptable`；修复后 3 分钟冒烟 14/14（峰值 RSS ui_backend 337828 KiB、overlay 58924 KiB）。
- 30 分钟完整负载（1800s，暂停每 300s）：14/14 检查通过（`load.log`、`load/summary.json`）。180 次采样；峰值 RSS ui_backend 406588 KiB、overlay 97916 KiB（合计约 492 MiB，低于 3 GiB 上限）；`status` 轮询 30 次、通知 15 次、暂停周期 6 次、输入轮 96 次、root 按钮事件 186 次；结束时 Shift 未卡住、XRandR 签名稳定、窗口回收（8→8）、无残留进程/僵尸，叠层驱动 exit 0。
- 证据根 `.release-tools/linux-probes/phase4-gate-2026-11-21/`（gitignored）；功能矩阵 `matrix.md` 按“隔离通过/待验收/BLOCKED”分层，真实会话验收清单 `real-session-checklist.md`。

### 已知限制与边界

- Xephyr 无合成器：真实 Cinnamon 下的观感、阴影、动画与多屏拖尾未由代理验收，须用户执行清单。
- 本机无多屏硬件：排列/镜像/旋转/负坐标的隔离证据仅单屏 no-op，真实多屏待验收；触摸/手势沿用 F4 BLOCKED 登记。
- 无双机 peer：Remote 输入/显示反馈真实链路需 Windows 测试机配合。
- 文件/音频/麦克风负载不属阶段 4，未覆盖；F5/F6 登记保持原样。
- 负载点击仅落在 UI 之外的 root 边角，按键仅 Shift 按下/抬起；不向 `DISPLAY=:0` 注入、不抓真实键盘。

### 交接

阶段 5（完整剪贴板、文件与跨机拖放）交接文档：[2026-11-21-linux-x11-phase5-handoff.md](../handover/2026-11-21-linux-x11-phase5-handoff.md)。
