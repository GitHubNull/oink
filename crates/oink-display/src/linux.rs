//! Linux X11：按 XRandR 枚举显示器并监听配置变化。
//!
//! 只支持 X11；Wayland 会话由后台入口在创建任何后端之前拒绝，这里不处理。
//! 本机坐标为 X11 根窗口像素（scale 恒为 1）。原生写事务（排列/隐藏）留待
//! 布局接管阶段，这里只用 trait 的默认实现（Unsupported/不变）。

use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use oink_foundation::display::{DisplayInfo, REFERENCE_DIAGONAL_METERS};
use oink_foundation::{Rect, Size};
use x11_dl::xlib::{self, Display, Xlib};
use x11_dl::xrandr::{self as xrr, XRRMonitorInfo, Xrandr};

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
        let result = enumerate(&self.xlib, &self.xrandr, dpy);
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

fn enumerate(xlib: &Xlib, xrandr: &Xrandr, dpy: *mut Display) -> Result<Vec<DisplayInfo>, DisplayError> {
    let mut event_base = 0;
    let mut error_base = 0;
    // SAFETY: dpy is an open connection and the two out-pointers are valid storage.
    let has_randr = unsafe { (xrandr.XRRQueryExtension)(dpy, &mut event_base, &mut error_base) } != 0;
    if !has_randr {
        return Err(DisplayError::Platform("X 服务器没有 XRandR 扩展".into()));
    }
    // SAFETY: the root window of the default screen exists for this connection.
    let root = unsafe { (xlib.XDefaultRootWindow)(dpy) };
    let mut count = 0;
    // SAFETY: get_active=1 asks only for attached, active monitors; count receives the array length.
    let monitors = unsafe { (xrandr.XRRGetMonitors)(dpy, root, 1, &mut count) };
    if monitors.is_null() || count <= 0 {
        return Ok(vec![default_screen(xlib, dpy)]);
    }
    // Rotation lives on the CRTC, not the monitor: consult the bounded output list.
    // SAFETY: root belongs to this connection; a null result is handled below.
    let resources = unsafe { (xrandr.XRRGetScreenResourcesCurrent)(dpy, root) };
    let mut out = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        // SAFETY: index is within the array returned with length count.
        let monitor = unsafe { &*monitors.add(index) };
        let atom_name = atom_name(xlib, dpy, monitor.name);
        let fallback = || format!("Monitor {}", index + 1);
        let name = atom_name.filter(|n| !n.is_empty()).unwrap_or_else(fallback);
        let rotation_deg = rotation_of(xrandr, dpy, resources, monitor);
        let reference_extent = reference_extent(monitor, rotation_deg);
        out.push(DisplayInfo {
            key: format!("x11:{name}"),
            name: name.clone(),
            bounds: Rect::new(
                f64::from(monitor.x),
                f64::from(monitor.y),
                f64::from(monitor.width.max(0)),
                f64::from(monitor.height.max(0)),
            ),
            pixel_width: monitor.width.max(0) as u32,
            pixel_height: monitor.height.max(0) as u32,
            reference_extent,
            rotation_deg,
            is_primary: monitor.primary != 0,
            is_builtin: is_builtin(&name),
            is_virtual: looks_virtual(&name),
        });
    }
    // SAFETY: both arrays were allocated by XRandR for this connection and are freed once here.
    unsafe {
        (xrandr.XRRFreeMonitors)(monitors);
        if !resources.is_null() {
            (xrandr.XRRFreeScreenResources)(resources);
        }
    }
    Ok(out)
}

fn atom_name(xlib: &Xlib, dpy: *mut Display, atom: x11_dl::xlib::Atom) -> Option<String> {
    if atom == 0 {
        return None;
    }
    // SAFETY: atom belongs to this connection; a null name is checked before CStr reads it.
    unsafe {
        let name = (xlib.XGetAtomName)(dpy, atom);
        if name.is_null() {
            return None;
        }
        let text = CStr::from_ptr(name).to_string_lossy().into_owned();
        (xlib.XFree)(name.cast());
        Some(text)
    }
}

/// Rotation of the monitor's first connected output; 0 when no CRTC or no outputs.
fn rotation_of(
    xrandr: &Xrandr,
    dpy: *mut Display,
    resources: *mut xrr::XRRScreenResources,
    monitor: &XRRMonitorInfo,
) -> u16 {
    if resources.is_null() || monitor.noutput <= 0 {
        return 0;
    }
    for index in 0..monitor.noutput {
        // SAFETY: index is within the monitor's own output array.
        let output = unsafe { *monitor.outputs.add(index as usize) };
        // SAFETY: resources is the live screen resources array; output came from this server.
        unsafe {
            let info = (xrandr.XRRGetOutputInfo)(dpy, resources, output);
            if info.is_null() {
                continue;
            }
            let crtc = (*info).crtc;
            let connection = (*info).connection;
            (xrandr.XRRFreeOutputInfo)(info);
            if crtc == 0 || i32::from(connection) != xrr::RR_Connected {
                continue;
            }
            let crtc_info = (xrandr.XRRGetCrtcInfo)(dpy, resources, crtc);
            if crtc_info.is_null() {
                continue;
            }
            let rotation = (*crtc_info).rotation;
            (xrandr.XRRFreeCrtcInfo)(crtc_info);
            return rotation_deg(rotation);
        }
    }
    0
}

fn rotation_deg(rotation: x11_dl::xrandr::Rotation) -> u16 {
    let rotation = i32::from(rotation);
    if rotation & xrr::RR_Rotate_90 != 0 {
        90
    } else if rotation & xrr::RR_Rotate_180 != 0 {
        180
    } else if rotation & xrr::RR_Rotate_270 != 0 {
        270
    } else {
        0
    }
}

/// Physical size from the monitor's millimetre fields, normalized like other platforms.
/// XRandR reports unrotated panel dimensions, so swap them for 90/270.
fn reference_extent(monitor: &XRRMonitorInfo, rotation_deg: u16) -> Option<Size> {
    let (mw, mh) = (f64::from(monitor.mwidth) / 1000.0, f64::from(monitor.mheight) / 1000.0);
    if mw <= 0.001 || mh <= 0.001 {
        return None;
    }
    let (mw, mh) = if rotation_deg == 90 || rotation_deg == 270 {
        (mh, mw)
    } else {
        (mw, mh)
    };
    Some(Size::new(
        mw / REFERENCE_DIAGONAL_METERS,
        mh / REFERENCE_DIAGONAL_METERS,
    ))
}

/// Some minimal servers report no RandR monitors; the root screen geometry is still real.
fn default_screen(xlib: &Xlib, dpy: *mut Display) -> DisplayInfo {
    // SAFETY: dpy is open; the default screen index and its geometry are read-only queries.
    let (width, height, mm_width, mm_height) = unsafe {
        let screen = (xlib.XDefaultScreen)(dpy);
        (
            (xlib.XDisplayWidth)(dpy, screen),
            (xlib.XDisplayHeight)(dpy, screen),
            (xlib.XDisplayWidthMM)(dpy, screen),
            (xlib.XDisplayHeightMM)(dpy, screen),
        )
    };
    let (width, height) = (width.max(0), height.max(0));
    let reference_extent = if mm_width > 0 && mm_height > 0 {
        Some(Size::new(
            f64::from(mm_width) / 1000.0 / REFERENCE_DIAGONAL_METERS,
            f64::from(mm_height) / 1000.0 / REFERENCE_DIAGONAL_METERS,
        ))
    } else {
        None
    };
    DisplayInfo {
        key: "x11:screen".into(),
        name: "X11 Screen".into(),
        bounds: Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
        pixel_width: width as u32,
        pixel_height: height as u32,
        reference_extent,
        rotation_deg: 0,
        is_primary: true,
        is_builtin: false,
        is_virtual: false,
    }
}

fn is_builtin(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["edp", "lvds", "dsi", "embedded"].iter().any(|k| name.starts_with(k))
}

fn looks_virtual(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "virtual",
        "dummy",
        "headless",
        "vnc",
        "remote display",
        "parsec",
        "deskpad",
    ]
    .iter()
    .any(|k| name.contains(k))
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
