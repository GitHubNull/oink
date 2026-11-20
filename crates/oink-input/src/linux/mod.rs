//! Linux/X11 输入后端。
//!
//! 连接、XTEST 可用性校验、截获线程与停止都是真实的系统调用；阶段 3 没有
//! XRecord/XGrabKeyboard，所以 `capture_available()` 如实上报未接管，由路由层
//! 发布故障码，而不是伪造「已截获」。注入只作用于本进程连接的那个 X server。

use std::sync::{Mutex, MutexGuard};

use oink_foundation::Point;
use oink_foundation::input::{CaptureMode, InjectEvent, PermissionState};
use x11_dl::xlib::{Display, Xlib};
use x11_dl::xtest::Xf86vmode as Xtst;

use crate::{EventHandler, FeelProfile, InputBackend, InputError, InputPermissions};

mod capture;
mod cursor;
mod devices;
mod event;
mod hotkeys;
mod inject;
mod keymap;
mod session;

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

pub(super) struct Conn {
    pub(super) dpy: *mut Display,
}

// SAFETY: Display 只在本对象的 conn 锁内使用；XInitThreads 已在打开前调用，
// 其他线程各自的连接互不共享这个指针。
unsafe impl Send for Conn {}

pub(super) struct X11Input {
    pub(super) xlib: Xlib,
    pub(super) xtest: Xtst,
    pub(super) conn: Mutex<Conn>,
    pub(super) mode: Mutex<CaptureMode>,
    pub(super) scroll: Mutex<(f64, f64)>,
    pub(super) capture: Mutex<Option<capture::CaptureThread>>,
}

impl std::fmt::Debug for X11Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Input")
            .field("capture", &self.capture.lock().map(|c| c.is_some()).unwrap_or(false))
            .finish_non_exhaustive()
    }
}

impl X11Input {
    pub(super) fn with_dpy<T>(&self, f: impl FnOnce(*mut Display) -> T) -> Result<T, InputError> {
        let conn: MutexGuard<'_, Conn> = self
            .conn
            .lock()
            .map_err(|_| InputError::Platform("X11 连接锁已损坏".into()))?;
        if conn.dpy.is_null() {
            return Err(InputError::Platform("X11 连接已关闭".into()));
        }
        Ok(f(conn.dpy))
    }

    pub(super) fn flush(&self) -> Result<(), InputError> {
        self.with_dpy(|dpy| {
            // SAFETY: dpy 有效且由本对象持有；XFlush 不访问传入的栈外指针。
            unsafe { (self.xlib.XFlush)(dpy) };
        })
    }

    pub(super) fn pointer(&self) -> Result<(i32, i32), InputError> {
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

    fn start_loop(&self, owner: Box<dyn crate::capture_loop::CaptureLoop>) -> Result<(), InputError> {
        capture::start_loop(self, owner)
    }

    fn stop(&self) {
        capture::stop(self);
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
        inject::inject(self, event)
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
        if let Ok(conn) = self.conn.get_mut()
            && !conn.dpy.is_null()
        {
            // SAFETY: 截获线程已 join，连接不再被其他线程使用。
            unsafe { (self.xlib.XCloseDisplay)(conn.dpy) };
            conn.dpy = std::ptr::null_mut();
        }
    }
}
