# Linux X11 阶段 4 续作 · 跨代理交接（第八轮）

日期：2026-11-21。仓库：`github.com/GitHubNull/oink`（私有）。分支 `main`，版本 `0.3.5`。基线环境：Linux Mint 22.3 / Cinnamon / Xorg / x86_64；另有用户的 Windows 测试机用于双机验收。当前 HEAD 见 `git log -1`（即本文档所在文档提交）；阶段 3 C4 `9de8223` 与验收记录 `a2ab43c` 已按用户要求推送到 `origin/main`。接手时先执行 `git ls-remote origin refs/heads/main` 与本地 HEAD 核对。

## 0. 一句话状态

**阶段 3 已完成并推送**：Linux 入口/启动骨架、真实 X11 input/display 后端、GLib 主循环、关窗隐藏与重显/reopen、系统托盘与面板生命周期、前端 Linux 分支、零告警门禁、29 条测试基线正式登记、中文渲染证据、真实会话托盘验收。**阶段 4（输入/显示/原生可视反馈）尚未开始**：当前输入 `capture_available()=false`、不 grab、不隐藏光标；显示只有 XRandR 枚举/监听；`oink-locator` 无 Linux 后端。不得把阶段 4–8 能力表述为已支持。

## 1. 任务表（阶段 4，全部未开始）

| ID | 范围 | 状态 | 入口 / 验收要点 |
|---|---|---|---|
| P4-0 | 基线核验与结构调整 | 未开始 | 重跑 §4 门禁确认基线不漂移；`crates/oink-input/src/linux.rs` 当前 561 行，已超 500 行上限（C2 起的历史遗留），按计划拆成 `crates/oink-input/src/linux/` 子模块，不得继续加长 |
| P4-1 | 输入完整语义 | 未开始 | 生产 `start_loop`（`linux.rs:255` 现为占位）；sync grab/replay 与 `Disposition::Swallow`；相对模式、XKB/热键/紧急恢复、滚轮/手势、热插拔、`session_gate` epoch、锁屏/会话切换/休眠恢复、只释放本应用注入状态 |
| P4-2 | 显示事务与壁纸 | 未开始 | 在 `crates/oink-display/src/linux.rs`（现有 `backend()` :43、`enumerate()` :103、`watch_loop()` :298）上复用 `native_configuration`/`apply_native_origins`：写前重读、陈旧版本拒绝、失败恢复并上报实际结果；Cinnamon 壁纸读取与变化观察；镜像/旋转/缩放/负坐标/热插拔 |
| P4-3 | 原生可视反馈 | 未开始 | `oink-locator` 目前只有 macos/windows 模块（`lib.rs:21-27`），需补拖尾/定位/投影/通知 Linux 后端并复用 raster/动画；ARGB 窗口 + Shape/XFixes 输入区域（装饰穿透、通知可点、不抢焦点）；Pango/Fontconfig/Cairo 中文渲染 |
| P4-4 | 门禁、证据与双机验收 | 未开始 | §9 DoD 全部门禁；输入安全与布局反馈功能矩阵；30 分钟混合负载；Windows 双机与真实桌面由用户执行；F4 触摸 BLOCKED 保持登记，F5/F6 不得改写 |

阶段 4 的完整需求以 `doc/linux-x11/plan.md` 第 4 节与 Verification 为准；本表只是入口索引。

## 2. 接手先做的只读核验

```bash
cd /home/wgh/dev/cross_platform_app/oink
git log --oneline -5
git status --short                      # 期望干净
git ls-remote origin refs/heads/main    # 与本地 HEAD 一致
wc -l crates/oink-input/src/linux.rs    # 561，阶段 4 拆分
jq -r '.checks | to_entries | map(select(.value==true)) | length as $ok | "\($ok)/\(length) true"' \
  .release-tools/linux-probes/phase3-gate-20261120/w7/tray-zh/summary.json
ls .release-tools/linux-probes/phase3-gate-20261120/
```

## 3. 阶段 3 已完成行为（按层）

1. **入口/启动**：`apps/oink/src-tauri/src/main.rs` 处理 `--autostart` 后进入 `oink_ui::run()`；Linux 后台线程 `service::start_once` + `follow`；Wayland/无图形会话拒绝启动并提示。
2. **后台与主循环**：`crates/oink-agent/src/backend/linux.rs` 的 `run()`/`on_main`（glib MainContext invoke）/`run_main`（工作线程 + GLib MainLoop）。
3. **单实例/所有权**：`host.rs` 的 `acquire_ui`/`hand_off_to_running_ui`；第二实例 rc=0，经后端广播 `ShowWindow` 让首实例重显同一 X 窗口。
4. **关窗/重开**：`app.rs:540` `prevent_close` + `hide`（日志 `product window hidden on close`）；托盘或二次启动重显；`oink-agent reopen` 停止旧对并拉起新对。
5. **托盘**：`apps/oink/src-tauri/src/tray_linux.rs`（169 行）——`install`:14 检查 SNI 宿主、`update`:36 重建共享菜单、`set_visible`:53 跟随设置、`toggle_panel`:62 面板开关；`app.rs:391` 拦截 `native-panel`，`app.rs:412` 安装托盘；共享菜单 `native_controls.rs:139 build_menu`；面板项四语言标签 `crates/oink-agent/src/native_controls.rs:77 panel_label`；`commands.rs:161` 接线 `shell_set_status_icon`。面板 360×520（`shell_policy.rs:190-191`），锚点为光标位置（libappindicator 无托盘矩形，已知限制）。
6. **前端 Linux 分支**：`App.tsx` 导航/chrome/托盘开关按 `platform !== 'mac'`；`PackageDrop.tsx` 对 Linux 拒绝 dmg/msi；`Upgrade.tsx` 映射 linux 且不弹拖放覆盖层；设置 schema `not?: Platform` 对 Linux 隐藏虚拟麦克风；`pasteAsFile` 用“文件管理器”；`SettingsPreview` 支持 `?platform=linux`；`app.css` 补 Noto CJK/Droid 回退。
7. **门禁与测试**：零告警（W0 的 71 errors + 1 manifest warning → 0）；29 条存量失败正式登记（10 剪贴板依赖 + 8 多节点/流 + 3 DnD + 6 `oink-clipboard` + 2 `oink-network`），本轮引入 0；watchdog fixture 竞态已修（deferred-arm，7 条红→绿，断言未弱化）。

## 4. 已验证证据

证据目录被 gitignore，仅本机磁盘存在。根目录：`.release-tools/linux-probes/phase3-gate-20261120/`。

### 4.1 门禁（阶段 3 最终树）

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings     # rc=0，零告警
cargo test --locked --workspace --no-fail-fast                     # rc=101，仅 29 条登记基线
cd apps/oink && npm run typecheck && npm test && npm run build     # 全绿（1536 tests）
python3 -m unittest discover -s build-support -p 'test_linux*.py'  # 289 项 OK
```

- 汇总：`w7/gate-summary.txt`；失败清单 `w7/failed-names-final.txt`；裁定 `w4/test-adjudication.txt`。
- 告警分类修复与 29 条基线登记见 `doc/linux-x11/plan.md`「阶段 3 实施记录」。

### 4.2 原生探针（私有 Xephyr，串行、限额内）

```bash
DISPLAY=:0 python3 build-support/linux-native-test.py --timeout 300 -- \
  python3 build-support/linux_phase3_tray_probe.py \
    --backend target/debug/oink-backend --ui target/debug/oink \
    --cli target/debug/oink-agent \
    --out .release-tools/linux-probes/phase3-gate-20261120/w7/tray-zh \
    --lang zh_CN.UTF-8
```

- 最终二进制串行复跑：W2 10/10、W3 14/14、托盘中文轮 18/18；Phase A 无 watcher 降级、Phase B 真实注册/菜单/面板开→关→重开/菜单重显主界面。
- 假宿主：`build-support/linux_sni_watcher.py`（StatusNotifierWatcher + dbusmenu 驱动）；探针：`build-support/linux_phase3_tray_probe.py`（恰 500 行）。
- 真实 Cinnamon 会话验收：2026-11-21 用户确认托盘图标、菜单、面板开/关/重开、关窗后菜单重显、退出无残留；记录 `w7/real-session-acceptance.txt`。

### 4.3 视觉

- `w6/preview/*.png`：`?fixture=settings&platform=linux&lang=zh-Hans` 的 9 个分区无 tofu；Linux 分支正确。
- `w6/native/*.png`：真实 WebKitGTK 主窗、面板开关、主窗重显。
- 字体命中：`fc-match 'Noto Sans CJK SC'` → `NotoSansCJK-Regular.ttc`（`w6/rendering-notes.txt`）。

## 5. 产品代码现状（阶段 4 入口）

- `crates/oink-input/src/linux.rs`（561 行）：真实生命周期与健康检查；`start()`:250、`start_loop()`:255 仍为占位/记录模式；`:275` 上报 `capture_available(false)`；`:328` 明确“阶段 3 不 grab、不隐藏光标”。阶段 4 在此实现生产抓取/回放并拆分目录。
- `crates/oink-display/src/linux.rs`（358 行）：XRandR 枚举 `:103`、变化监听 `:298`、`backend()`:43；尚无排列事务、壁纸观察。
- `crates/oink-locator/src/lib.rs`（850 行）：仅 `mod macos` / `mod windows`（`:21-27`），Linux 无后端；`platform()` 返回 `None`。阶段 4 需补 trail/projection/notification 的 Linux 实现与 ARGB/Shape 窗口。
- `crates/oink-agent/src/{host.rs,backend/linux.rs}`：单实例、UI 交接、GLib 主循环已就绪，阶段 4 应复用而非另建。
- `apps/oink/src-tauri/src/{tray_linux.rs,native_controls.rs,app.rs,commands.rs,shell.rs,shell_policy.rs}`：托盘与面板链路完成；阶段 4 不改语义，只承接设置/映射的 Linux 目标。
- 前端：`App.tsx`、`window/{PackageDrop,Upgrade}.tsx`、`settings/{schema.tsx,SettingsPreview.tsx}`、`app.css`、i18n en/ja/ko 与测试；阶段 4 若新增设置项需保持 ja/ko 键完整与 1536+ 测试全绿。
- 缺口总表：完整输入语义与显示事务（阶段 4）、剪贴板/文件/DnD（阶段 5）、音频/虚拟麦克风（阶段 6）、协议/设置/XDG autostart/诊断（阶段 7）、打包签名与本地升级（阶段 8）。

## 6. 环境手册

- 工作目录：`/home/wgh/dev/cross_platform_app/oink`；分支 `main`；版本 `0.3.5`。
- 工具链：Rust stable（cargo）、Node v24.22.1/npm、Python 3（dbus-python 1.3.2、GLib）、`dbus-daemon`、Xephyr、metacity、`xdotool/xwininfo/xprop/xwd/ffmpeg/wmctrl/gdbus`；`libayatana-appindicator3-dev` 已装。**不 sudo、不安装系统包。**
- 原生隔离：`build-support/linux-native-test.py` 启动私有 Xephyr/D-Bus/PipeWire，限额 4 GiB / CPU 200% / TasksMax 1024；探针必须串行。
- 构建顺序：先 `cd apps/oink && npm run build`（产出 `dist`），再 `cargo build -p oink-ui --features custom-protocol --bin oink`；否则 devUrl 构建没有可执行页面。二进制要晚于 `dist/index.html`。
- 显示：代理 shell 的 `DISPLAY` 可能是私有 Xephyr；真实桌面验收须 `DISPLAY=:0` 且由用户执行。
- 网络：需要访问外网时使用 `https_proxy=http://127.0.0.1:7890`（cargo/gh 等）；git HTTPS 握手被 RST 时改用显式 SSH 地址 `git@github.com:GitHubNull/oink.git`。
- 证据：`.release-tools/` 被 gitignore，不得强制入库。

## 7. 已知坑与平台限制

- **SNI 必填**：无 `org.kde.StatusNotifierWatcher` 时托盘仅 warn 不出现；Cinnamon 自带宿主。XWayland/Wayland 拒绝启动为设计。
- **NameHasOwner 元组**：D-Bus 回复体是 `(b)`，必须 `reply.get::<(bool,)>()`；假 watcher 的 `--state` 必须绝对路径，否则 `watcher_ready=false`。
- **面板锚点**：libappindicator 无托盘矩形，当前用光标位置（已知限制）；面板宽 360、高按内容测量。
- **零告警**：只允许按平台正确 cfg 或窄作用域 allow 并写明理由，禁止 crate 级一刀切 allow。
- **测试基线**：29 条是阶段 4–5 的真实工作，不是噪声；`oink-network` 两条是本机 closed-UDP 探测超过 1s。watchdog 竞态已修，回退即成新失败。不得 skip/删断言。
- **500 行**：`build-support/test_linux_dnd_scenarios.py:147-173` 维护探针名单；`linux_phase3_tray_probe.py` 恰 500 行。另外 `crates/oink-input/src/linux.rs` 561 行已超限，阶段 4 拆分解决。
- **前端预览**：headless Chrome `--virtual-time-budget` 下 smooth scroll 不完成，截图脚本直接设 `scroller.scrollTop`；`permissions` 分区为 mac-only，Linux 不出现属预期。
- **平台输入**：严禁向宿主 `DISPLAY=:0` 注入合成键鼠或 grab 真实键盘；合成输入仅限私有 Xephyr；真实桌面手测交用户。

## 8. 用户红线与禁止事项（继续生效）

- 不 sudo、不安装系统包、不改 CI/GitHub Actions；不 tag、不发 Release。
- **推送授权不延续**：本轮（2026-11-21）用户明确要求提交并推送阶段 3 收口与本文档；后续代理的提交、推送、合并、发版均须另行取得用户明确授权。
- 不记录真实按键内容、剪贴板/音频正文、凭证；只记录结构化状态。
- 保留红证据；不得把 F4（触摸 BLOCKED）/F5/F6 登记改写为 PASS；不以单测替代交互证据。
- 构建/原生限额 4 GiB、CPU 200%、TasksMax 1024；原生探针串行。
- 单文件 ≤500 行；零告警；不做简化实现；中文沟通；状态按层如实汇报。

## 9. 验收标准（DoD）与待用户确认项

阶段 4 完成条件（对齐 `plan.md` 第 4 节与 Verification）：

- 输入：生产抓取/回放实现，远控模式真正阻止本地交付；双向跨屏、连按/组合键/中文输入、滚轮/手势、相对模式、紧急恢复；断网/暂停/崩溃/锁屏解锁后无卡键与遗留抓取。
- 显示：屏幕枚举/排列/缩放/旋转/热插拔/负坐标事务正确，失败恢复并上报实际结果；壁纸读取与变化观察；多屏硬件不足则标记待验收。
- 反馈：拖尾/通知/投影真实显示、点击穿透、不抢焦点；中文字体真实渲染。
- 门禁：`cargo fmt`/`clippy -D warnings`/全量测试（29 条基线之外零新增）/前端 typecheck+test+build/离线单测全绿；阶段 3 探针回归不退化。
- 30 分钟混合负载稳定性；双机与真实桌面由用户执行；F4 触摸 BLOCKED 保持登记。
- 文档更新（计划实施记录 + 交接文档），提交与推送须获用户授权。

待用户确认/参与：Windows 双机配对与互通、真实 Cinnamon 会话手测、物理多屏/触摸硬件证据（触摸按 2026-11-20 决策登记 BLOCKED）。

## 10. 交接记录

| 日期 | 代理 | 范围 | 结果 | 产物 |
|---|---|---|---|---|
| 2026-11-17 | Qoder | 阶段 1–2 机制验证与 Qt 夹具 | 阶段性通过后按书面例外收口 | 第二至四轮交接 |
| 2026-11-20 | Qoder | 阶段 2 A2–F 分波 | 除触摸类外收口；F4 BLOCKED | 第四轮交接 |
| 2026-11-20 | Qoder | 阶段 3 C1–C3 | 完成并推送 `84a3b82`、`77a3602`、`72e22a2` | 代码 + `w1/`–`w3/` |
| 2026-11-21 | Qoder | 阶段 3 W0–W7：托盘、前端分支、零告警、基线、探针、渲染、文档 | 完成；C4 `9de8223` + 验收记录 `a2ab43c` 已推送；用户确认真实会话验收 | 代码 + 探针 + `w4/`、`w6/`、`w7/` + 第七/八轮交接 |
| — | 下一代理 | 阶段 4：输入/显示/原生可视反馈 | 未开始 | 本文件 |
