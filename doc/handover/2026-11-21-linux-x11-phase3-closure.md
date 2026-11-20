# Linux X11 阶段 3 收口（第七轮）· 跨代理交接

日期：2026-11-21。仓库：`github.com/GitHubNull/oink`（私有）。分支 `main`，本轮起点 HEAD `2114c19`（阶段 3 C1–C3 与 W1–W3 交接已推送）。C4 为**本地提交、未推送**：提交信息 `feat(linux): complete phase 3 with the tray, Linux UI branches and green gates`，SHA 见 `git log -1`。

## 0. 一句话状态

阶段 3 的桌面壳接入已完成：Linux 编译/入口、真实 X11 input/display 后端、GLib 主循环、关窗隐藏与重显/reopen、系统托盘（StatusNotifierItem）与面板生命周期、前端 Linux 全分支、零告警全量门禁、测试基线裁定与文档收口。全量测试仍有 29 条**已登记存量失败**（阶段 4–5 范围），本轮引入 0 条；真实 Cinnamon 会话的托盘视觉验收已由用户确认（2026-11-21）。阶段 4（完整输入语义/音频/剪贴板）尚未开始，产品功能不得表述为全部已支持。

## 1. 任务表

| ID | 范围 | 状态 | 证据 / 下一步 |
|---|---|---|---|
| C1 | Linux 编译启用 + UI 入口/启动骨架（推送 `84a3b82`） | 完成 | `phase3-gate-20261120/w1/` |
| C2 | 真实 X11 input/display 后端 + GLib 主循环（推送 `77a3602`） | 完成 | `w2/session3` 10/10；红证据 `w2/diag1,diag2` |
| C3 | 关窗隐藏、二次启动重显、reopen、退出顺序、面板尺寸契约（推送 `72e22a2`） | 完成 | `w3/session2` 14/14；红证据 `w3/session1` |
| W0 | 新 HEAD 门禁基线 | 完成 | `baseline-summary.txt`：clippy 71 errors + 1 manifest warning，fmt 通过 |
| W1 | Linux 托盘与面板入口（Rust） | 完成 | `apps/oink/src-tauri/src/tray_linux.rs`；探针 `w4/tray`、`w6/native` 18/18 |
| W2 | 前端 Linux 全分支与测试 | 完成 | typecheck/test/build 绿（1536 tests）；设置页 9 分区截图 `w6/preview/` |
| W3 | 零告警全量门禁与 500 行门禁 | 完成 | clippy rc=0；`test_linux_dnd_scenarios.py` 名单已补新文件 |
| W4 | watchdog 竞态修复与测试基线裁定 | 完成 | 7 条红→绿；`w4/full-suite-summary.txt`、`w4/test-adjudication.txt` |
| W5 | 托盘 SNI 探针（真实 D-Bus 链路） | 完成 | `build-support/linux_sni_watcher.py`、`linux_phase3_tray_probe.py`；中英各 18/18 |
| W6 | 中文渲染与截图验收 | 完成（代理侧） | `w6/native`、`w6/preview`、`w6/rendering-notes.txt` |
| W7 | 文档收口与 C4 本地提交 | 完成 | 本节 + `plan.md` 阶段 3 实施记录；C4 未推送 |
| — | 真实 Cinnamon 会话托盘视觉验收 | 完成（用户确认） | 2026-11-21；记录 `w7/real-session-acceptance.txt` |

## 2. 接手先做的只读核验

```bash
cd /home/wgh/dev/cross_platform_app/oink
git log --oneline -3                     # C4 为最新本地提交；C1-C3 已推送
git status --short                       # 期望干净
ls .release-tools/linux-probes/phase3-gate-20261120/
jq -r '.checks | to_entries | map(select(.value==true)) | length as $ok | "\($ok)/\(length) true"' \
  .release-tools/linux-probes/phase3-gate-20261120/w6/native/summary.json
```

## 3. 阶段 3 已完成行为（按层）

1. **入口/启动**：`apps/oink/src-tauri/src/main.rs` `oink_autostart::handle_autostart_arg()` 后进入 `oink_ui::run()`；Linux `app.rs` 后台线程 `service::start_once` + 单次 `follow`，随后按 `shows_on_start` 打开主窗。Wayland/无图形会话拒绝启动并提示。
2. **后台与主循环**：`crates/oink-agent/src/backend/linux.rs` `run()`（会话分类 → `host::run_service()`）、`on_main`（glib MainContext invoke）、`run_main`（工作线程 + GLib MainLoop）。
3. **单实例/所有权**：`host.rs` 的 `acquire_ui`/`hand_off_to_running_ui`；第二实例 rc=0，经后端广播 `ShowWindow` 让首实例重显同一 X 窗口。
4. **关窗/重开**：主窗 `CloseRequested` → `prevent_close` + `hide`（`app.rs`，日志 `product window hidden on close`）；托盘或二次启动重显；`oink-agent reopen` 停止旧对并拉起新对。
5. **真实平台后端**：`crates/oink-input/src/linux.rs` 真实生命周期与健康检查，`capture_available()=false` 如实上报；XTEST 注入、`XQueryPointer` 光标。`crates/oink-display/src/linux.rs` XRandR 枚举与监听。完整输入语义（grab/回放/XKB/相对/快捷键/锁屏）留阶段 4。
6. **托盘（本轮）**：`apps/oink/src-tauri/src/tray_linux.rs`
   - `install`：会话总线 `NameHasOwner("org.kde.StatusNotifierWatcher")`；有宿主才创建 TrayIcon（`32x32.png`），任何失败只 `warn` 不阻断启动。
   - `update`：复用共享 `native_controls::build_menu(app, model, Some(panel_label))`，菜单模型由后端持有；面板项 id `native-panel`，四语言标签在 `crates/oink-agent/src/native_controls.rs` 的 `panel_label`。
   - `set_visible`：设置「在系统托盘展示」的 Linux 接线；无托盘时返回 `Ok(())` 静默降级。
   - `toggle_panel`/`panel_anchor`：libappindicator 无托盘矩形，以点击光标位置 + 当前显示器 `work_area()`/`scale_factor` 作为面板锚点（1×1 物理矩形）。
   - `app.rs` 先拦截 `native-panel` 调 `toggle_panel`，其余 `native-` 前缀仍走 `native_controls::invoke`。
   - 真实缺陷修复：D-Bus `NameHasOwner` 回复体是 `(b)` 元组，原先 `reply.get::<bool>()` 恒失败导致「有宿主也判缺失」；改为 `reply.get::<(bool,)>()` 并补单测。
7. **前端 Linux 分支**：`App.tsx` 的导航返回/请求组/chrome gutter/托盘开关按 `platform !== 'mac'`；`PackageDrop.tsx` 对 Linux 拒绝 dmg/msi；`Upgrade.tsx` 的 `dropPlatform` 映射 linux 且不弹拖放覆盖层（保留手动发布页）；设置 schema 新增 `not?: Platform`，Linux 隐藏虚拟麦克风；`pasteAsFile` 文案用“文件管理器”；`SettingsPreview` 支持 `?platform=linux`；`app.css` 补 Noto CJK/Droid 回退。
8. **中文渲染**：私有 Xephyr `zh_CN.UTF-8` 下主窗/面板/托盘菜单中文正常；设置页 9 分区无 tofu；`fc-match 'Noto Sans CJK SC'` 命中 `NotoSansCJK-Regular.ttc`。

## 4. 已验证证据（命令、结果、路径）

证据目录均被 gitignore，仅本机磁盘存在。根目录：`.release-tools/linux-probes/phase3-gate-20261120/`。

### 4.1 门禁（本轮最终树）

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings     # rc=0，零告警
cargo test --locked --workspace --no-fail-fast                     # rc=101，仅 29 条登记基线
cd apps/oink && npm run typecheck && npm test && npm run build     # 全绿（1536 tests）
python3 -m unittest discover -s build-support -p 'test_linux*.py'  # 289 项 OK
```

- 告警：W0 的 71 errors + 1 manifest warning → 0；分类修复见 `plan.md` 阶段 3 实施记录。
- 测试基线（`w4/test-adjudication.txt`）：10 剪贴板依赖（oink-agent）+ 8 多节点/流（link_scenarios 7 + planned_exit 1）+ 3 DnD + 6 `oink-clipboard` + 2 `oink-network` = 29；后 8 条在 HEAD `2114c19` 独立 worktree 对照复现；不 skip、不删断言。watchdog 7 条红→绿（deferred-arm，断言未弱化）。

### 4.2 原生探针（私有 Xephyr，串行、限额内）

```bash
DISPLAY=:0 python3 build-support/linux-native-test.py --timeout 300 -- \
  python3 build-support/linux_phase3_tray_probe.py \
    --backend target/debug/oink-backend --ui target/debug/oink \
    --cli target/debug/oink-agent \
    --out .release-tools/linux-probes/phase3-gate-20261120/w6/native \
    --lang zh_CN.UTF-8
```

- Phase A（无 watcher）：主窗可见、UI/后端存活、日志含 `StatusNotifierWatcher absent; Linux tray disabled`、无 panic。
- Phase B（假 watcher）：item 注册、菜单含 `打开面板/显示主界面/退出 Oink`、面板 360 宽开→关→重开、菜单重显主界面、无僵尸/残留。English 轮 `w4/tray` 18/18；中文轮 `w6/native` 18/18。
- W2/W3 回归：`w4/regression/{w2,w3}`，10/10 与 14/14。
- 最终二进制：`target/debug/oink` 02:02:35 晚于 `apps/oink/dist/index.html` 02:02:19 与 `tray_linux.rs` 01:42:52，嵌入最终前端资产与托盘修复。
- 最终二进制串行复跑（`w7/`）：W2 10/10、W3 14/14、托盘中文轮 18/18；面板 360×520，菜单含 `打开面板/显示主界面/退出 Oink`；`gate-summary.txt` 汇总全部门禁。
- 真实会话验收（用户）：2026-11-21 用户确认托盘图标、菜单、面板开/关/重开、关窗后菜单重显、退出无残留全部通过；两次启动均以 `exiting UI end=Stopping` 干净退出。记录：`w7/real-session-acceptance.txt`。

### 4.3 视觉

- `w6/preview/*.png`：`?fixture=settings&platform=linux&lang=zh-Hans` 的 general/appearance/layout/input/clipboard/audio/shortcuts/tools/diagnostics；无 tofu；Linux 分支正确（无卸载/安装包拖放入口、托盘开关存在、虚拟麦克风隐藏、`permissions` 为 mac-only 不显示）。
- `w6/native/*.png`：真实 WebKitGTK 主窗、面板打开/关闭/重开、主窗重显。
- 字体命中与结论：`w6/rendering-notes.txt`。

## 5. 产品代码现状（按文件）

- `apps/oink/src-tauri/src/tray_linux.rs`（新增）：SNI 探测、托盘创建/更新/可见性、面板开关与光标锚点、`name_owned` 单测。
- `apps/oink/src-tauri/src/native_controls.rs`：共享 `build_menu(panel_label)`；Linux `refresh` 调 `tray_linux::update`；Windows 行为不变。
- `apps/oink/src-tauri/src/app.rs`：`native-panel` 事件、Linux `tray_linux::install`、关窗隐藏。
- `apps/oink/src-tauri/src/commands.rs`：Linux `shell_set_status_icon` 接线。
- `apps/oink/src-tauri/src/lib.rs`、`Cargo.toml`、`Cargo.lock`：Linux `tray_linux` 模块与 `gio` 依赖。
- `crates/oink-agent/src/native_controls.rs`：`panel` 标签 + `panel_label` + 四语言去重单测。
- 前端：`App.tsx`、`window/{PackageDrop,Upgrade}.tsx`、`settings/{schema.tsx,SettingsPreview.tsx}`、`app.css`、i18n en/ja/ko 及相关测试。
- 零告警清理：`crates/oink-{input,audio,clipboard,locator}` 的 cfg 门控/窄 allow；`oink-clipboard` 拆分 `body.rs`/`native_linked_promise.rs`。
- 探针：`build-support/linux_sni_watcher.py`、`linux_phase3_tray_probe.py`、`linux_phase3_w2_probe.py`、`linux_phase3_w3_probe.py`、`test_linux_dnd_scenarios.py`（500 行名单）。
- 缺口：完整输入语义、剪贴板/文件/DnD、音频/虚拟麦克风、XDG autostart 注册、打包/签名/安装包升级分别属阶段 4–8。

## 6. 环境手册

- 工作目录：`/home/wgh/dev/cross_platform_app/oink`；分支 `main`；版本 `0.3.5`。
- 工具链：Rust stable（`cargo`）、Node v24.22.1/npm、Python 3（`dbus-python` 1.3.2、GLib）、`dbus-daemon`、Xephyr、metacity、`xdotool/xwininfo/xprop/xwd/ffmpeg/wmctrl/gdbus`；系统包 `libayatana-appindicator3-dev` 已装（本轮不再安装任何系统包）。
- 原生隔离：`build-support/linux-native-test.py` 启动私有 Xephyr/D-Bus/PipeWire，限额 4 GiB / CPU 200% / TasksMax 1024；探针串行执行，禁止对宿主 `DISPLAY=:0` 注入。
- 构建顺序：先 `cd apps/oink && npm run build`（产出 `dist`），再 `cargo build -p oink-ui --features custom-protocol --bin oink`；否则 devUrl 构建没有可执行页面。
- 显示环境：代理 shell 的 `DISPLAY` 可能是私有 Xephyr；GUI 验收须显式 `DISPLAY=:0` 且由用户执行。

## 7. 已知坑与平台限制

- **SNI 必填**：无 `org.kde.StatusNotifierWatcher` 时托盘不出现，仅 warn；Cinnamon 会话自带宿主。XWayland/Wayland 拒绝启动为设计。
- **NameHasOwner 元组**：D-Bus 回复体是 `(b)`，不能裸读 `bool`；假 watcher 探针里同样以 `gdbus` 复核 NameHasOwner。
- **面板锚点**：libappindicator 不提供托盘矩形，当前以光标位置为锚点（已知限制）；面板宽 360、高度内容测量。
- **假 watcher 状态路径**：watcher 以 `build-support/` 为 cwd 运行，`--state` 必须传绝对路径，否则状态写到错误目录导致 `watcher_ready=false`。
- **clippy 基线**：存量 71 errors + 1 manifest warning 已清零；新增代码不得以宽泛 allow 放行，只允许按平台正确 cfg 或窄作用域 allow 并写明理由。
- **测试**：29 条存量失败是阶段 4–5 的真实工作，不是“可忽略噪声”；`oink-network` 两条失败源于本机 Linux 上 closed-UDP 探测超过 1s 超时，语义对齐留给网络阶段。
- **500 行门禁**：`linux_phase3_tray_probe.py` 恰 500 行，新增内容须先拆分/合并。
- **前端预览**：headless Chrome 的 `--virtual-time-budget` 下 smooth scroll 不完成；截图脚本需直接设置 scroller.scrollTop。`permissions` 分区为 mac-only，Linux 下不出现属预期。

## 8. 真实会话托盘验收（已完成，2026-11-21）

代理未向真实桌面注入、未自动启动。用户已在真实 Cinnamon 会话执行并确认：托盘出现 Oink 图标；菜单含「打开面板 / 显示主界面 / 退出 Oink」；面板可开、再点关闭、第三次重开；关闭主窗后仍能从托盘重显；退出后无残留进程。两次启动均以 `exiting UI end=Stopping` 干净退出，日志摘要见 `w7/real-session-acceptance.txt`。

复查命令（零占位符）：

```bash
/home/wgh/dev/cross_platform_app/oink/target/debug/oink
```

已知限制：面板在光标位置弹出。

## 9. 用户红线与禁止事项（继续生效）

- 不 sudo、不安装系统包、不改 CI/GitHub Actions；不 tag、不发 Release；C4 不推送（用户仅授权本地提交）。
- 严禁向宿主 `DISPLAY=:0` 注入合成键鼠或 grab 真实键盘；合成输入仅限私有 Xephyr。
- 不记录真实按键内容、剪贴板/音频正文、凭证；只记录结构化状态。
- 保留红证据；不得把 F4/F5/F6 登记改写为 PASS；不以单测替代交互证据。
- 构建/原生限额 4 GiB、CPU 200%、TasksMax 1024；原生探针串行。
- 单文件 ≤500 行；零告警；不做简化实现；中文沟通；状态按层如实汇报。

## 10. 验收标准（DoD）与结论

阶段 3 完成条件：W1–W7 全部落地 + 门禁绿或基线正式登记 + 文档收口 + C4 本地提交（未推送）。

- 已完成：托盘真实注册/菜单/面板生命周期、前端 Linux 分支、零告警、29 条测试基线登记、中英渲染证据、文档。
- 真实 Cinnamon 会话托盘视觉验收已由用户确认（2026-11-21）；完整输入语义等属后续阶段。
- 结论：阶段 3 按上述 DoD 收口；不把后续阶段能力表述为已支持。

## 11. 交接记录

| 日期 | 代理 | 范围 | 结果 | 产物 |
|---|---|---|---|---|
| 2026-11-20 | Qoder | 阶段 3 C1：入口/启动骨架 | 完成并推送 `84a3b82` | 代码 + `w1/` |
| 2026-11-20 | Qoder | 阶段 3 C2：真实 X11 后端 + GLib | 完成并推送 `77a3602` | 代码 + `w2/` |
| 2026-11-20 | Qoder | 阶段 3 C3：关窗隐藏/重显/reopen | 完成并推送 `72e22a2` | 代码 + `w3/` |
| 2026-11-21 | Qoder | 阶段 3 W0–W7：托盘、前端分支、零告警、基线、探针、渲染、文档 | 完成；C4 本地提交未推送 | 代码 + 探针 + `w4/`、`w6/`、`w7/` + 本文档 |
