//! 会话锁定与空闲复位：logind → Cinnamon → XScreenSaver 只读观察。
//!
//! 查询失败返回 `None`（未知），调用方绝不能把未知当作解锁。D-Bus 连接按总
//! 线缓存；总线不可用时 30 秒内不重试，避免每个观察周期都做一次连接失败。

use std::ffi::CString;
use std::os::raw::c_void;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use x11_dl::xlib::{Display, Xlib};
use x11_dl::xss::{ScreenSaverOn, XScreenSaverInfo};

use crate::InputError;

const CALL_TIMEOUT: Duration = Duration::from_millis(250);
const BUS_RETRY: Duration = Duration::from_secs(30);

#[derive(Default)]
struct BusSlot {
    connection: Option<dbus::blocking::Connection>,
    retry_at: Option<Instant>,
}

static SYSTEM_BUS: Mutex<Option<BusSlot>> = Mutex::new(None);
static SESSION_BUS: Mutex<Option<BusSlot>> = Mutex::new(None);

fn slot(system: bool) -> &'static Mutex<Option<BusSlot>> {
    if system { &SYSTEM_BUS } else { &SESSION_BUS }
}

fn with_connection(system: bool, call: impl FnOnce(&dbus::blocking::Connection) -> Option<bool>) -> Option<bool> {
    let mut guard = slot(system).lock().unwrap_or_else(|e| e.into_inner());
    let slot = guard.get_or_insert_with(BusSlot::default);
    if slot.connection.is_none() {
        if slot.retry_at.is_some_and(|retry| Instant::now() < retry) {
            return None;
        }
        let connection = if system {
            dbus::blocking::Connection::new_system()
        } else {
            dbus::blocking::Connection::new_session()
        };
        match connection {
            Ok(connection) => slot.connection = Some(connection),
            Err(_) => {
                slot.retry_at = Instant::now().checked_add(BUS_RETRY);
                return None;
            }
        }
    }
    call(slot.connection.as_ref()?)
}

fn logind() -> Option<bool> {
    use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
    with_connection(true, |connection| {
        let proxy = connection.with_proxy(
            "org.freedesktop.login1",
            "/org/freedesktop/login1/session/auto",
            CALL_TIMEOUT,
        );
        proxy
            .get::<bool>("org.freedesktop.login1.Session", "LockedHint")
            .or_else(|_| proxy.get::<bool>("org.freedesktop.login1.Session", "Locked"))
            .ok()
    })
}

fn cinnamon() -> Option<bool> {
    with_connection(false, |connection| {
        let proxy = connection.with_proxy("org.cinnamon.ScreenSaver", "/org/cinnamon/ScreenSaver", CALL_TIMEOUT);
        proxy
            .method_call::<(bool,), _, _, _>("org.cinnamon.ScreenSaver", "GetActive", ())
            .ok()
            .map(|(active,)| active)
    })
}

type QueryExtension = unsafe extern "C" fn(*mut Display, *mut i32, *mut i32) -> i32;
type QueryInfo = unsafe extern "C" fn(*mut Display, u64, *mut XScreenSaverInfo) -> i32;

#[derive(Clone, Copy)]
struct XssFunctions {
    query_extension: QueryExtension,
    query_info: QueryInfo,
}

static XSS: OnceLock<Option<XssFunctions>> = OnceLock::new();

fn load_symbol(handle: *mut c_void, name: &str) -> Option<usize> {
    let name = CString::new(name).ok()?;
    // SAFETY: handle 来自 dlopen；name 以 NUL 结尾。符号只解析一次并缓存为 usize。
    let symbol = unsafe { libc::dlsym(handle, name.as_ptr()) };
    (!symbol.is_null()).then_some(symbol as usize)
}

fn xss_functions() -> Option<XssFunctions> {
    *XSS.get_or_init(|| {
        // 有些发行版只提供 libXss.so.1（没有 .so/.so.2），x11-dl 的加载器
        // 找不到它，因此直接按 soname 打开并复用；句柄随进程存活。
        let soname = CString::new("libXss.so.1").ok()?;
        // SAFETY: soname 以 NUL 结尾；RTLD_NOW 立即解析依赖。
        let handle = unsafe { libc::dlopen(soname.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return None;
        }
        let query_extension = load_symbol(handle, "XScreenSaverQueryExtension")?;
        let query_info = load_symbol(handle, "XScreenSaverQueryInfo")?;
        // SAFETY: query_extension 是 dlsym 得到的 XScreenSaverQueryExtension，按头文件原型调用。
        let extension = unsafe { std::mem::transmute::<usize, QueryExtension>(query_extension) };
        // SAFETY: query_info 是 dlsym 得到的 XScreenSaverQueryInfo，按头文件原型调用。
        let info = unsafe { std::mem::transmute::<usize, QueryInfo>(query_info) };
        Some(XssFunctions {
            query_extension: extension,
            query_info: info,
        })
    })
}

/// 非 X 的锁定观察：logind（`LockedHint`，旧实现回退 `Locked`）→ Cinnamon
/// `GetActive`。失败返回 `None`，由调用方再尝试 XScreenSaver。
pub(super) fn bus_locked() -> Option<bool> {
    logind().or_else(cinnamon)
}

/// 测试入口：丢弃总线缓存，让下一次观察按新的 `DBUS_SYSTEM_BUS_ADDRESS` 重连。
#[cfg(test)]
pub(super) fn reset_bus_cache() {
    for slot in [&SYSTEM_BUS, &SESSION_BUS] {
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

pub(super) fn screensaver(xlib: &Xlib, dpy: *mut Display) -> Option<bool> {
    let functions = xss_functions()?;
    let (mut event_base, mut error_base) = (0, 0);
    // SAFETY: dpy 由调用线程独占；两个 out 指针有效。
    let present = unsafe { (functions.query_extension)(dpy, &mut event_base, &mut error_base) };
    if present == 0 {
        return None;
    }
    let mut info = XScreenSaverInfo {
        window: 0,
        state: 0,
        kind: 0,
        til_or_since: 0,
        idle: 0,
        eventMask: 0,
    };
    // SAFETY: dpy 有效；root 来自同一连接；info 是正确布局的 XScreenSaverInfo。
    let root = unsafe { (xlib.XDefaultRootWindow)(dpy) };
    // SAFETY: 同上，符号签名与 XScreenSaver.h 一致。
    let status = unsafe { (functions.query_info)(dpy, root, &mut info) };
    if status == 0 {
        return None;
    }
    Some(info.state == ScreenSaverOn)
}

/// 复位 X 空闲计时（不是常驻断言）。返回 0 表示扩展拒绝。
pub(super) fn reset_idle(xlib: &Xlib, dpy: *mut Display) -> Result<(), InputError> {
    // SAFETY: dpy 有效且由调用线程的 conn 锁保护。
    let status = unsafe { (xlib.XResetScreenSaver)(dpy) };
    if status == 0 {
        return Err(InputError::Platform("XResetScreenSaver 失败".into()));
    }
    Ok(())
}
