//! 截获线程：独立 X 连接上的 XI2 raw 观察。
//!
//! Local 模式只在 root 上被动观察（不抢任何输入）；Remote 主动抓取、同步被动
//! 抓取与热键属于 W3。线程用 self-pipe 唤醒，X 事件与 `capture_loop::poll` 的
//! 定时器在同一线程上串行处理。

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use oink_foundation::Point;
use oink_foundation::input::CaptureMode;
use x11_dl::xinput2::{
    XI_HierarchyChanged, XI_RawButtonPress, XI_RawButtonRelease, XI_RawKeyPress, XI_RawKeyRelease, XI_RawMotion,
    XIAllDevices, XIAllMasterDevices, XIEventMask, XIHierarchyEvent, XIRawEvent, XInput2,
};
use x11_dl::xlib::{Display, GenericEvent, XEvent, Xlib};

use super::{X11Input, devices, event};
use crate::InputError;
use crate::capture_loop::{self, CaptureLoop, Wake};

pub(crate) struct CaptureThread {
    stopped: Arc<AtomicBool>,
    wake: UnixStream,
    pub(super) handle: JoinHandle<()>,
}

pub(super) fn start_loop(input: &X11Input, owner: Box<dyn CaptureLoop>) -> Result<(), InputError> {
    start(input, owner)
}

pub(super) fn start_callback(input: &X11Input, handler: crate::EventHandler) -> Result<(), InputError> {
    start(input, Box::new(CallbackOwner { handler }))
}

fn start(input: &X11Input, owner: Box<dyn CaptureLoop>) -> Result<(), InputError> {
    let mut slot = input
        .capture
        .lock()
        .map_err(|_| InputError::Platform("截获锁已损坏".into()))?;
    if slot.is_some() {
        return Err(InputError::Platform("截获循环已在运行".into()));
    }
    let stopped = Arc::new(AtomicBool::new(false));
    let (reader, writer) =
        UnixStream::pair().map_err(|e| InputError::Platform(format!("无法创建截获唤醒管道：{e}")))?;
    let thread_wake = writer
        .try_clone()
        .map_err(|e| InputError::Platform(format!("无法复制截获唤醒管道：{e}")))?;
    let thread_stopped = Arc::clone(&stopped);
    let mode = Arc::clone(&input.mode);
    let (ready_tx, ready_rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("oink-x11-capture".into())
        .spawn(
            move || match prepare(owner, mode, reader, thread_wake, thread_stopped) {
                Ok(mut state) => {
                    let _ = ready_tx.send(Ok(()));
                    state.run();
                    capture_loop::clear();
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            },
        )
        .map_err(|e| InputError::Platform(format!("无法启动 X11 截获线程：{e}")))?;
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let _ = handle.join();
            return Err(error);
        }
        Err(_) => {
            stopped.store(true, Ordering::SeqCst);
            let _ = (&writer).write(&[1]);
            let _ = handle.join();
            return Err(InputError::Platform("X11 截获线程启动超时".into()));
        }
    }
    *slot = Some(CaptureThread {
        stopped,
        wake: writer,
        handle,
    });
    Ok(())
}

pub(super) fn stop(input: &X11Input) {
    let thread = input.capture.lock().ok().and_then(|mut slot| slot.take());
    if let Some(thread) = thread {
        thread.stopped.store(true, Ordering::SeqCst);
        let _ = (&thread.wake).write(&[1]);
        let _ = thread.handle.join();
    }
}

/// `InputBackend::start` 的适配器：把每条事件原样交给回调。
struct CallbackOwner {
    handler: crate::EventHandler,
}

impl CaptureLoop for CallbackOwner {
    fn set_wake(&mut self, _: Wake) {}
    fn event(&mut self, event: &crate::CapturedEvent) -> crate::Disposition {
        (self.handler)(event)
    }
    fn poll(&mut self, _: Instant) -> Option<Instant> {
        None
    }
    fn stopped(&self) -> bool {
        false
    }
}

struct LoopState {
    xlib: Arc<Xlib>,
    xi: XInput2,
    dpy: *mut Display,
    opcode: i32,
    translator: event::Translator,
    stopped: Arc<AtomicBool>,
    reader: UnixStream,
    reader_buf: [u8; 64],
    supported_repeat: bool,
}

impl Drop for LoopState {
    fn drop(&mut self) {
        if !self.supported_repeat {
            tracing::warn!("X server 不支持可检测的自动重复；键盘重复按原生按下上报");
        }
        // SAFETY: 截获线程独占这个连接，join 前不会再有人使用。
        unsafe { (self.xlib.XCloseDisplay)(self.dpy) };
    }
}

fn prepare(
    owner: Box<dyn CaptureLoop>,
    mode: Arc<Mutex<CaptureMode>>,
    reader: UnixStream,
    writer: UnixStream,
    stopped: Arc<AtomicBool>,
) -> Result<LoopState, InputError> {
    let xlib = Arc::new(Xlib::open().map_err(|e| InputError::Platform(format!("截获线程加载 libX11 失败：{e}")))?);
    let xi = XInput2::open().map_err(|e| InputError::Platform(format!("截获线程加载 libXi 失败：{e}")))?;
    let dpy = super::open_display(&xlib)?;
    let state = match build_state(Arc::clone(&xlib), xi, dpy, mode, reader, stopped) {
        Ok(state) => state,
        Err(error) => {
            // SAFETY: 连接尚未交给其他线程，且没有其他所有者。
            unsafe { (xlib.XCloseDisplay)(dpy) };
            return Err(error);
        }
    };
    let wake: Wake = Arc::new(move || {
        let _ = (&writer).write(&[1]);
    });
    capture_loop::install(owner, wake);
    // 本波只有观察，没有抓取；如实上报「未接管本机输入」。
    capture_loop::capture_available(false);
    Ok(state)
}

fn build_state(
    xlib: Arc<Xlib>,
    xi: XInput2,
    dpy: *mut Display,
    mode: Arc<Mutex<CaptureMode>>,
    reader: UnixStream,
    stopped: Arc<AtomicBool>,
) -> Result<LoopState, InputError> {
    devices::negotiate(&xi, dpy)?;
    let opcode = extension_opcode(&xlib, dpy)?;
    let mut supported = 0;
    // SAFETY: dpy 有效；supported 是有效 out 指针。返回 0 表示服务器不支持可检测重复。
    let detectable = unsafe { (xlib.XkbSetDetectableAutoRepeat)(dpy, 1, &mut supported) };
    let supported_repeat = detectable != 0;
    let devices = devices::query(&xi, dpy)?;
    select_raw(&xlib, &xi, dpy)?;
    let table = Arc::clone(&xlib);
    let sampler: Box<dyn event::Sampler> = Box::new(move || query_cursor(&table, dpy));
    Ok(LoopState {
        translator: event::Translator::new(devices, mode, sampler),
        xlib,
        xi,
        dpy,
        opcode,
        stopped,
        reader,
        reader_buf: [0; 64],
        supported_repeat,
    })
}

fn extension_opcode(xlib: &Xlib, dpy: *mut Display) -> Result<i32, InputError> {
    let name = std::ffi::CString::new("XInputExtension").map_err(|_| InputError::Platform("扩展名无效".into()))?;
    let (mut opcode, mut first_event, mut first_error) = (0, 0, 0);
    // SAFETY: dpy 有效；name 以 NUL 结尾；三个 out 指针有效。
    let present =
        unsafe { (xlib.XQueryExtension)(dpy, name.as_ptr(), &mut opcode, &mut first_event, &mut first_error) };
    if present == 0 {
        return Err(InputError::Unsupported);
    }
    Ok(opcode)
}

fn select_raw(xlib: &Xlib, xi: &XInput2, dpy: *mut Display) -> Result<(), InputError> {
    let raw_kinds = [
        XI_RawKeyPress,
        XI_RawKeyRelease,
        XI_RawButtonPress,
        XI_RawButtonRelease,
        XI_RawMotion,
    ];
    let mut raw_mask = [0_u8; 4];
    for kind in raw_kinds {
        raw_mask[kind as usize / 8] |= 1 << (kind as usize % 8);
    }
    // XI_HierarchyChanged 只能挂在 XIAllDevices 上；master 设备选择会得到 BadValue。
    let mut hierarchy_mask = [0_u8; 4];
    hierarchy_mask[XI_HierarchyChanged as usize / 8] |= 1 << (XI_HierarchyChanged as usize % 8);
    let mut selections = [
        XIEventMask {
            deviceid: XIAllMasterDevices,
            mask_len: raw_mask.len() as i32,
            mask: raw_mask.as_mut_ptr(),
        },
        XIEventMask {
            deviceid: XIAllDevices,
            mask_len: hierarchy_mask.len() as i32,
            mask: hierarchy_mask.as_mut_ptr(),
        },
    ];
    // SAFETY: dpy 有效；root 来自同一连接；mask 在调用期间存活。
    let root = unsafe { (xlib.XDefaultRootWindow)(dpy) };
    // SAFETY: dpy 有效；selections 指向存活的掩码数组；root 来自同一连接。
    let status = unsafe { (xi.XISelectEvents)(dpy, root, selections.as_mut_ptr(), selections.len() as i32) };
    if status != 0 {
        return Err(InputError::Platform(format!("XI2 选择 raw 事件失败（状态 {status}）")));
    }
    Ok(())
}

fn query_cursor(xlib: &Xlib, dpy: *mut Display) -> Option<Point> {
    // SAFETY: dpy 有效且由截获线程独占。
    let root = unsafe { (xlib.XDefaultRootWindow)(dpy) };
    let (mut root_return, mut child_return) = (0, 0);
    let (mut root_x, mut root_y, mut win_x, mut win_y) = (0, 0, 0, 0);
    let mut mask = 0;
    // SAFETY: 所有 out 指针都是有效栈变量；root 来自同一连接。
    let ok = unsafe {
        (xlib.XQueryPointer)(
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
    (ok != 0).then_some(Point::new(f64::from(root_x), f64::from(root_y)))
}

impl LoopState {
    fn run(&mut self) {
        loop {
            if self.stopped.load(Ordering::SeqCst) {
                break;
            }
            while self.pending() > 0 {
                self.dispatch();
            }
            let now = Instant::now();
            for event in self.translator.flush_motion(now) {
                let _ = capture_loop::event(&event);
            }
            let (deadline, stopped) = capture_loop::poll(now);
            if stopped || self.stopped.load(Ordering::SeqCst) {
                break;
            }
            self.wait(deadline);
        }
    }

    fn pending(&self) -> i32 {
        // SAFETY: dpy 由本线程独占。
        unsafe { (self.xlib.XPending)(self.dpy) }
    }

    #[allow(non_upper_case_globals)]
    fn dispatch(&mut self) {
        // SAFETY: XEvent 是可零初始化的 C union 存储，XNextEvent 会写满。
        let mut event: XEvent = unsafe { std::mem::zeroed() };
        // SAFETY: dpy 有效；event 是足够大的 XEvent 存储；只有 XPending>0 才调用。
        unsafe { (self.xlib.XNextEvent)(self.dpy, &mut event) };
        // SAFETY: type_ 是 union 的公共首字段。
        if unsafe { event.type_ } != GenericEvent {
            return;
        }
        // SAFETY: 上面已确认 type_ == GenericEvent，cookie 字段有效。
        let cookie = unsafe { &mut event.generic_event_cookie };
        if cookie.extension != self.opcode {
            return;
        }
        // SAFETY: cookie 来自 XNextEvent；成功后 data 指向扩展事件结构。
        if unsafe { (self.xlib.XGetEventData)(self.dpy, cookie) } == 0 {
            return;
        }
        let now = Instant::now();
        match cookie.evtype {
            XI_RawKeyPress | XI_RawKeyRelease => {
                // SAFETY: XI2 raw 事件的 data 指向 XIRawEvent。
                let raw = unsafe { &*(cookie.data as *const XIRawEvent) };
                if let Some(event) = self.translator.key(now, raw) {
                    let _ = capture_loop::event(&event);
                }
            }
            XI_RawButtonPress | XI_RawButtonRelease => {
                // SAFETY: 同上。
                let raw = unsafe { &*(cookie.data as *const XIRawEvent) };
                if let Some(event) = self.translator.button(now, raw) {
                    let _ = capture_loop::event(&event);
                }
            }
            XI_RawMotion => {
                // SAFETY: 同上。
                let raw = unsafe { &*(cookie.data as *const XIRawEvent) };
                if let Some(event) = self.translator.motion(now, raw) {
                    let _ = capture_loop::event(&event);
                }
            }
            XI_HierarchyChanged => {
                // SAFETY: hierarchy 事件的 data 指向 XIHierarchyEvent。
                let change = unsafe { &*(cookie.data as *const XIHierarchyEvent) };
                tracing::info!(flags = change.flags, "XI2 设备层级变化，重新枚举");
                if let Ok(devices) = devices::query(&self.xi, self.dpy) {
                    self.translator.replace_devices(devices);
                }
            }
            _ => {}
        }
        // SAFETY: 与 XGetEventData 成对释放同一 cookie。
        unsafe { (self.xlib.XFreeEventData)(self.dpy, cookie) };
    }

    fn wait(&mut self, deadline: Option<Instant>) {
        let timeout: i32 = deadline.map_or(-1, |due| {
            let millis = due.saturating_duration_since(Instant::now()).as_millis();
            i32::try_from(millis.min(i32::MAX as u128)).unwrap_or(i32::MAX)
        });
        let mut fds = [
            libc::pollfd {
                // SAFETY: dpy 有效且由本线程独占。
                fd: unsafe { (self.xlib.XConnectionNumber)(self.dpy) },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: fds 指向两个有效 pollfd；-1 表示等待任一 fd 就绪。
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if fds[1].revents & libc::POLLIN != 0 {
            let _ = self.reader.read(&mut self.reader_buf);
        }
    }
}

#[cfg(test)]
mod tests;
