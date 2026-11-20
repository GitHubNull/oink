//! Linux/X11 输入后端（阶段 3 的最小真实实现）。
//!
//! 连接、XTEST 可用性校验、截获线程与停止都是真实的系统调用；本阶段没有
//! XRecord/XGrabKeyboard，所以 `capture_available()` 如实上报未接管，由路由层
//! 发布故障码，而不是伪造「已截获」。注入只作用于本进程连接的那个 X server。

use std::os::raw::c_int;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use oink_foundation::Point;
use oink_foundation::input::{CaptureMode, InjectEvent, KeyCode, MouseButton, PermissionState, ScrollUnit};
use x11_dl::xlib::{Display, Xlib};
use x11_dl::xtest::Xf86vmode as Xtst;

use crate::capture_loop::{self, CaptureLoop, Wake};
use crate::{EventHandler, FeelProfile, InputBackend, InputError, InputPermissions};

/// X11 只有整格滚轮；像素滚动按常见桌面约 100 px/格折算，余数累积。
const PIXELS_PER_NOTCH: f64 = 100.0;

pub(crate) fn backend() -> Result<Box<dyn InputBackend>, InputError> {
    let xlib = Xlib::open().map_err(|e| InputError::Platform(format!("加载 libX11 失败：{e}")))?;
    let xtest = Xtst::open().map_err(|e| InputError::Platform(format!("加载 libXtst 失败：{e}")))?;
    // 全局初始化必须先于其他 X 调用：显示器枚举与截获/注入线程各持独立连接。
    // SAFETY: 无参数、进程内幂等，此时尚未发出任何 X 请求。
    unsafe { (xlib.XInitThreads)() };
    let dpy = open_display(&xlib)?;
    let (mut event_base, mut error_base, mut major, mut minor) = (0, 0, 0, 0);
    // SAFETY: dpy 是刚打开的有效连接；四个 out 指针都是有效栈变量。
    let has_xtest =
        unsafe { (xtest.XTestQueryExtension)(dpy, &mut event_base, &mut error_base, &mut major, &mut minor) } != 0;
    if !has_xtest {
        // SAFETY: 连接由本函数独占，还没有交给其他线程。
        unsafe { (xlib.XCloseDisplay)(dpy) };
        return Err(InputError::Platform("X 服务器缺少 XTEST 扩展，无法注入输入".into()));
    }
    Ok(Box::new(X11Input {
        xlib,
        xtest,
        conn: Mutex::new(Conn { dpy }),
        mode: Mutex::new(CaptureMode::Local),
        scroll: Mutex::new((0.0, 0.0)),
        capture: Mutex::new(None),
    }))
}

fn open_display(xlib: &Xlib) -> Result<*mut Display, InputError> {
    // SAFETY: NULL 让 Xlib 读取 DISPLAY 环境变量；返回空指针表示连接失败。
    let dpy = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
    if dpy.is_null() {
        let display = std::env::var("DISPLAY").unwrap_or_default();
        return Err(InputError::Platform(format!(
            "无法连接 X 显示服务器（DISPLAY={display:?}）"
        )));
    }
    Ok(dpy)
}

struct Conn {
    dpy: *mut Display,
}

// SAFETY: Display 只在本对象的 conn 锁内使用；XInitThreads 已在打开前调用，
// 其他线程各自的连接互不共享这个指针。
unsafe impl Send for Conn {}

struct CaptureThread {
    signal: Arc<Signal>,
    handle: JoinHandle<()>,
}

#[derive(Default)]
struct SignalState {
    stopped: bool,
    woken: bool,
}

#[derive(Default)]
struct Signal {
    state: Mutex<SignalState>,
    cond: Condvar,
}

impl Signal {
    fn wake(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.woken = true;
        drop(state);
        self.cond.notify_all();
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        drop(state);
        self.cond.notify_all();
    }
}

pub(crate) struct X11Input {
    xlib: Xlib,
    xtest: Xtst,
    conn: Mutex<Conn>,
    mode: Mutex<CaptureMode>,
    scroll: Mutex<(f64, f64)>,
    capture: Mutex<Option<CaptureThread>>,
}

impl std::fmt::Debug for X11Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Input")
            .field("capture", &self.capture.lock().map(|c| c.is_some()).unwrap_or(false))
            .finish_non_exhaustive()
    }
}

impl X11Input {
    fn with_dpy<T>(&self, f: impl FnOnce(*mut Display) -> T) -> Result<T, InputError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| InputError::Platform("X11 连接锁已损坏".into()))?;
        if conn.dpy.is_null() {
            return Err(InputError::Platform("X11 连接已关闭".into()));
        }
        Ok(f(conn.dpy))
    }

    fn flush(&self) -> Result<(), InputError> {
        self.with_dpy(|dpy| {
            // SAFETY: dpy 有效且由本对象持有；XFlush 不访问传入的栈外指针。
            unsafe { (self.xlib.XFlush)(dpy) };
        })
    }

    fn synth_motion_to(&self, pos: Point) -> Result<(), InputError> {
        let ok = self.with_dpy(|dpy| {
            // SAFETY: dpy 有效；screen=-1 表示默认屏，坐标为整数物理像素。
            unsafe { (self.xtest.XTestFakeMotionEvent)(dpy, -1, round(pos.x), round(pos.y), 0) }
        })?;
        self.flush()?;
        if ok == 0 {
            return Err(InputError::Platform("XTest 注入指针移动失败".into()));
        }
        Ok(())
    }

    /// XTestRelativeMotionEvent 的绑定签名与本机 libXtst 不一致，改用
    /// 「查询当前位置 + 绝对移动」合成相对位移（阶段 3 的远端位移量很小）。
    fn synth_relative(&self, dx: f64, dy: f64) -> Result<(), InputError> {
        let (dx, dy) = (round(dx), round(dy));
        if dx == 0 && dy == 0 {
            return Ok(());
        }
        let (x, y) = self.pointer()?;
        self.synth_motion_to(Point {
            x: f64::from(x.saturating_add(dx)),
            y: f64::from(y.saturating_add(dy)),
        })
    }

    fn synth_button(&self, button: u32, pressed: bool) -> Result<(), InputError> {
        let ok = self.with_dpy(|dpy| {
            // SAFETY: dpy 有效；XTEST 按钮号是 X11 核心按钮编号。
            unsafe { (self.xtest.XTestFakeButtonEvent)(dpy, button, c_int::from(pressed), 0) }
        })?;
        self.flush()?;
        if ok == 0 {
            return Err(InputError::Platform(format!("XTest 注入鼠标按钮 {button} 失败")));
        }
        Ok(())
    }

    fn synth_scroll(&self, dx: f64, dy: f64, unit: ScrollUnit) -> Result<(), InputError> {
        let per_notch = match unit {
            ScrollUnit::Line => 1.0,
            ScrollUnit::Pixel => 1.0 / PIXELS_PER_NOTCH,
        };
        let (nx, ny) = {
            let mut acc = self
                .scroll
                .lock()
                .map_err(|_| InputError::Platform("滚动累积锁已损坏".into()))?;
            (
                take_whole(&mut acc.0, dx * per_notch),
                take_whole(&mut acc.1, dy * per_notch),
            )
        };
        // dy>0 表示视图向上 = 滚轮上（4）；dx>0 表示视图向左 = 滚轮左（6）。
        for (button, steps) in [(4, ny), (5, -ny), (6, nx), (7, -nx)] {
            for _ in 0..steps.max(0) {
                self.synth_button(button, true)?;
                self.synth_button(button, false)?;
            }
        }
        Ok(())
    }

    fn synth_key(&self, code: KeyCode, pressed: bool) -> Result<(), InputError> {
        if matches!(code.page(), 0xff | 0xff_ac) {
            // 平台合成键（AppKit Fn、Windows 菜单遮罩）不是真实按键：其他目标忽略。
            return Ok(());
        }
        let Some(keycode) = x_keycode(code) else {
            return Err(InputError::Unsupported);
        };
        let ok = self.with_dpy(|dpy| {
            // SAFETY: dpy 有效；keycode 是 X11 键码（evdev+8）。
            unsafe { (self.xtest.XTestFakeKeyEvent)(dpy, keycode, c_int::from(pressed), 0) }
        })?;
        self.flush()?;
        if ok == 0 {
            return Err(InputError::Platform(format!("XTest 注入按键 {} 失败", code.0)));
        }
        Ok(())
    }

    fn pointer(&self) -> Result<(i32, i32), InputError> {
        self.with_dpy(|dpy| {
            // SAFETY: dpy 有效且无其他线程同时使用这个连接（conn 锁保护）。
            let root = unsafe { (self.xlib.XDefaultRootWindow)(dpy) };
            let (mut root_return, mut child_return) = (0, 0);
            let (mut root_x, mut root_y, mut win_x, mut win_y) = (0, 0, 0, 0);
            let mut mask = 0;
            // SAFETY: 所有 out 指针都是有效栈变量；root 来自同一个连接。
            let ok = unsafe {
                (self.xlib.XQueryPointer)(
                    dpy,
                    root,
                    &mut root_return,
                    &mut child_return,
                    &mut root_x,
                    &mut root_y,
                    &mut win_x,
                    &mut win_y,
                    &mut mask,
                )
            };
            if ok == 0 {
                return Err(InputError::Platform("XQueryPointer 查询光标失败".into()));
            }
            Ok((root_x, root_y))
        })?
    }
}

impl InputBackend for X11Input {
    fn start(&self, _handler: EventHandler) -> Result<(), InputError> {
        // 阶段 3 只有路由 owner 的生命周期；回调式截获留阶段 4。
        Err(InputError::Unsupported)
    }

    fn start_loop(&self, owner: Box<dyn CaptureLoop>) -> Result<(), InputError> {
        let mut slot = self
            .capture
            .lock()
            .map_err(|_| InputError::Platform("截获锁已损坏".into()))?;
        if slot.is_some() {
            return Err(InputError::Platform("截获循环已在运行".into()));
        }
        let signal = Arc::new(Signal::default());
        let thread_signal = Arc::clone(&signal);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("oink-x11-capture".into())
            .spawn(move || {
                let wake_signal = Arc::clone(&thread_signal);
                let wake: Wake = Arc::new(move || wake_signal.wake());
                capture_loop::install(owner, wake);
                // 调用方在返回前等待这次确认：之后发送的命令一定能看到 wake。
                let _ = ready_tx.send(());
                // 本阶段没有真实采集：如实上报「未接管本机输入」。
                capture_loop::capture_available(false);
                loop {
                    {
                        let mut state = thread_signal.state.lock().unwrap_or_else(|e| e.into_inner());
                        if state.stopped {
                            break;
                        }
                        if state.woken {
                            state.woken = false;
                            continue;
                        }
                    }
                    let (deadline, stopped) = capture_loop::poll(Instant::now());
                    if stopped {
                        break;
                    }
                    let state = thread_signal.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.stopped {
                        break;
                    }
                    let timeout = deadline
                        .map(|due| due.saturating_duration_since(Instant::now()))
                        .unwrap_or(Duration::from_secs(3600));
                    let (state, _) = thread_signal
                        .cond
                        .wait_timeout(state, timeout)
                        .unwrap_or_else(|e| e.into_inner());
                    drop(state);
                }
                capture_loop::clear();
            })
            .map_err(|e| InputError::Platform(format!("无法启动 X11 截获线程：{e}")))?;
        if ready_rx.recv_timeout(Duration::from_secs(5)).is_err() {
            signal.stop();
            let _ = handle.join();
            return Err(InputError::Platform("X11 截获线程启动超时".into()));
        }
        *slot = Some(CaptureThread { signal, handle });
        Ok(())
    }
    fn stop(&self) {
        let thread = self.capture.lock().ok().and_then(|mut slot| slot.take());
        if let Some(thread) = thread {
            thread.signal.stop();
            let _ = thread.handle.join();
        }
    }

    fn capture_active(&self) -> Option<bool> {
        Some(self.capture.lock().map(|slot| slot.is_some()).unwrap_or(false))
    }

    fn set_mode(&self, mode: CaptureMode) -> Result<(), InputError> {
        // 阶段 3 不 grab、不隐藏光标：记录模式，能力由 capture_available(false) 上报。
        *self
            .mode
            .lock()
            .map_err(|_| InputError::Platform("模式锁已损坏".into()))? = mode;
        Ok(())
    }

    fn inject(&self, event: &InjectEvent) -> Result<(), InputError> {
        match event {
            InjectEvent::MoveTo { pos } | InjectEvent::MoveToWithDelta { pos, .. } => self.synth_motion_to(*pos),
            InjectEvent::MoveRelative { dx, dy } => self.synth_relative(*dx, *dy),
            InjectEvent::Button { button, pressed } => {
                let button = x_button(*button).ok_or(InputError::Unsupported)?;
                self.synth_button(button, *pressed)
            }
            InjectEvent::Scroll { dx, dy, unit, .. } => self.synth_scroll(*dx, *dy, *unit),
            InjectEvent::Key { code, pressed } | InjectEvent::KeyWithoutLock { code, pressed } => {
                self.synth_key(*code, *pressed)
            }
            InjectEvent::TouchpadScroll { wheel, .. } => self.synth_scroll(wheel.dx, wheel.dy, wheel.unit),
            InjectEvent::Gesture(_) | InjectEvent::TouchpadContacts(_) | InjectEvent::Volume { .. } => {
                Err(InputError::Unsupported)
            }
        }
    }

    fn cursor_position(&self) -> Result<Point, InputError> {
        let (x, y) = self.pointer()?;
        Ok(Point {
            x: f64::from(x),
            y: f64::from(y),
        })
    }

    fn cursor_injection_target(&self, event: &InjectEvent) -> Option<(Point, f64)> {
        match event {
            InjectEvent::MoveTo { pos } | InjectEvent::MoveToWithDelta { pos, .. } => Some((*pos, 0.5)),
            _ => None,
        }
    }

    fn feel_profile(&self) -> FeelProfile {
        FeelProfile::default()
    }

    fn permissions(&self) -> InputPermissions {
        // X11 没有授权模型：能连上显示服务器即拥有注入权限。
        InputPermissions {
            accessibility: PermissionState::Granted,
            input_monitoring: PermissionState::Granted,
            secure_input_active: false,
        }
    }
}

impl Drop for X11Input {
    fn drop(&mut self) {
        self.stop();
        if let Ok(conn) = self.conn.get_mut() {
            if !conn.dpy.is_null() {
                // SAFETY: 截获线程已 join，连接不再被其他线程使用。
                unsafe { (self.xlib.XCloseDisplay)(conn.dpy) };
                conn.dpy = std::ptr::null_mut();
            }
        }
    }
}

fn x_button(button: MouseButton) -> Option<u32> {
    Some(match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 3,
        MouseButton::Back => 8,
        MouseButton::Forward => 9,
        MouseButton::Other(_) => return None,
    })
}

fn take_whole(accumulator: &mut f64, value: f64) -> i32 {
    *accumulator += value;
    let whole = accumulator.trunc();
    *accumulator -= whole;
    whole as i32
}

fn round(value: f64) -> i32 {
    value.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// HID 用法 → X11 键码。现代 X server 的 evdev 键码表固定为「evdev code + 8」，
/// 与键盘布局无关；布局只影响键符解释，不影响扫描码。
fn x_keycode(code: KeyCode) -> Option<u32> {
    if code.page() != KeyCode::PAGE_KEYBOARD {
        return None;
    }
    Some(u32::from(evdev_code(code.usage())?) + 8)
}

fn evdev_code(usage: u16) -> Option<u16> {
    // HID 0x04..=0x1D 按字母顺序 a..z；evdev 顺序不连续，用查表。
    const LETTERS: [u16; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,
    ];
    match usage {
        0x04..=0x1D => Some(LETTERS[usize::from(usage - 0x04)]),
        0x1E..=0x27 => Some(2 + usage - 0x1E),
        0x28 => Some(28),
        0x29 => Some(1),
        0x2A => Some(14),
        0x2B => Some(15),
        0x2C => Some(57),
        0x2D => Some(12),
        0x2E => Some(13),
        0x2F => Some(26),
        0x30 => Some(27),
        0x31 => Some(43),
        0x32 => Some(43),
        0x33 => Some(39),
        0x34 => Some(40),
        0x35 => Some(41),
        0x36 => Some(51),
        0x37 => Some(52),
        0x38 => Some(53),
        0x39 => Some(58),
        0x3A..=0x43 => Some(59 + usage - 0x3A),
        0x44 => Some(87),
        0x45 => Some(88),
        0x46..=0x52 => Some(match usage {
            0x46 => 99,
            0x47 => 70,
            0x48 => 119,
            0x49 => 110,
            0x4A => 102,
            0x4B => 104,
            0x4C => 111,
            0x4D => 107,
            0x4E => 109,
            0x4F => 106,
            0x50 => 105,
            0x51 => 108,
            0x52 => 103,
            _ => return None,
        }),
        0x53..=0x63 => Some(match usage {
            0x53 => 69,
            0x54 => 98,
            0x55 => 55,
            0x56 => 74,
            0x57 => 78,
            0x58 => 96,
            0x59 => 79,
            0x5A => 80,
            0x5B => 81,
            0x5C => 75,
            0x5D => 76,
            0x5E => 77,
            0x5F => 71,
            0x60 => 72,
            0x61 => 73,
            0x62 => 82,
            0x63 => 83,
            _ => return None,
        }),
        0x64 => Some(86),
        0x67 => Some(117),
        0x68..=0x73 => Some(183 + usage - 0x68),
        0xE0 => Some(29),
        0xE1 => Some(42),
        0xE2 => Some(56),
        0xE3 => Some(125),
        0xE4 => Some(97),
        0xE5 => Some(54),
        0xE6 => Some(100),
        0xE7 => Some(126),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_keyboard_usages_map_to_evdev_x_keycodes() {
        assert_eq!(x_keycode(KeyCode::keyboard(0x04)), Some(38)); // a = evdev 30
        assert_eq!(x_keycode(KeyCode::keyboard(0x1E)), Some(10)); // 1 = evdev 2
        assert_eq!(x_keycode(KeyCode::keyboard(0x28)), Some(36)); // Enter = evdev 28
        assert_eq!(x_keycode(KeyCode::keyboard(0x44)), Some(95)); // F11 = evdev 87
        assert_eq!(x_keycode(KeyCode::LEFT_CTRL), Some(37)); // evdev 29
        assert_eq!(x_keycode(KeyCode::keyboard(0x99)), None);
        assert_eq!(x_keycode(KeyCode::new(KeyCode::PAGE_CONSUMER, 0xE9)), None);
    }

    #[test]
    fn fractional_notches_accumulate_into_whole_wheel_steps() {
        let mut accumulator = 0.0;
        assert_eq!(take_whole(&mut accumulator, 0.4), 0);
        assert_eq!(take_whole(&mut accumulator, 0.4), 0);
        assert_eq!(take_whole(&mut accumulator, 0.4), 1);
        assert!((accumulator - 0.2).abs() < 1e-9);
    }

    #[test]
    fn synthetic_platform_keys_are_ignored_and_volume_is_unsupported() {
        // 不需要 X 连接：这两条分支在访问连接之前返回。
        let xlib = match Xlib::open() {
            Ok(xlib) => xlib,
            Err(_) => return,
        };
        let xtest = match Xtst::open() {
            Ok(xtest) => xtest,
            Err(_) => return,
        };
        let backend = X11Input {
            xlib,
            xtest,
            conn: Mutex::new(Conn {
                dpy: std::ptr::null_mut(),
            }),
            mode: Mutex::new(CaptureMode::Local),
            scroll: Mutex::new((0.0, 0.0)),
            capture: Mutex::new(None),
        };
        assert!(backend.synth_key(KeyCode::APPLE_FN, true).is_ok());
        assert!(backend.synth_key(KeyCode::MENU_MASK, true).is_ok());
        assert!(matches!(
            backend.synth_key(KeyCode::new(KeyCode::PAGE_CONSUMER, 0xE9), true),
            Err(InputError::Unsupported)
        ));
        assert!(backend.synth_key(KeyCode::keyboard(0x04), true).is_err());
    }
}
