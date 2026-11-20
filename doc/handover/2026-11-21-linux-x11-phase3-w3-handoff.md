# Linux X11 阶段 3（W1–W3 完成）与 W4 待办 · 跨代理交接（第六轮）

日期：2026-11-21。仓库：`github.com/GitHubNull/oink`（私有）。分支 `main`，HEAD `72e22a2`，已推送 `origin/main`（本轮用户明确授权推送）。工作树仅本文件与 README 索引改动。

## 0. 一句话状态

阶段 2 已按用户书面例外关闭；阶段 3 的 W0–W3 已完成并推送（C1=`84a3b82`、C2=`77a3602`、C3=`72e22a2`）。**W4（Linux 托盘、前端 Linux 全分支走查、零告警全量门禁、文档收口、C4）尚未开始**。阶段 3 尚未整体完成，产品功能不得表述为“已支持”。

## 1. 任务表

| ID | 范围 | 状态 | 证据 / 下一步 |
|---|---|---|---|
| W0 | 阶段 2 书面例外收口（F4 BLOCKED、F5/F6 保持登记） | 完成 | 随 C1 入库：`doc/linux-x11/plan.md` 阶段 2 判定段 |
| W1 | Linux 编译启用 + UI 入口/启动骨架 | 完成 | `.release-tools/linux-probes/phase3-gate-20261120/w1/`（check 日志、Xephyr 截图） |
| W2 | 最小真实 X11 input/display 后端 + GLib 主循环 | 完成 | `.../w2/session3` 10/10 checks；红证据 `w2/diag1,diag2` |
| W3 | 关窗隐藏、二次启动重显、reopen、退出顺序、面板尺寸断言 | 完成 | `.../w3/session2` 14/14 checks；红证据 `w3/session1` 保留 |
| W3-测试 | UI crate 单测 + agent/input/display 全量套件 | 已跑，有存量基线失败 | UI rc=0；agent/input/display rc=101，24 个唯一失败名归档 `w3/session2/test-failures.txt`（详见 §4.3） |
| W4 | 托盘、前端 Linux 全分支、零告警、文档收口、C4 | **未开始** | 见 §6 |
| — | 面板交互验收 | 顺延 W4 | 私有 Xephyr 无 SNI 宿主，无法交互触发 `TogglePanel` |

### W3 交付内容（C3）

- `apps/oink/src-tauri/src/app.rs:523`：Linux `CloseRequested`（main）→ `api.prevent_close()` + `window.hide()`，成功时日志 `product window hidden on close`。共享不因关窗停止；二次启动/托盘可再唤出。
- `apps/oink/src-tauri/src/shell_policy.rs:195,204,208`：`normal_size`/`shadow_gutter`/`MAIN_MIN_WIDTH|HEIGHT`，含门禁断言测试 `product_window_sizes_match_the_native_constructor_contract`（Main 1040×680+24 gutter、min 820×560、About 360×500、Quit 480×320、Panel 360×520）。
- `apps/oink/src-tauri/src/shell.rs:313`：`create_normal` 改用上述策略值（行为不变，去内联 match）。
- `crates/oink-agent/src/backend/cli.rs:66`：`entry_name()` 单测钉住平台 reopen 入口（Linux→`oink`，Windows→`oink.exe`，macOS→后端可执行名）。
- `crates/oink-agent/src/backend/linux.rs:15`：会话分类测试拆为 X11/Wayland/NoDisplay 三例，均断言拒绝文案关键字。
- `build-support/linux_phase3_w3_probe.py`（289 行）：W3 门禁探针。

## 2. 接手先做的只读核验

```bash
cd /home/wgh/dev/cross_platform_app/oink
git log --oneline -4          # 期望 HEAD=72e22a2
git status --short            # 期望干净（本文件提交后）
ls .release-tools/linux-probes/phase3-gate-20261120/{w1,w2,w3}
jq .checks .release-tools/linux-probes/phase3-gate-20261120/w3/session2/summary.json
jq .checks .release-tools/linux-probes/phase3-gate-20261120/w2/session3/summary.json
```

## 3. 阶段 3 已完成行为（按层）

1. **入口/启动**：`apps/oink/src-tauri/src/main.rs:22` `oink_ui::service::dispatch_args()`；Linux `app.rs` 后台线程 `service::start_once` + 单次 `follow`，随后 `open_normal(Main)`；后端崩溃结束 UI、不自动重启。
2. **后台**：`oink-backend` = `crates/oink-agent/src/backend/main.rs`；Linux 实现 `backend/linux.rs:41 run()`（会话分类→`host::run_service()`）、`:59 on_main`（glib MainContext invoke）、`:76 run_main`（工作线程 + GLib MainLoop，照 `main_windows.rs` 范式）。
3. **单实例/所有权**：`crates/oink-agent/src/host.rs`；第二实例 `acquire_ui` 失败→`hand_off_to_running_ui`：Linux 走 `check_backend_ownership` + `api.show_window()`→后端广播 `ApiEvent::ShowWindow`→同进程 `shell::show_background/show_main`。W3 探针证实：第二次 `oink` rc=0、首实例同一 X 窗口重显。
4. **关窗/重开**：见 §1 W3 交付；`oink-agent reopen`（`backend/cli.rs` + `backend/launch.rs`）stop_service（IPC stop→UI exit(0)）→ `spawn_detached oink --reopen`；新 UI `wait_previous_pair` 后正常启动。W3 探针：旧 UI exit 0、旧 backend 消失、新 UI/新窗口/新 backend status 正常、无残留无僵尸。
5. **真实平台后端**（W2）：`crates/oink-input/src/linux.rs:255 start_loop`（真实生命周期+健康检查；`capture_available()=false` 如实上报）、`:327 set_mode`（记录不 grab）、`:336 inject`（XTEST）、`cursor_position`（XQueryPointer）；`crates/oink-display/src/linux.rs:62 displays`（XRandR XRRGetMonitors）、`:72 watch`（XRRSelectInput 监听线程）。完整输入语义（grab/回放/XKB/相对/拖拽）留阶段 4。
6. **前端平台值**：`apps/oink/src/App.tsx:81`、`apps/oink/src/settings/model.ts:87` 映射 `'linux'`；`pairing/*` 已识别 linux。W4 需逐窗口走查。

## 4. 已验证证据（命令、结果、路径）

证据目录均被 gitignore，仅本机磁盘存在。

### 4.1 门禁探针（私有 Xephyr，串行、限额内）

```bash
DISPLAY=:0 python3 build-support/linux-native-test.py --timeout 300 -- \
  python3 build-support/linux_phase3_w3_probe.py \
    --backend target/debug/oink-backend --ui target/debug/oink \
    --cli target/debug/oink-agent \
    --out .release-tools/linux-probes/phase3-gate-20261120/w3/session2
```

- W2：`w2/session3/summary.json` 10/10：direct status、duplicate exit=3、SIGTERM exit=0、无残留、UI 拉起后端并显示窗口、后端被 TERM 优雅退出、Wayland/无 DISPLAY 拒绝 exit=1。
- W3：`w3/session2/summary.json` 14/14：首窗 `IsViewable`→`wmctrl -ic` 关窗→`IsUnMapped` 且 UI/backend 存活；二次启动 rc=0 且同一窗口 `4194319` 重显；reopen 旧 UI exit 0、旧 backend pid 消失、新 UI/窗口/status 正常；`residual_after_all=[]`、`zombies=[]`。关键日志行：`ui-first.log` 的 `product window hidden on close`、`ready product window shown`、`backend subscription ended; exiting UI end=Stopping`。
- 红→绿：`w3/session1` 保留首轮红证据（`xdotool windowclose` 直接 XDestroyWindow→GTK `GdkWindow unexpectedly destroyed`+RenderBadPicture→UI 崩溃）。修复方式：用 EWMH 关闭请求（`wmctrl -ic`），等价 WM_DELETE。
- 截图：`connected.png`、`hidden.png`、`reshown.png`、`reopened.png`（各轮 xwd 存档同目录）。

### 4.2 本波相关单测（新鲜日志）

- `w3/session2/test-ui.log`：`cargo test -p oink-ui` rc=0；15 项单测 + 文档测试全过，含 `shell_policy::tests::product_window_sizes_match_the_native_constructor_contract`。
- `w3/session2/test-agent-input-display.log`：`cargo test --no-fail-fast -p oink-agent -p oink-input -p oink-display` 中新增/改动用例全过：`backend::cli::tests::reopen_entry_is_the_product_process_for_this_platform`、`backend::linux::tests::{x11_sessions_are_accepted, wayland_sessions_are_refused_even_with_an_x11_display, missing_display_is_refused}`；oink-agent lib 35 passed / 10 failed（剪贴板基线）。

### 4.3 全量套件存量失败基线（未裁定，W4 收口）

- 归档：`w3/session2/test-failures.txt`（28 条记录、24 个唯一用例名；watchdog 同名用例在多个测试二进制各失败一次）。rc=101。
- 分类：① 剪贴板依赖 10 条（`clip::fetch_tests`×2、`diagnostics::trace`、`snapshot_tests`、`two_nodes_tests::echo`×5、`pair_route_clipboard_layout_disconnect`）→阶段 5 才有后端；② 多节点/流集成 8 条（`link_drop_*`、`pause*`、`relay_*`、`restarted_backend_*`、`single_device_pairs_*`、`exit_child`）→依赖剪贴板/完整输入能力；③ DnD 3 条（`link_scenarios/dnd.rs` `Err(Unsupported)`）→阶段 4/5；④ watchdog `process_watchdog_bounds_a_blocked_executor` 7 条记录（limit=50ms 界限用例）→W4 需单独裁定（疑似 Windows 语义/时序依赖）。
- 与 W3 的关系：W3 对 `oink-agent`/`oink-input`/`oink-display` 的改动全部位于 `#[cfg(test)]` 测试模块，集成测试链接的非测试代码与 C2 相同，因此该失败集不可能由 W3 引入；但 W4 必须在最新 HEAD 重测并正式登记，**禁止 skip/删除断言过门禁**。

### 4.4 构建

```bash
cargo build -p oink-ui --features custom-protocol --bin oink   # UI 门禁二进制必须带 custom-protocol
cargo build -p oink-agent --bins                               # oink-agent CLI + oink-backend
```

## 5. 环境手册与已知坑

- 工具链：`rustc 1.99.0`（`/home/wgh/dev_env/rust/rustup/...`）；Node 24 / npm 11；Linux Mint Cinnamon X11；12 核 / 31 GiB。代理：本机 HTTP 代理 7890（cargo/git 慢时 `https_proxy`）。
- 原生测试入口 `build-support/linux-native-test.py`：必须普通用户、宿主 `DISPLAY=:0`、`systemd-run --user --scope` 限额（MemoryMax=4G、CPUQuota=200%、TasksMax=1024）。它建**私有 Xephyr**（xauth cookie、`xinput disable` 屏蔽宿主键鼠转发）、私有 D-Bus、私有 PipeWire/Pulse（唯一 null sink `oink_test`）、metacity；环境变量白名单固定，**不透传任意环境变量**（探针参数走 argv）。
- 坑 1：`xdotool windowclose` 直接 `XDestroyWindow`（out-of-band）会令 GTK 崩溃；关窗脚本必须用 `wmctrl -ic <hex id>`（EWMH `_NET_CLOSE_WINDOW`→metacity 发 WM_DELETE_WINDOW）。
- 坑 2：debug 版 `oink` 若未带 `custom-protocol` 特性，会从 `tauri.conf` devUrl `http://localhost:1440` 加载前端；无 dev server 时页面不执行 JS、窗口永不 reveal（W2 `diag1/diag2` 红证据）。门禁构建必须用上面 §4.4 的命令。
- 坑 3：私有 Xephyr 无 StatusNotifier watcher/托盘宿主；W4 托盘实现只能做代码路径验收（无 SNI 时警告且不 panic），真实 Cinnamon 托盘视觉验收需在用户真实会话启动一次应用供用户查看，**不得**向宿主注入输入。
- 坑 4：`xwininfo` 的 Map State 用于隐藏断言（`IsUnMapped`）；窗口 id 用十进制，`wmctrl` 用 `hex(int(id))`。
- 坑 5：`linux-native-test.py` 失败时保留现场（`/tmp/oink-native-*`），`--keep` 可保留；mount 残留会拒绝删除目录。
- 已知告警：oink-agent 22 条存量告警（清单 `phase3-gate-20261120/baseline-summary.txt`，含 oink-input/clipboard/audio locator 存量）；W4 统一清零并分列“本次引入 vs 存量”。
- 单文件 ≤500 行：新增文件硬门禁；`build-support/test_linux_dnd_scenarios.py` 维护名单，**W4 需把 `linux_phase3_shell_probe.py`、`linux_phase3_w2_probe.py`、`linux_phase3_w3_probe.py` 加入名单**并跑离线套件。

## 6. W4 待办（按已批准波次计划）

1. **Linux 托盘**：`apps/oink/src-tauri/src/native_controls.rs` 增加 Linux 分支——从 `windows_menu` 提取共享菜单构建，`TrayIconBuilder` + libappindicator；无 SNI 宿主→`tracing::warn!` 降级、UI 不受影响。面板（`shell.rs:202 create_or_close_panel`）在真实托盘下做打开/关闭/重复打开幂等验收。
2. **前端 Linux 全分支**：设置/配对/升级/关于窗口逐项走查；中文渲染截图核对；按平台隐藏 mac/win 专属项。
3. **全量门禁**：`cargo fmt --check`、`cargo clippy --locked --workspace --all-targets -- -D warnings`（存量告警清零）、`cargo test`（对 §4.3 基线正式裁定并更新登记）、前端 `typecheck`/`test`/`build`。单文件 500 行检查（含上述名单扩充）。
4. **文档**：`doc/linux-x11/plan.md` 追加阶段 3 实施记录（W1–W4 证据、告警基线、已知限制：托盘依赖 SNI、面板定位初版、输入能力边界）；更新本目录 README 索引；更新项目记忆。
5. **提交 C4**（本地）；推送需用户当轮明确授权（本轮 C1–C3 已获授权并推送）。不 tag、不发 Release、不改 CI。
6. 面板交互、`--autostart` 参数链路（仅验证传递，XDG autostart 注册留阶段 7）。

## 7. 用户红线与禁止事项（继续生效）

- 严禁向宿主 `DISPLAY=:0` 注入合成键鼠/grab 真实键盘；合成输入仅限私有 Xephyr。真实托盘验收由用户在真实会话查看。
- 不 sudo、不装系统包；不修改 GitHub Actions/CI；不 tag/Release。
- 任何 push 需用户明确授权（本轮已授权 C1–C3）。
- 不记录真实按键内容、剪贴板/音频正文、凭证；只记录结构化状态。
- 保留红证据，不覆盖历史批次；构建/原生限额 4 GiB、CPU 200%、TasksMax 1024；原生串行。
- 单文件 ≤500 行（新增）；零告警（存量 W4 清零，报告分「本次引入 vs 存量」）；中文沟通；不做简化实现；状态按层如实汇报。
- F4（触摸板/触摸/精细滚动，硬件不可得 BLOCKED）、F5（Chromium 合成拖拽起始，平台限制）、F6（Firefox 文件源，未实现）登记不得改写为 PASS。
- 不包含：剪贴板/文件/FUSE/DnD（阶段 5）、完整输入语义/音频（阶段 4/6）、XDG autostart（阶段 7）、打包/CI（阶段 8）、XWayland。

## 8. 验收标准（DoD）

- 阶段 3 完成 = W4 全部落地 + 全量门禁绿或基线正式登记 + 文档收口 + C4 提交（推送经用户授权）。
- 门禁口径：探针证据必须为私有 Xephyr 下的真实进程/窗口证据；单测不能替代交互证据；无法验证的项如实登记 BLOCKED，不做口头保证。

## 9. 交接记录

| 日期 | 代理 | 范围 | 结果 | 产物 |
|---|---|---|---|---|
| 2026-11-21 | Qoder | 阶段 3 W1–W3 + 第六轮交接 | W0–W3 完成，C1–C3 推送；W4 未开始 | 本文件；`phase3-gate-20261120/` 证据 |
