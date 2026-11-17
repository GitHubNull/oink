# Linux X11 阶段 2 交接：Qt 夹具验收后续作

日期：2026-11-17。仓库：`/home/wgh/dev/cross_platform_app/oink`。

**一句话状态：本机 Qt6/GTK 原生文本拖拽夹具及 GTK 异步 grab 清理修复已验收、提交并推送；可复现依赖获取入口和 B–F 仍未完成，阶段 2 未通过，产品阶段 3–8 未开始。**

本文件是第三轮交接，不覆盖前两轮快照。完整目标以 [Linux X11 计划](../linux-x11/plan.md) 为准。本文件已收录获批 A–F 的剩余范围与约束，接手不需要历史会话或仓库外的代理计划文件。

## 1. 基线、交付范围和先读结论

- 分支：`main`；代码提交：`2d2061a7c01c13e07e43afb15f9dd62ca808c7cd`。
- 提交主题：`test(linux): validate Qt X11 drag fixtures and asynchronous grab cleanup`。
- `origin`：`https://github.com/GitHubNull/oink.git`；`upstream`：`https://github.com/oinkvm/oink.git`。本次只推送 `origin/main`。
- 代码推送后 `git ls-remote origin refs/heads/main` 与上述 SHA 一致，编写本文前工作树干净。本文、索引与实施记录随后独立提交；文档提交号用 `git log -1 --format='%H %s' -- doc/handover/2026-11-17-linux-x11-phase2-qt-continuation.md` 查询，不在文件内循环记录自身 SHA。
- 本轮代码提交包含 19 个文件，范围为必要的 `build-support/`、`.gitignore` 与实施记录；`git diff --name-only d8b2932..2d2061a -- apps crates` 为空。没有产品、锁文件、CI 或系统权限变更。
- 用户此次明确授权提交并推送代码及交接交付；这不是后续代理持续提交、推送或发布的授权。没有版本号、tag、Release 或安装包交付。
- 本机 Rust 实查 `1.99.0`；仓库 Node 实查 `v24.14.1`。会话系统 Node 为 `v24.22.1`，不可冒充仓库工具链。

**与旧交接不同，务必以本文件为准：**

1. 不再要求本机安装全局 Qt5/XRes 开发包：已使用项目内 Qt6 6.12.0 kit 和 XRes sysroot。
2. Qt 原生夹具已实现并验收，但 Qt 文件/目录/INCR 与真实应用矩阵没有实现。
3. 新报告字段 `toolkit_grabbed` 已追加到协议；旧 role 二进制不能与新控制器混用，必须重建。
4. `input-matrix` 构建必须链接 `-lXau`；旧交接示例遗漏该参数。
5. 构建也必须资源限额；不要照搬旧交接的裸编译器命令。
6. `.tools/`、`.release-tools/` 都被忽略，依赖、二进制、日志、截图不在 GitHub；fresh clone 不是即刻可复现环境。
7. B–E 含大量尚未编写的软件，不得把总缺口描述为“只等用户硬件”。

## 2. 任务总表与实施顺序

顺序保持 **A 收尾 → B 隔离夹具/文本 → C → B 完整矩阵 → D → E → F**。FUSE 日志可提前只读核验；所有原生运行串行。

| ID | 范围 | 当前状态 | 优先级/规模 | 下一步与完成证据 |
|---|---|---|---|---|
| A1 | 本地 Qt6/XRes 依赖检查 | 已完成 | 保持回归/小 | 本地优先、缺失回退、不完整失败；27 项依赖测试 |
| A2 | fresh clone 固定版本依赖获取 | 未实现 | P0/中 | 独立获取并验证 `.tools`，不读 snow-apps，不改 CI/全局环境 |
| A3 | 六个 C 产物与原机制基线 | 已完成 | 保持回归/中 | 当前源码新鲜构建、七套原生基线与清理汇总 |
| A4–A5 | Qt 独立进程原生文本矩阵 | 本机已完成 | 保持回归/中 | 六个混合 Drop/Escape、两个身份拒绝、七个协议负例 |
| A 回归修复 | GTK 异步 grab 缓存 | 已完成 | 保持回归/中 | 保留强断言；五轮各八场景、四项新增离线用例 |
| B | Nemo/Chrome/Firefox 隔离与身份、完整矩阵 | 仅只读准备，未实现 | P0/大 | 新进程归属、XRes/epoch 身份链、真实源/目标应用证据 |
| C | 文件/目录、分块桥、真实 INCR | 未实现 | P0/大 | 独立远端 staging、至少 1 MiB 真 INCR、哈希与故障矩阵 |
| D | 新源 generation、peer/controller 恢复 | 未实现 | P0/大 | 受控故障两代真实交付，拒绝旧身份，回收无残留 |
| E | 专属 master 到真实 toolkit 输入 | 未实现 | P0/大 | 实际应用回调/可信事件，普通输入对照，设备边界 |
| F1 | FUSE 普通用户验收 | 尚无可读约定日志 | 必需/用户步骤 | 先查已有证据，再按 §7.5 交用户执行 |
| F2–F3 | 物理输入/锁屏/真实麦克风 | 缺完整入口和用户证据 | 必需/软件+用户 | 先完善安全入口，再交用户操作；不可伪造通过 |
| F4–F5 | 全量回归与阶段 2 总门槛 | 未通过 | 必需/中 | 逐项 PASS/BLOCKED/FAIL，任一必需缺证据不得进入阶段 3 |
| 产品 3–8 | Linux 产品、包装、Windows 双向互通 | 未开始 | 后续/很大 | 见 §9，不在本次机制续作中提前实现 |

## 3. 已完成代码与不应倒退的契约

### 3.1 本地依赖

- `build-support/linux-deps.sh`：`--toolkit-tests` 优先 `.tools/Qt/6.12.0/gcc_64` 和 `.tools/sysroot`；允许 `OINK_QT_KIT`/`OINK_XRES_SYSROOT` 显式覆盖。路径不存在才可回退系统 `qtbase5-dev`/`libxres-dev`，存在却不完整必须失败。
- Qt kit 来自用户批准的独立拷贝，约 1.7 GiB/11037 文件；不再引用另一项目。XRes 开发内容来自 `libxres-dev 2:1.2.1-1build1`。
- `.tools/sysroot/usr/lib/x86_64-linux-gnu/libXRes.so` 当前指向 `/usr/lib/x86_64-linux-gnu/libXRes.so.1.0.0`；它不是完全自带的运行时。系统 `libxres1` 和 Qt xcb 运行库仍须检查。
- 本机系统有 Qt6 6.4.2，kit 为 6.12.0；kit 自带 ICU73，已验证 glibc 符号上限 2.34 低于本机 2.39。不能据此宣称任意发行版兼容。
- `linux_dnd_qt_build.py:22` 严格锁定 6.12.0；通用依赖门禁能识别 Qt6 不等于任意 Qt6 能通过该 builder。
- 官方 Qt pc 文件带 `/home/qt/work/install` 前缀；builder 只在 Qt 查询使用 `--define-variable=prefix=<kit>`，不污染系统 X11 pkg-config 查询。
- RUNPATH 指向本地 kit，隔离环境不继承全局 Qt/动态库路径；`linux_dnd_qt_scenarios.py:48` 另查实际 `/proc/<pid>/maps`，要求 Qt6 库和 xcb 插件真实路径属于 kit，拒绝 `<kit>-foreign` 目录前缀碰撞。Qt 没有加入 Tauri 产品。

### 3.2 角色协议、Qt 生命周期与 GTK 清理

| 入口 | 当前作用与约束 |
|---|---|
| `build-support/linux_dnd_wire.h:6` | GTK/Qt 共享字段与既有 18 个 opcode，字段末尾追加 `toolkit_grabbed` |
| `build-support/linux_dnd_protocol.py:11` | 控制帧上限 2048、正文上限 128；非阻塞 seq/generation 和私有环境，不可直接膨胀为大 selection |
| `build-support/linux_dnd_native.h` / `.c:115` | GTK 无关原生钩子；FREE 先 toolkit 检查，再独立 pointer/keyboard grab 成功检查并立即释放观测 grab |
| `build-support/linux_dnd_roles.c:65`、`:82` | GTK 报告真实 GDK grab 缓存；原 `GTK released grab` 断言保留 |
| `build-support/linux_dnd_scenarios.py:144` | End 后有界等待真实 `toolkit_grabbed=0`，再执行原 FREE；取消时也不能提前 UP 来促成释放 |
| `build-support/linux_dnd_qt_probe.cpp:24` | armed/source/实际 selection owner/私有 generation 强校验；可见 QWidget XID 不等于真实临时 owner |
| `build-support/linux_dnd_qt_probe.cpp:124`、`:148` | 自发 Qt 鼠标事件进入 QDrag，COPY 在真实 XdndFinished/协商目标/selection 读取证据成立后 End |
| `build-support/linux_dnd_qt_control.cpp` | Qt 事件循环内非阻塞控制、有限缓冲/背压、EOF/QUIT 与自有按钮清理 |
| `build-support/linux_dnd_qt_scenarios.py:58`、`:94`、`:115` | 混合角色矩阵、身份拒绝、坏控制帧；正常角色须 QUIT/exit0/零 stderr |
| `build-support/test_linux_dnd_cleanup.py`、`test_linux_dnd_qt.py` | 时序、强断言、构建路径、maps 归属、矩阵与协议负例回归 |

Qt COPY 的关键坑：`QDrag::exec()` 返回可能早于真实 `UTF8_STRING` SelectionRequest。不得在 exec 返回后删除仍被 Qt 管理的 QDrag 或立即结束角色；当前以 XdndFinished 加已协商 COPY/target、实际读取和源正文不变确认 End。Escape 另记取消，不能伪造成功。

GTK 的关键坑：`drag-end` 和 XSync 均不是 GDK 异步 ungrab 缓存完成屏障。失败实证记录为 `end=1 button=1 server_grab=0 cache_before=1 cache_sync=1 cache_dispatch=0 rounds=3`。其中 `server_grab=0` 是独立 XIGrab 成功并立即释放的主动观测，不是只读查询。最终修复报告真实 toolkit 状态并有界等待，不主动 ungrab GTK、不提前 UP、不固定盲睡、不嵌套泵事件；超时、缺字段或原生 grab 失败仍失败关闭。

本机文本矩阵采用私有随机 48 字节载荷，校验完整 SHA-256、长度/FNV、源删除为零和 CLIPBOARD 哨兵所有权/内容不变。一次源拖拽只许一次 Down/Begin/End；回源再进入保持原 context/grab，重复 Down 被拒绝。既有八个 GTK 场景保留。

**覆盖边界：** DnD 用的是私有服务器 core XTEST，不是 E 的专属 master 到真实 toolkit 交付；双端是 selection→catcher→测试桥→proxy→target 的机制桥，不是产品网络；128 字节协议不支持当前尚缺的文件/大 selection/INCR。GTK 场景脚本中的 Qt 未覆盖提示只描述该脚本，新增 Qt 覆盖在独立套件中。

## 4. 已验收证据与尚未验收项

以下均为本机 `.release-tools/linux-probes/` 下的本轮证据，不随 git 推送。换机器必须重新准备依赖、构建及复跑，不把本地日志路径当作 GitHub 产物。

| 证据 | 实际结果 |
|---|---|
| `verification-wave-a3.log`、`run-status-wave-a3.log` | 六个 C 新产物 mtime/SHA-256、纯校验器及提取前七套原生基线 |
| `native-build-wave-a3.log`、`input-matrix-build-wave-a3.log` | 严格构建，保留最初缺 Xau 的失败；修正链接后通过 |
| `input-wave-a3.log`、`dnd-wave-a3.log`、`input-matrix-wave-a3.log` | 原输入、GTK、26 项 core+XI2 观察窗输入矩阵通过 |
| `audio-wave-a3.log`、`audio-route-wave-a3.log`、`audio-restart-wave-a3.log` | 原音频、独立播放支路/需求门控、两代服务恢复通过 |
| `dnd-all-wave-a3.log` | 提取前 GTK 八场景通过 |
| `qt-matrix-wave-a-first.log`、`qt-selection-state.log` | Qt exec 过早结束的红证据与实际选择请求时序 |
| `qt-plugin-red.log` | xcb 插件目录前缀碰撞红测试 |
| `dnd-all-after-qt.log`、`gtk-all-dnd-roles-qt-wave-2.log` | GTK 完整序列先后在目标退出和双端取消中复现，不是只看重跑绿灯 |
| `gtk-grab-diagnostic-4.log` | GTK 异步缓存根因实证，原失败仍保留 |
| `gtk-cleanup-red.log`、`gtk-cleanup-green.log` | 四项新增回归先红后绿；聚焦 DnD 47 项通过 |
| `gtk-build-cleanup-final.log`、`qt-build-cleanup-final.log` | 最终 GTK/Qt 当前源码严格零告警构建，mtime 晚于源码 |
| `gtk-cleanup-final-1.log` 至 `gtk-cleanup-final-5.log` | 连续五轮各八场景，共 40 次；未删原负面用例 |
| `qt-matrix-wave-a-final.log` | Qt→Qt、Qt→GTK、GTK→Qt 各 Drop/Escape 六场景，加两个身份拒绝、七个坏控制帧；九组正常进程退出证据含截图组 |
| `linux-unit-wave-a-verified.log` | 完整 Linux Python 210 项，33.821 秒，OK/exit0 |
| `deps-wave-a-verified.log` | 本地 Qt6 6.12.0、XRes1.2.1 门禁通过 |
| `qt-fixture-final.png` | 已实际查看的私有夹具截图，仅隔离蓝色源/绿色目标；不是产品 UI 验收 |

最终五份 GTK 日志和一份 Qt 日志均逐一验证唯一 `native_test_summary`，`probe_exit=result=remaining_mounts=0`，`services_ok/cleanup_ok/mount_check_ok=true`。提取前七套基线也核验这些字段，不能只引用进程 exit0。

最终角色二进制：

- `dnd-roles-qt-wave`：SHA-256 `31cd3061057a74a00f547a163221c82742be5743f0eb61d7e0d19409a2debce2`。
- `dnd-qt`：SHA-256 `7ad632f977e0c12e7ba56f4a5db7302bd58ed53680bba449a6401e738d2b5d26`。

历史失败目录/诊断二进制和日志保留，不当作当前验收程序。临时源代码诊断和临时调查脚本已移除。

音频边界仍保留：本机 pw-cat 存在 Pulse 镜像，`native_only_evidence=not_exercised_pulse_mirrored`，未镜像 native-only 仅离线覆盖；私有合成 RMS、路由/重启不是用户真实麦克风或产品虚拟输入验收。宿主能力历史只读见 `host-device-capabilities.log`，没有触点类不等于 Linux 不支持触点。

## 5. 下一代理必须完成的 A–F

### A2：独立依赖获取入口（未实现）

为 fresh clone 提供独立固定版本获取脚本，准备并验证项目内 Qt 6.12.0 和兼容 XRes 开发 sysroot；不引用 snow-apps、不提交 1.7 GiB kit、不把系统 Qt5 回退冒充 Qt6 夹具，不修改 CI。明确来源、版本、完整性、依赖运行库、失败清理和重复运行行为。取得来源后验证获取脚本及新目录的 qmake/编译/实际加载路径，再称其可复现；当前本地拷贝不满足此项。需系统新增依赖时整理用户安装步骤，不代 sudo。

### B：真实应用隔离、身份与源目标矩阵（尚无实现）

拟新增 `build-support/linux_dnd_app_scenarios.py`、本地浏览器页面及分职责应用观测/身份模块，扩展现有角色/selection/protocol 接口。只读已确认 `/usr/bin/nemo`、`/usr/bin/google-chrome`、`/usr/bin/firefox` 存在，Chrome 命令解析到 `/opt/google/chrome/google-chrome`；没有启动或验收真实应用。XRes.h 提供 `XResQueryClientIds`/`XResGetClientPid`，但仓库没有已实现的该身份链；`Session` 也没有 D 所需 supervisor epoch。

1. Nemo 使用私有 DBus/HOME/临时文件树，Chrome/Firefox 使用各自临时 profile，仅访问本地测试页面，不向外上传文件。证明新进程、私有 display 和窗口归属，防止转交宿主实例；禁止 `--no-sandbox` 等沙箱绕过。
2. 保留 GTK/Qt 私有 generation 强校验。仅为本次启动并登记的真实应用增加独立关联：XRes 服务器侧客户端 PID + 进程启动身份/子进程归属 + server epoch + 实际 XDND source/selection owner + 本地拖拽 generation/context。
3. 不凭标题、`_NET_WM_PID` 或缺失私有属性放行；临时 owner 也须证明属于应用。进程退出、owner 更换、旧 epoch/XID 复用立即失效，证据不足 BLOCKED。
4. 先验启动/可观测性/文本；C 完成后补文件/目录。GTK、Qt、Nemo、Chrome、Firefox 逐行记录源/目标、格式、Drop/回源/Cancel、哈希、源保持、清理。
5. 浏览器用真实 DOM drag/input 事件及可信性，Nemo 用实际文件/窗口变化与原生 XDND，GTK/Qt 用原生回调；应用不提供的事件不能填造数值。不接受格式时记录实际契约/限制，不直接算 PASS。

**验收：** 逐行 PASS/BLOCKED/FAIL + 可定位证据；外来 owner、迟到 generation 和退出后身份复用负例继续通过。原生/沙箱/XRes 缺证据不得跳过该应用。

### C：文件/目录、有界分块桥、真实 INCR（尚无实现）

拟扩展 `linux_dnd_roles.c/.h`、`linux_dnd_selection.c`、`linux_dnd_protocol.py`，新增 `linux_dnd_incr.c`、文件/分块模块及独立测试；超过 500 行前按职责拆分，不往临界文件堆代码。

1. MIME 协商和 `text/uri-list`，私有源文件/目录；目标实际打开、按 offset 读取、遍历并验证大小/SHA-256，覆盖名称编码、路径逃逸和符号链接边界；源不删除，CLIPBOARD 哨兵不变。
2. 唯一正文通路：source selection → catcher 实读 → 测试桥 → proxy selection → target 实读。不得向 target 控制管道预置正文。文件必须源端实读后进入远端私有 staging，由 proxy 发布远端本地 URI，不能把共享路径可读当跨端交付。
3. 保留控制帧有界；新增分块帧、transfer-id、epoch/generation、offset、总量上限/背压，公平调度控制与正文，不增加输入注入通道，不塞巨型 ASCII 控制帧。
4. 真异步 INCR：类型公告 → 属性删除握手 → 多块顺序读取 → 零长度终止；明确阈值强制至少 1 MiB 走 INCR，不能只看正文够大。
5. 检查完整哈希、分块数/字节/offset；超限、乱序、owner 退出、取消、超时、陈旧代际失败关闭。慢传输控制 ACK <500ms。

**验收：** 原生 INCR 握手和终止实证、文件实际读取与隔离 staging、负面矩阵、期间控制响应与清理全部成立。500ms 是探针控制预算，不替代产品 3 秒 preparation/5 秒 ordinary-input 过期；桥仍不是产品网络。

### D：新源 generation 与 peer/controller 恢复（尚无实现）

拟新增 `linux_dnd_recovery.py` 及测试，扩展角色启动代际和退出清理；按需提取 Session 小模块，不降低外层运行器断言。

1. 存活 supervisor 分配源/server/controller epoch 与 generation，新源首次 Down 前登记；旧命令/回调/分块/清理不能影响新代。
2. 不杀外层监控服务后忽略失败。用既有 Session 机制在存活私有主 X server 上创建、登记并拥有故障用嵌套 Xephyr；独立根/Xauthority、同一资源限额。旧 peer 完全回收后才起新代，不叠多套故障环境。
3. 登记预期故障的精确进程/代际、退出原因和清理证据；其他服务死亡仍 FAIL。每代重新验证 display、认证、连接，拒绝旧描述/owner/XID/迟到消息。
4. controller 故障发生在独立子 controller，不是外层 runner/主代理；监督者不得保留写端掩盖 EOF。验证 EOF、角色退出、自有按钮释放/回收后才新建。
5. 覆盖源重启、peer 重连、controller 恢复、旧清理试图结束新拖拽、新代启动失败。旧代明确取消，不宣称原 context 能跨进程复活。

**验收：** 两代真实 selection 交付；旧身份拒绝、新代不受迟到消息影响；无 grab/按键/进程/FD/挂载残留，外层 `require_alive` 保持。

### E：专属 master 到真实 toolkit 输入与设备边界（尚无实现）

复用 `linux_input_matrix*.c/.h` 的私有 master 能力，拟新增 `linux_input_toolkit_scenarios.py`，复用 A/B 应用夹具。

1. 专属 master + 自建 XTEST 的 ClientPointer/焦点绑定应用实际客户端/窗口；保留普通输入对照，不是仅向观察窗注入。
2. GTK/Qt 原生事件、浏览器可信 DOM 事件/真实界面变化、Nemo 可访问性/实际导航滚动为应用侧证据；raw/core/XI2 计数只佐证来源。
3. 按键、左右修饰键、AltGr、重复、媒体键可达性、相对位移/双向滚轮；普通输入不误吞。WM/应用未交付如实分类，不拿系统动作冒充应用事件。
4. 只读采集 XIScrollClass/XITouchClass/XIGestureClass，分别报告 Xephyr 与宿主；物理 HID、精细滚动、触摸板/触点、锁屏/切换交用户。无设备不等于平台不支持，快捷键不冒充触点。
5. 保留既有产品 contacts `0x020b` 网络契约；本阶段不声称已验证 Linux 产品触点端到端。

**验收：** 实际应用使用和事件证据、普通输入对照、拒绝/未交付边界、设备能力与用户实证明确分层。现有 26 项观察窗输入不能替代本项。

### F：用户门槛、完整回归与收口

1. 先只读复查 `.release-tools/linux-probes/fuse-user.log`。目前无可读取文件不代表用户没操作；已有有效证据直接核验，不要求重复。仍缺时新鲜构建 FUSE 后把 §7.5 命令交用户普通终端，代理不挂载。
2. 先实现短步骤和仅数值/状态硬件记录入口，必要时新增 `linux_hardware_acceptance.py` 与独立测试；物理输入/锁屏由用户操作，不自动监听宿主键盘、不改变运行器禁用宿主转发策略。
3. 真实麦克风须用户主动启用、真实应用选择虚拟输入；复用需求/许可门控，验启采/cork/最后消费者退出/撤销/服务恢复。只留 RMS/计数/启停状态，不保存音频正文，不用合成音频冒充。
4. 用户说完成后代理主动取约定日志/非敏感证据；不要求用户描述现象或重做已完成步骤。必需项无硬件/权限保持 BLOCKED，软件未写单独列明。
5. 每波追加命令、exit、日志、覆盖/缺口到长期计划。新交接按需新建并更新索引，不覆盖旧快照。

## 6. 安全边界与禁止事项

- 合成输入仅私有 Xephyr；严禁向宿主 `DISPLAY=:0` 注入、grab 真实键盘或接用户浏览器 profile/登录态。仅 runner 为嵌套窗口连接宿主，探针获得 runner 创建的私有 display。
- 私有 HOME/XDG/Xauthority/DBus/Pulse/PipeWire、认证、进程归属、超时和失败保留必须保留。不能绕过 `--inside-scope`、`require_alive`、清理和挂载检查。
- 构建/原生运行限额 4 GiB、CPU200%、Tasks256；原生串行，不并发多组桌面/音频服务，不以不限额重试处理卡顿。
- 系统安装/FUSE/物理设备/锁屏/麦克风交用户；不 sudo、不重试代理 FUSE、不启用 helper/uinput、不改用户组/udev/防火墙/X server 权限。新权限方案先说明并申请。
- 阶段 2 只改必要 `build-support/` 与实施记录；不动产品、工具链锁、CI。必要忽略项有明确依据，不改全局 Qt/Rust/Node，不修改另一项目。
- 新增/拆分源码 ≤500 行，不往接近上限测试文件堆代码、不无关重构；保留原用例和负面/bait 文件，不 skip/删断言过门禁。
- 不记录真实按键、剪贴板/音频正文、凭证；诊断仅测试随机数据的哈希/数值/状态，截图只取私有夹具。
- 不把一次重跑成功抹掉已出现失败。保留红证据，定位根因，补回归再完整运行；保留历史诊断文件但不拿旧二进制验新协议。
- 500ms 仅探针控制预算；3s/5s 产品语义保持。测试桥不冒充产品网络、观察窗不冒充真实应用、合成设备不冒充硬件。
- 后续 commit/push/tag/release、Windows 安装或发布操作须新的明确授权；此次授权不能延伸为长期授权。不要修改远端配置或跳过 hooks。

## 7. 环境、构建与复跑手册

### 7.1 入口和只读检查

以下命令在仓库根目录、同一个 Bash 会话执行。使用现有用户会话连接 Xephyr；不要人为把探针 DISPLAY 指向宿主。脚本缺依赖时停下，不自动安装系统包。

```bash
cd /home/wgh/dev/cross_platform_app/oink
export PATH="$PWD/.release-tools/node-v24.14.1-linux-x64/bin:$PATH"
git status --short --branch
rustc --version
node --version
bash -n build-support/linux-deps.sh
bash build-support/linux-deps.sh --check --native-tests --toolkit-tests
git diff --check
```

下载需要时显式使用本地代理 `http://127.0.0.1:7890`，保留 `no_proxy=localhost,127.0.0.1`；仅获取授权依赖，不上传正文。Qt/XRes 环境只用于探针，不加入产品构建。fresh clone 缺 `.tools` 时先完成 A2，不盲目重拷其他项目。

### 7.2 当前源码重建（六个 C 产物和 Qt）

下面 `handover-rebuild` 必须是新目录名，已存在就换批次名，不能覆盖历史红/绿证据。构建失败即停，不用旧二进制继续跑。`bounded` 仅当前 shell 函数，不是已有仓库脚本。

```bash
set -euo pipefail
mkdir -p .release-tools/linux-probes
out="$PWD/.release-tools/linux-probes/handover-rebuild"
mkdir "$out"
bounded() {
  systemd-run --user --scope --quiet -p MemoryMax=4G -p CPUQuota=200% -p TasksMax=256 \
    timeout 180 "$@"
}
bounded cc -std=c11 -O2 -Wall -Wextra -Werror -o "$out/input" \
  build-support/linux_input_probe.c $(pkg-config --cflags --libs x11 xi xtst)
bounded cc -std=c11 -O2 -Wall -Wextra -Werror -o "$out/dnd" \
  build-support/linux_dnd_probe.c $(pkg-config --cflags --libs gtk+-3.0 x11 xi xtst)
bounded cc -std=c11 -O2 -Wall -Wextra -Werror -o "$out/fuse" \
  build-support/linux_fuse_probe.c $(pkg-config --cflags --libs fuse3)
bounded cc -std=c11 -O2 -Wall -Wextra -Werror -o "$out/dnd-roles" \
  build-support/linux_dnd_roles.c build-support/linux_dnd_native.c \
  build-support/linux_dnd_selection.c $(pkg-config --cflags --libs gtk+-3.0 x11 xi xtst)
bounded cc -std=c11 -O2 -Wall -Wextra -Werror -o "$out/input-matrix" \
  build-support/linux_input_matrix.c build-support/linux_input_matrix_events.c \
  build-support/linux_input_matrix_keys.c build-support/linux_input_matrix_pointer.c \
  $(pkg-config --cflags --libs x11 xi xtst) -lXau
bounded cc -std=c11 -O2 -Wall -Wextra -Werror -o "$out/input-matrix-test" \
  build-support/linux_input_matrix_test.c
bounded python3 -B build-support/linux_dnd_qt_build.py --output "$out/dnd-qt"
bounded "$out/input-matrix-test"
sha256sum "$out/input" "$out/dnd" "$out/fuse" "$out/dnd-roles" \
  "$out/input-matrix" "$out/input-matrix-test" "$out/dnd-qt"
```

同时记录每个构建 exit、产物 mtime 晚于全部源码/头文件输入、完整日志和 SHA-256。Qt builder 使用 C11/C++17、`-O2 -Wall -Wextra -Werror`、`QT_NO_KEYWORDS`、PIC、本地 RUNPATH，链接成功后才用 `.partial` 替换目标。禁止仅看有旧产物就开始验收。

### 7.3 纯测试与原生串行回归

```bash
bounded python3 -B -m unittest discover -s build-support -p 'test_linux_dnd*.py' -v \
  > "$out/dnd-unit.log" 2>&1
bounded python3 -B -m unittest discover -s build-support -p 'test_linux*.py' -v \
  > "$out/linux-unit.log" 2>&1
python3 -B build-support/linux-native-test.py --timeout 60 -- "$out/input" > "$out/input.log" 2>&1
python3 -B build-support/linux-native-test.py --timeout 60 -- "$out/dnd" > "$out/dnd.log" 2>&1
python3 -B build-support/linux-native-test.py --timeout 60 -- "$out/input-matrix" > "$out/input-matrix.log" 2>&1
python3 -B build-support/linux-native-test.py --timeout 120 -- \
  python3 -B build-support/linux_audio_probe.py > "$out/audio.log" 2>&1
python3 -B build-support/linux-native-test.py --timeout 180 -- \
  python3 -B build-support/linux_audio_route_probe.py > "$out/audio-route.log" 2>&1
python3 -B build-support/linux-native-test.py --timeout 120 -- \
  python3 -B build-support/linux_audio_restart_probe.py > "$out/audio-restart.log" 2>&1
python3 -B build-support/linux-native-test.py --peer-x11 --timeout 150 -- \
  python3 -B build-support/linux_dnd_scenarios.py --roles "$out/dnd-roles" --suite all \
  > "$out/dnd-all.log" 2>&1
python3 -B build-support/linux-native-test.py --timeout 150 -- \
  python3 -B build-support/linux_dnd_qt_scenarios.py --gtk "$out/dnd-roles" \
  --qt "$out/dnd-qt" --kit "$PWD/.tools/Qt/6.12.0/gcc_64" > "$out/qt-matrix.log" 2>&1
```

新功能先跑其聚焦测试，再全 Linux 套件；上例 DnD 聚焦不是未来所有模块的通用替代。当前原生 runner 自动进入用户 scope，不能绕开其私有环境。先检查真实退出码；每份原生日志必须且只能有一条 `native_test_summary`，且 `probe_exit=result=remaining_mounts=0`、`services_ok/cleanup_ok/mount_check_ok=true`。再查场景计数、角色退出与正文哈希等业务断言；恢复场景额外核验受控故障每一代，不只看最终汇总。纯测试日志不应套原生汇总要求。

改共享钩子或清理时重复完整 GTK 序列，不能只复跑曾失败的单场景；本轮单场景六次全绿，完整序列仍复现过竞态。UI 只在私有桌面实际操作和取证，不用 DOM 合成 drag/key 冒充 X11 交付。

### 7.4 已知运行环境坑

- Xephyr virtual keyboard/mouse 的宿主转发已在私有 display 内被 runner 禁用；不得重新开启来“方便测试”。
- 新 X 客户端可能没有 ClientPointer；输入矩阵已有 `XQueryPointer` 初始化后再查询的处理，不无故撤销。
- PipeWire PID 在 `client.id` 指向的 Client 而非 Node；同快照 Node→Client→自有 PID 关联，歧义失败关闭。
- `pactl -f json list modules` 不含 index，清理用 short modules；source RUNNING 不代表有消费者，cork/active links/身份必须核验。
- sink mute 会静音 monitor；保捕获静音依赖独立播放支路，不改回简单 mute。
- 私有音频服务恢复创建第二套自有实例，不杀外层服务，不放宽外层存活断言。
- 异常退出日志、失败目录和本机 ignored 产物要保留；不能用删除证据消除失败。

### 7.5 FUSE：仅交用户普通终端执行

先读既有约定日志；仍缺时仅安全重建，不代理挂载。若使用 §7.2 的新批次产物，把下面二进制参数改为已核验新鲜度的该绝对路径；不能因老路径存在就复用老产物。批准计划约定命令如下：

```bash
python3 /home/wgh/dev/cross_platform_app/oink/build-support/linux-native-test.py --timeout 12 -- \
  /home/wgh/dev/cross_platform_app/oink/.release-tools/linux-probes/fuse \
  > /home/wgh/dev/cross_platform_app/oink/.release-tools/linux-probes/fuse-user.log 2>&1
```

代理环境的 `NoNewPrivs=1` 限制 setuid fusermount3 是既有阻塞，不是换工具重试的理由。用户执行后代理读日志，核验 `content_and_offset=ok`、写入 EROFS、卸载成功、唯一汇总通过、无挂载/进程残留；已有有效文件不要覆盖或要求重做。

## 8. 产品代码真实现状（本轮复核，不是待修的几个编译点）

下列行号针对代码基线 `2d2061a`；后续实施前重新读当时代码。

| 当前证据 | 真实缺口 |
|---|---|
| `apps/oink/src-tauri/src/main.rs:37–45` | 非 macOS/Windows 分支打印“界面只支持 macOS 与 Windows”并失败；Linux UI 生命周期未接通 |
| `crates/oink-input/src/lib.rs:440–446` | Linux 平台工厂返回 Unsupported |
| `crates/oink-display/src/lib.rs:105–111` | Linux 平台工厂返回 Unsupported |
| `crates/oink-clipboard/src/lib.rs:360–366` | Linux 平台工厂返回 Unsupported |
| `crates/oink-audio/src/lib.rs:255–278` | Linux capture capability 为 unsupported，仅占位 Capture |
| `crates/oink-agent/src/backend/mod.rs:12–24` | 已有 Windows/macOS 专属后台路径；不能把探针 Session 当产品 Linux 后台 |

`apps/`、`crates/` 自 `d8b2932` 至本轮代码提交无改动；此前 locator 等 Linux 编译问题没有在本轮修复。上述只是代表性证据，不是产品缺口全集，不得简化成扩大 cfg 或修 locator 后就宣称 Linux 支持。

## 9. 阶段门槛、完整后续范围与接手步骤

阶段 2 的必需条件：Qt/真实应用源目标矩阵、文件目录/INCR、代际恢复、真实 toolkit 输入、音频回归、FUSE 和用户硬件均有证据；任一必需项 FAIL/BLOCKED/缺证据都不能进入阶段 3。平台不适用须有明确依据，不能重新分类降低契约。依赖获取入口亦须如实收口，不把当前机器拷贝算可复现交付。

阶段 2 通过后，重新核对当时代码，按长期计划阶段 3–8 继续完整产品范围：

- Linux Tauri/GTK 生命周期与后台工厂、单实例/ownership/IPC/托盘/关闭重开。
- 输入、显示、定位、通知/投影；不能仅让 Rust 编译通过。
- 剪贴板、Namespace/FUSE、文件与原生 DnD，以及真实产品网络路径。
- libpulse 音频与虚拟麦克风、恢复和权限/需求策略。
- 协议/平台枚举、设置/语言、XDG 自启动、崩溃/诊断、本地更新。
- 本地 DEB/可解压包，不依赖源码目录或开发服务器；安装/升级/卸载。
- Windows 双向互通与 30 分钟混合负载，无卡键、幽灵 grab、音频回环或声音不恢复。Windows 安装与发布另行授权。

下一代理的第一轮：

1. 读本文件和长期计划，核验 HEAD/status/本机 kit，不重复旧的全局 Qt5 安装要求。
2. 只读查 FUSE 约定日志；没有就保留缺证据，不代理挂载。
3. 按资源限额新鲜重建并复跑基线；红灯先定位，不携带旧绿灯进入下一波。
4. 完成 A2，再按 B 隔离/文本→C→B 完整矩阵→D→E→F 推进。每项先聚焦测试再全量；真 UI 仅私有环境。
5. 每波把完成/未完成、命令、exit、日志追加到长期计划；不提前产品化、不未经授权提交或发布。

## 10. 交接与完成记录

| 日期 | 代理 | 范围 | 结果/产物 |
|---|---|---|---|
| 2026-11-17 | Qoder | 第三轮 A 本机 Qt 夹具和 GTK 时序修复 | 210 项离线测试、GTK 五轮×八场景、Qt 八场景+七协议负例；代码 `2d2061a` 已推送 origin/main |
| 2026-11-17 | Qoder | 任务转交 | 新建本文，更新交接索引并追加长期计划 checkpoint；旧快照不改 |
| 待接手填写 | 后续代理 | A2/B/C/D/E/F 分波 | 填实际命令、退出码、日志、证据范围、未完成项及用户门槛；不得只写“完成” |
