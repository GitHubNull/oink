//! Linux X11：按 XRandR 枚举显示器并监听配置变化。
//!
//! 只支持 X11；Wayland 会话由后台入口在创建任何后端之前拒绝，这里不处理。
//! 本机坐标为 X11 根窗口像素（scale 恒为 1）。原生写事务（排列/隐藏）留待
//! 布局接管阶段，这里只用 trait 的默认实现（Unsupported/不变）。

mod arrangement;
mod enumerate;
mod wallpaper;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use oink_foundation::display::DisplayInfo;
use x11_dl::xlib::{self, Display, Xlib};
use x11_dl::xrandr::{self as xrr, Xrandr};

use super::{ChangeHandler, DisplayBackend, DisplayError};

pub(crate) struct X11Display {
    xlib: Xlib,
    xrandr: Xrandr,
    watchers: Arc<Mutex<Vec<ChangeHandler>>>,
    watch: Mutex<Option<JoinHandle<()>>>,
    stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for X11Display {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Display").finish_non_exhaustive()
    }
}

impl Drop for X11Display {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.watch.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn backend() -> Result<Box<dyn DisplayBackend>, DisplayError> {
    let xlib = Xlib::open().map_err(|e| DisplayError::Platform(format!("加载 libX11 失败：{e}")))?;
    let xrandr = Xrandr::open().map_err(|e| DisplayError::Platform(format!("加载 libXrandr 失败：{e}")))?;
    // 枚举连接与变化监听线程各持独立连接；全局初始化必须先于其他 X 调用。
    // SAFETY: 无参数、进程内幂等，此时尚未发出任何 X 请求。
    unsafe { (xlib.XInitThreads)() };
    let backend = X11Display {
        xlib,
        xrandr,
        watchers: Arc::default(),
        watch: Mutex::new(None),
        stop: Arc::new(AtomicBool::new(false)),
    };
    // 创建即验证：把连接失败暴露在启动阶段，而不是第一次枚举时。
    backend.displays()?;
    Ok(Box::new(backend))
}

impl DisplayBackend for X11Display {
    fn displays(&self) -> Result<Vec<DisplayInfo>, DisplayError> {
        let dpy = open_display(&self.xlib)?;
        let result = enumerate::enumerate(&self.xlib, &self.xrandr, dpy);
        // SAFETY: dpy came from XOpenDisplay in this function and is closed exactly once here.
        unsafe {
            (self.xlib.XCloseDisplay)(dpy);
        }
        result
    }

    fn watch(&self, on_change: ChangeHandler) -> Result<(), DisplayError> {
        self.watchers.lock().unwrap_or_else(|e| e.into_inner()).push(on_change);
        let mut watch = self.watch.lock().unwrap_or_else(|e| e.into_inner());
        if watch.is_some() {
            return Ok(());
        }
        let xlib = Xlib::open().map_err(|e| DisplayError::Platform(format!("加载 libX11 失败：{e}")))?;
        let xrandr = Xrandr::open().map_err(|e| DisplayError::Platform(format!("加载 libXrandr 失败：{e}")))?;
        let watchers = self.watchers.clone();
        let stop = self.stop.clone();
        let handle = std::thread::Builder::new()
            .name("oink-display-watch".into())
            .spawn(move || watch_loop(&xlib, &xrandr, &stop, &watchers))
            .map_err(|e| DisplayError::Platform(format!("显示器变化线程启动失败：{e}")))?;
        *watch = Some(handle);
        Ok(())
    }
}

fn open_display(xlib: &Xlib) -> Result<*mut Display, DisplayError> {
    // SAFETY: null selects the DISPLAY environment variable, as every X client does.
    let dpy = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
    if dpy.is_null() {
        return Err(DisplayError::Platform(format!(
            "无法连接 X11 显示服务器（DISPLAY={:?}）",
            std::env::var("DISPLAY").ok()
        )));
    }
    Ok(dpy)
}

fn watch_loop(xlib: &Xlib, xrandr: &Xrandr, stop: &AtomicBool, watchers: &Mutex<Vec<ChangeHandler>>) {
    let dpy = match open_display(xlib) {
        Ok(dpy) => dpy,
        Err(error) => {
            tracing::warn!(%error, "显示器变化监听未启动");
            return;
        }
    };
    let mut event_base = 0;
    let mut error_base = 0;
    // SAFETY: dpy is an open connection and both out-pointers are valid storage.
    if unsafe { (xrandr.XRRQueryExtension)(dpy, &mut event_base, &mut error_base) } == 0 {
        tracing::warn!("显示器变化监听未启动：X 服务器没有 XRandR 扩展");
        // SAFETY: dpy came from XOpenDisplay above and is closed exactly once.
        unsafe {
            (xlib.XCloseDisplay)(dpy);
        }
        return;
    }
    // SAFETY: root belongs to this connection; selecting input is a no-op on an empty mask.
    unsafe {
        let root = (xlib.XDefaultRootWindow)(dpy);
        (xrandr.XRRSelectInput)(
            dpy,
            root,
            xrr::RRScreenChangeNotifyMask | xrr::RRCrtcChangeNotifyMask | xrr::RROutputChangeNotifyMask,
        );
        (xlib.XFlush)(dpy);
    }
    // SAFETY: XEvent is a plain 24-long union; zeroed storage is a valid event buffer.
    let mut event: xlib::XEvent = unsafe { std::mem::zeroed() };
    while !stop.load(Ordering::Acquire) {
        let mut changed = false;
        // SAFETY: XNextEvent fills the buffer for events XPending reported on this connection;
        // XRRUpdateConfiguration only reads the same buffer.
        unsafe {
            while (xlib.XPending)(dpy) > 0 {
                (xlib.XNextEvent)(dpy, &mut event);
                let kind = event.type_;
                if kind == event_base + xrr::RRScreenChangeNotify {
                    (xrandr.XRRUpdateConfiguration)(&mut event);
                    changed = true;
                } else if kind == event_base + xrr::RRNotify {
                    changed = true;
                }
            }
        }
        if changed {
            // Copy out so a callback may register another watcher without deadlocking.
            let handlers = watchers.lock().unwrap_or_else(|e| e.into_inner()).clone();
            for handler in handlers {
                handler();
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // SAFETY: this thread owns dpy and closes it once before returning.
    unsafe {
        (xlib.XCloseDisplay)(dpy);
    }
}
