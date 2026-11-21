# Linux X11 阶段 5 交接 · 完整剪贴板、文件与跨机拖放（第九轮）

## 0. 一句话状态

**阶段 4（输入完整语义、显示事务/壁纸、原生可视反馈）已完成 W1–W5 实现与 W6 门禁；W6 仅本地提交，未推送。阶段 5 未开始。** 真实 Cinnamon 会话的观感/多屏/双机项仍待用户验收（清单见 §9）。

## 1. 任务表（阶段 5，全部未开始）

计划正文：`doc/linux-x11/plan.md` §5。范围与顺序建议：

| ID | 范围 | 状态 | 入口/证据 |
|---|---|---|---|
| P5-1 | CLIPBOARD selection 生命周期、XFixes 变化观察、TARGETS/TIMESTAMP/INCR、多表示形式、所有权变化/取消；PRIMARY 不擅自同步 | 未开始 | `crates/oink-clipboard/src/body.rs` 背板；`linux/` 子模块待建 |
| P5-2 | 文本/HTML/RTF/PNG/URL/文件目录/隐私标记/Linux 本机 MIME；元数据与正文分离、owner 退出、延迟提供、自写防回环 | 未开始 | `content/`、`negotiation.rs`、`received.rs`、`cache.rs`、`dedupe.rs` |
| P5-3 | 复用 `ClipboardBackend`/`Provider`/`Translator`/`ReaderGuard`/grants/namespace/received/cache，不另建传输协议 | 未开始 | `crates/oink-agent/src/clip.rs`、`clip/runtime.rs` |
| P5-4 | `DndBackend` probe/hold/resume/finish、begin/cancel/source_content；generation、取消、3 秒准备预算、原生完成确认 | 未开始 | `crates/oink-clipboard/src/dnd.rs`、`crates/oink-agent/src/dnd/*` |
| P5-5 | 私有只读 FUSE3 桥接 `Namespace`：根不递归、目录分页、版本/offset 读、受限缓存、句柄/lease 生命周期；仅本机挂载 URI，不上网绝对路径 | 未开始 | 阶段 2 `linux_fuse_probe.c` 与 FUSE 证据可复用 |
| P5-6 | Nemo“粘贴为文件”入口，复用现有转换；安装/关闭不残留失效入口 | 未开始 | `crates/oink-agent/src/paste_file.rs`（440 行） |
| P5-7 | 大文件、目录、浏览器沙箱访问实测；不得先全量下载或用文件名冒充 | 未开始 | 需新增原生探针与真实会话验收 |

现状（如实）：`platform_backend()` 在 Linux 返回 `Err(Unsupported)`（`crates/oink-clipboard/src/body.rs:354`），`platform_dnd()` 同为 `Err(Unsupported)`（`crates/oink-clipboard/src/dnd.rs:475`）。`Node::platform` 对二者只 warn/info 后置空（`crates/oink-agent/src/node.rs:117`、`:484`），故当前 `status` 的剪贴板/DnD 能力为不可用、属预期。

## 2. 接手先做的只读核验

```bash
cd /home/wgh/dev/cross_platform_app/oink
git log --oneline -8                      # HEAD 应为阶段 4 W6 提交（831d929 之后）
git status --short                        # 除文档外应干净
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
python3 -m unittest discover -s build-support -p 'test_linux*.py'   # 289 项
```
不得跳过全量测试门禁：只需“29 条登记基线之外零新增”，失败名单见阶段 3/4 证据。

## 3. 阶段 4 已完成行为（按层）

- **输入（P4-1）**：真实 XI2 观察线程与事件翻译（键/按钮/移动/滚动 valuator）；自身注入按 `sourceid` 过滤；同步被动抓取支持回放/吞掉，主动抓取阻断本地交付且 `AlreadyGrabbed` 可恢复；热键注册与锁定位展开；暂停仅保留恢复组合并重建抓取、清理注入台账；logind → Cinnamon → XScreenSaver 锁屏观察（未知≠解锁）；XKB 可检测重复；热插拔重枚举；相对模式；光标隐藏/全覆盖遮罩/停靠恢复。入口 `crates/oink-input/src/linux/mod.rs`，抓取模式 `linux/capture/{modes.rs,events.rs,x11.rs}`。
- **显示（P4-2）**：XRandR 枚举/监听、排列事务（快照、陈旧版本拒绝、无操作快速返回、失败回滚与重读回报）、Cinnamon 壁纸读取/缩略图/变化观察（GSettings keyfile 安全路径）。入口 `crates/oink-display/src/linux/{mod.rs,arrangement.rs,wallpaper.rs}`。
- **可视反馈（P4-3）**：拖尾/投影/通知 Linux 后端；depth-32 ARGB、override-redirect、不抢焦点；XFixes 输入 shape（装饰穿透、卡片可点）；Expose 重绘、隐藏撤窗；XShm 上传（已修复 `XShmSegmentInfo` 需 Box 固定的坑）；Pango/Cairo 中文文字与 DPI 档位；通知点击/反馈与 Cinnamon 动效开关。入口 `crates/oink-locator/src/{linux.rs,text_linux.rs,projection/linux.rs,notification/linux.rs}`、`src/x11/{mod.rs,shm.rs,shape.rs}`。
- 阶段 3 的 UI/托盘/单实例/GLib 主循环与前端 Linux 分支均保持；阶段 4 未改 `apps/` 前端代码。

## 4. 已验证证据（2026-11-21，证据根 `.release-tools/linux-probes/phase4-gate-2026-11-21/`，gitignored）

```bash
cargo fmt --all -- --check                                           # rc=0
cargo clippy --locked --workspace --all-targets -- -D warnings       # rc=0，零告警
cargo test --locked --workspace --no-fail-fast                       # rc=101，19/19 均为 29 条登记基线
cargo check --locked --workspace --release                           # rc=0，零告警
cd apps/oink && npm run typecheck && npm test && npm run build       # 1536/1536，build 绿
python3 -m unittest discover -s build-support -p 'test_linux*.py'    # 289 OK
```

- 门禁汇总：`gates-summary.txt`；日志在 `gates/`（含失败名单 `failed-names.txt`）。
- 原生探针（串行、私有 Xephyr、限额 4 GiB/CPU 200%/TasksMax 1024）：输入语义 9/9、遮罩语义 14/14、壁纸 14/14；阶段 3 回归 W2 10/10、W3 14/14、壳探针起窗、托盘中文 18/18。红证据 `input-probe-red1.log` 保留（WM 焦点在重放瞬间仍在 1152 窗口；绿跑 `focus=4194305`），判定为探针环境焦点竞态，非产品缺陷。
- 混合负载：3 分钟冒烟 14/14；30 分钟完整负载 14/14（`load.log`/`load/summary.json`）：180 次采样，峰值 RSS ui_backend 406588 KiB + overlay 97916 KiB，30 次状态轮询、15 次通知、6 次暂停周期、186 个 root 按钮事件，无卡键、无残留/僵尸、窗口 8→8 回收。红冒烟 `load-smoke-red1.log` 保留（驱动把 CPU tick 存成 int 导致第二轮采样崩溃，已修复）。
- 功能矩阵：`matrix.md`（隔离通过/待验收/BLOCKED 分层）；真实会话清单：`real-session-checklist.md`。
- 既有视觉证据：`xephyr-w5.log`（拖尾/投影/通知）、`notification-ppm/`（中文墨迹）、`wallpaper/` 与 `wallpaper-w6/`（PNG/JPEG 探测产物）。

## 5. 产品代码现状（阶段 5 入口）

- **剪贴板工厂**：`crates/oink-clipboard/src/body.rs:354` `platform_backend()` 仅 mac/windows 分支，Linux 走 `Err(ClipboardError::Unsupported)`。`body.rs`（366 行）是平台无关背板，Linux 实现应按职责拆分 `src/linux/` 并 ≤500 行/文件。
- **DnD 工厂**：`crates/oink-clipboard/src/dnd.rs:475` `platform_dnd()` 同上；`DndBackend` trait 与 trait 方法（probe/hold/resume/finish/begin/cancel/source_content）已定义在 `dnd.rs`。agent 侧编排在 `crates/oink-agent/src/dnd/{link_platform.rs,preparation.rs,preparation_choice.rs,files.rs,source_release.rs,watchdog.rs,timing.rs,wire.rs,cancel_tests.rs}`，不应重写协议层。
- **agent 接线**：`crates/oink-agent/src/node.rs:117` 失败仅 warn 并置 `clipboard: None`；`:484 start_platform_dnd` 失败 info “drag and drop unavailable”。阶段 5 在此接真实 backend，不改既有模块状态机。
- **粘贴为文件**：`crates/oink-agent/src/paste_file.rs`（440 行）已有转换与入口逻辑；Nemo 入口属阶段 5 补测/接线，不是新协议。
- **FUSE 基座**：阶段 2 已交付 `build-support/linux_fuse_probe.c`、fusermount3 私有挂载与清理证据；阶段 5 只读桥接应复用该验证过的挂载/归属/清理路径。
- **共享抽象**：`librs`、`content/namespace.rs`、`content/promise.rs`、`grants`、`received.rs`、`cache.rs`、`dedupe.rs`、`negotiation.rs`；阶段 5 必须复用而不是新建传输协议。
- **后端缺口**：Linux clipboard/dnd 两个工厂是阶段 5 的唯一起点；其余（音频/麦克风、协议/设置、打包）仍属阶段 6–8。

## 6. 环境手册

- 工作目录 `/home/wgh/dev/cross_platform_app/oink`；分支 `main`；版本 `0.3.5`；阶段 4 W1–W5 本地提交 `f5b78e5, 08750ea, 7a3a18a, da2edc3, 831d929`（未推送，W6 另有一笔本地提交）。
- 工具链：Rust stable、Node v24.22.1/npm、Python 3（dbus-python 1.3.2、GLib）、`dbus-daemon`、Xephyr、metacity、xdotool/xwininfo/xprop/xwd/ffmpeg/wmctrl/gdbus、Nemo、Chrome/Firefox、Qt 6.12 本地 kit（仅探针）。**不 sudo、不安装系统包。**
- 原生隔离：`build-support/linux-native-test.py`（私有 Xephyr 1280x800、私有 HOME/OINK_HOME/XDG/D-Bus/PipeWire、禁 Xephyr 虚拟键鼠；探针串行）。模板：
  ```bash
  DISPLAY=:0 python3 build-support/linux-native-test.py --timeout 300 -- \
    <probe command> > <evidence>.log 2>&1
  ```
- 构建顺序：先 `cd apps/oink && npm run build`，再 `cargo build -p oink-ui --features custom-protocol --bin oink`；`cargo build -p oink-agent --bins`；负载示例 `cargo build -p oink-locator --example phase4_overlay_load`。二进制必须晚于 `dist/index.html`。
- C 探针重建：`gcc -O2 -Wall -Wextra -Werror $(pkg-config --cflags x11 xi xtst xfixes) ... $(pkg-config --libs x11 xi xtst xfixes)`。
- 显示：代理 shell 的 `DISPLAY` 可能是私有 Xephyr；真实桌面验收须 `DISPLAY=:0` 且由用户执行。
- 网络：`https_proxy=http://127.0.0.1:7890`；git HTTPS 被 RST 时用 `git@github.com:GitHubNull/oink.git`。
- 负载与限额：`build-support/linux_phase4_load.py` 可复用于阶段 5 的共跑背景检查（`--duration/--pause-every/--out`）；任何原生探针仍须串行。

## 7. 已知坑与平台限制

- **XI2 事件掩码是协议位域，不是事件号**：历史上 `1<<KeyPress` 会选中 ButtonPressMask；阶段 4 已按掩码常量修正，阶段 5 观察 XFixes/Xdnd 事件时勿重蹈。
- **WM 焦点竞态**：探针启动窗口后焦点可能短暂停在 WM 帧/其他窗口（如 1152）。断言前必须轮询到目标窗口获得焦点；此类红跑先判别环境竞态，保留红证据再复跑。
- **Xephyr 合成输入边界**：Xephyr 虚拟键鼠被禁，但 XTest 在私有会话内可用；负载点击只落在 UI 之外的 root 边角，避免误触产品 UI；按键只用 Shift 按下/抬起（不输入正文）。
- **XShm**：`XShmSegmentInfo` 必须 Box 固定（分配器搬移会导致 BadShmSeg/花屏），阶段 4 已修复，阶段 5 的 FUSE/预览窗口若复用 SHM 保持该约束。
- **Xvfb 不存在**：本机只有 Xephyr+metacity；Xephyr 无合成器，不得把其观感/动画当作真实桌面证据。
- **测试基线**：29 条存量失败跨阶段真实存在（剪贴板依赖、多节点/流、DnD、`oink-clipboard`、`oink-network`）；全量跑呈现 19–29 条波动，只允许“零新增”，禁止 skip/删断言。
- **500 行门禁**：`build-support/test_linux_dnd_scenarios.py:147-173` 维护名单；新文件超 500 行即拆。
- **中文**：Xephyr 内 `zh_CN.UTF-8` 可用；Pango/Cairo 字体命中 Noto Sans CJK SC，不得以空白 fallback 验收。
- **F4/F5/F6 登记不改写**：触摸/手势 BLOCKED（硬件不可得）、Chromium 源合成拖拽平台限制、Firefox 文件源未实现，红证据与绊线保留。
- **Wayland**：拒绝启动为当前设计；不要用 XWayland 验证。

## 8. 用户红线与禁止事项（继续生效）

- 不 sudo、不安装系统包、不改 CI/GitHub Actions；不 tag、不发 Release。
- **推送授权不延续**：阶段 4 W1–W5 与 W6 的本地提交均未推送；后续推送、合并、发版必须另行取得用户明确授权。
- 不记录真实按键内容、剪贴板正文、音频正文、凭证；证据只记结构化状态/尺寸/计数。
- 保留红证据；F4/F5/F6 登记不得改写为 PASS；不以单测替代交互证据。
- 构建/原生限额 4 GiB、CPU 200%、TasksMax 1024；原生探针串行。
- **严禁**向宿主 `DISPLAY=:0` 注入合成键鼠或 grab 真实键盘；合成输入仅限私有 Xephyr；真实桌面手测交用户。
- 剪贴板/DnD 阶段尤其禁止把用户真实剪贴板内容写入日志或证据。

## 9. 验收标准（DoD）与待用户确认项

阶段 5 DoD（全部满足才算完成）：

1. CLIPBOARD/PRIMARY 语义、XFixes 观察、TARGETS/TIMESTAMP/INCR、多格式与所有权/取消均有真实 X11 证据；自写防回环、owner 退出、延迟提供有测试。
2. `DndBackend` 全方法接线，probe/hold/resume/finish/begin/cancel/source_content 有隔离 DnD 套件证据；generation/取消/3 秒预算/原生完成确认不弱化。
3. 私有只读 FUSE3 桥：根不递归、分页、版本/offset、受限缓存、句柄/lease、卸载清理；无 `allow_other`；路径/符号链接/逃逸/冲突检查；网络不带源端绝对路径。
4. 错误不伪造 EOF；断网/取消/磁盘满/配额/陈旧版本显式失败；退出取消并卸载，仅清理验证归属的挂载。
5. Nemo“粘贴为文件”有效且关闭/卸载后无失效入口；大文件、目录、浏览器沙箱访问实测。
6. 门禁：fmt、零告警 clippy、全量测试零新增、Python 离线全量、原生探针串行全绿；阶段 4 回归仍绿。
7. 真实桌面验收由用户执行并记录；双机 Remote 用 Windows 测试机实测（本机隔离环境无双机 peer）。

待用户确认（阶段 4 遗留，不阻塞阶段 5 开工但需在阶段收口前完成）：

- 真实 Cinnamon 会话：输入切换/紧急恢复/锁屏、壁纸显示与切换、拖尾/投影/通知观感与多屏行为、30 分钟负载后的稳定性（清单 `real-session-checklist.md`）。
- 多屏排列/镜像/旋转/负坐标：本机无多屏硬件，隔离仅单屏 no-op。
- 双机 Remote 输入/显示反馈真实链路：需 Windows 测试机配合。
- 触摸/手势：BLOCKED（F4，硬件不可得），不改写。

## 10. 交接记录

| 日期 | 代理 | 范围 | 结果 | 产物 |
|---|---|---|---|---|
| 2026-11-21 | Qoder（第八轮接手） | 阶段 4 W1–W6 | 输入/显示/反馈实现完成；门禁与原生探针全绿；混合负载完成；待真实会话验收 | 本地提交 `f5b78e5..831d929` + W6 提交；证据 `.release-tools/linux-probes/phase4-gate-2026-11-21/`；本文档 |
