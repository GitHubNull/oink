//! 截获线程：独立 X 连接上的 XI2 raw 观察、被动热键抓取与 Remote 主动抓取。
//!
//! 所有输入相关 X 请求都在这个线程上串行执行；其他线程只改共享愿望值并写
//! self-pipe 唤醒。抓取/遮罩的增删由 `modes::Modes` 按当前安全状态对齐。

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicI8, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use oink_foundation::Point;
use oink_foundation::input::CaptureMode;
use x11_dl::xlib::{Display, Xlib};

use super::{X11Input, devices, event};
use crate::InputBackend;
use crate::InputError;
use crate::capture_loop::{self, CaptureLoop, Wake};
use crate::session_gate::SessionGate;
use x11::{extension_opcode, query_cursor, select_hierarchy, select_raw};

mod events;
mod modes;
mod x11;

/// 没有 X 事件时也定期复核锁屏/截获请求，避免外部状态变化被无限期忽略。
const GATE_INTERVAL: Duration = Duration::from_millis(500);

pub(crate) struct CaptureThread {
    stopped: Arc<AtomicBool>,
    wake: UnixStream,
    commands: mpsc::Sender<Command>,
    pub(super) handle: JoinHandle<()>,
}

enum Command {
    Restore {
        at: Point,
        complete: Box<dyn FnOnce(bool) + Send>,
    },
}

impl CaptureThread {
    pub(super) fn notify(&self) {
        let _ = (&self.wake).write(&[1]);
    }

    fn send_restore(&self, at: Point, complete: Box<dyn FnOnce(bool) + Send>) -> Result<(), InputError> {
        self.commands
            .send(Command::Restore { at, complete })
            .map_err(|_| InputError::Platform("截获线程已停止".into()))?;
        self.notify();
        Ok(())
    }
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
    let (commands, commands_rx) = mpsc::channel();
    let startup = Startup {
        mode: Arc::clone(&input.mode),
        stopped: Arc::clone(&stopped),
        safety: Arc::clone(&input.safety),
        locked: Arc::clone(&input.locked),
        commands: commands_rx,
    };
    let (ready_tx, ready_rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("oink-x11-capture".into())
        .spawn(move || match prepare(owner, reader, thread_wake, startup) {
            Ok(mut state) => {
                let _ = ready_tx.send(Ok(()));
                state.run();
                capture_loop::clear();
            }
            Err(error) => {
                let _ = ready_tx.send(Err(error));
            }
        })
        .map_err(|e| InputError::Platform(format!("无法启动 X11 截获线程：{e}")))?;
    // 先登记再等就绪：启动期间的 set_mode/restore 只会写唤醒管道，不会死锁。
    *slot = Some(CaptureThread {
        stopped,
        wake: writer,
        commands,
        handle,
    });
    drop(slot);
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            stop(input);
            Err(error)
        }
        Err(_) => {
            stop(input);
            Err(InputError::Platform("X11 截获线程启动超时".into()))
        }
    }
}

pub(super) fn stop(input: &X11Input) {
    let thread = input.capture.lock().ok().and_then(|mut slot| slot.take());
    if let Some(thread) = thread {
        thread.stopped.store(true, Ordering::SeqCst);
        thread.notify();
        let _ = thread.handle.join();
    }
}

/// 恢复请求交给截获线程有序执行；完成回调只在观测核验后触发。
pub(super) fn restore_local(
    input: &X11Input,
    at: Point,
    complete: Box<dyn FnOnce(bool) + Send>,
) -> Result<(), InputError> {
    {
        let slot = input
            .capture
            .lock()
            .map_err(|_| InputError::Platform("截获锁已损坏".into()))?;
        if let Some(thread) = slot.as_ref() {
            return thread.send_restore(at, complete);
        }
    }
    input.inject(&oink_foundation::input::InjectEvent::MoveTo { pos: at })?;
    input.set_mode(CaptureMode::Local)?;
    input.inject(&oink_foundation::input::InjectEvent::MoveTo { pos: at })?;
    let placed = input
        .cursor_position()
        .is_ok_and(|point| (point.x - at.x).abs() <= 1.0 && (point.y - at.y).abs() <= 1.0);
    complete(placed);
    Ok(())
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
    dpy: *mut Display,
    raw_dpy: *mut Display,
    opcode: i32,
    translator: event::Translator,
    mode: Arc<Mutex<CaptureMode>>,
    stopped: Arc<AtomicBool>,
    reader: UnixStream,
    reader_buf: [u8; 64],
    supported_repeat: bool,
    safety: Arc<SessionGate>,
    locked: Arc<AtomicI8>,
    commands: mpsc::Receiver<Command>,
    modes: modes::Modes,
    active: bool,
    next_sync: Instant,
}

impl Drop for LoopState {
    fn drop(&mut self) {
        if !self.supported_repeat {
            tracing::warn!("X server 不支持可检测的自动重复；键盘重复按原生按下上报");
        }
        // 先撤抓取与遮罩，再关连接；Modes 没有 Drop，不会再触碰已关闭的 dpy。
        self.modes.shutdown();
        // SAFETY: 两个连接都由截获线程独占，join 前不会再有人使用。
        unsafe {
            (self.xlib.XCloseDisplay)(self.raw_dpy);
            (self.xlib.XCloseDisplay)(self.dpy);
        }
    }
}

struct Startup {
    mode: Arc<Mutex<CaptureMode>>,
    stopped: Arc<AtomicBool>,
    safety: Arc<SessionGate>,
    locked: Arc<AtomicI8>,
    commands: mpsc::Receiver<Command>,
}

fn prepare(
    owner: Box<dyn CaptureLoop>,
    reader: UnixStream,
    writer: UnixStream,
    startup: Startup,
) -> Result<LoopState, InputError> {
    let xlib = Arc::new(Xlib::open().map_err(|e| InputError::Platform(format!("截获线程加载 libX11 失败：{e}")))?);
    let xi = Arc::new(
        x11_dl::xinput2::XInput2::open().map_err(|e| InputError::Platform(format!("截获线程加载 libXi 失败：{e}")))?,
    );
    let dpy = super::open_display(&xlib)?;
    // raw 观察用独立连接：主动抓取的持有者收不到本设备 raw 事件。
    let raw_dpy = match super::open_display(&xlib) {
        Ok(raw_dpy) => raw_dpy,
        Err(error) => {
            // SAFETY: 连接尚未交给其他线程，且没有其他所有者。
            unsafe { (xlib.XCloseDisplay)(dpy) };
            return Err(error);
        }
    };
    let state = match build_state(Arc::clone(&xlib), xi, dpy, raw_dpy, reader, startup) {
        Ok(state) => state,
        Err(error) => {
            // SAFETY: 两个连接尚未交给其他线程，且没有其他所有者。
            unsafe {
                (xlib.XCloseDisplay)(raw_dpy);
                (xlib.XCloseDisplay)(dpy);
            }
            return Err(error);
        }
    };
    let wake: Wake = Arc::new(move || {
        let _ = (&writer).write(&[1]);
    });
    capture_loop::install(owner, Arc::clone(&wake));
    state.watch(&wake);
    // 抓取/热键/暂停/锁屏路径已在本波实现，才如实上报「已接管本机输入」。
    capture_loop::capture_available(true);
    Ok(state)
}

impl LoopState {
    fn watch(&self, wake: &Wake) {
        self.safety.watch(wake);
    }
}

fn build_state(
    xlib: Arc<Xlib>,
    xi: Arc<x11_dl::xinput2::XInput2>,
    dpy: *mut Display,
    raw_dpy: *mut Display,
    reader: UnixStream,
    startup: Startup,
) -> Result<LoopState, InputError> {
    let Startup {
        mode,
        stopped,
        safety,
        locked,
        commands,
    } = startup;
    devices::negotiate(&xi, dpy)?;
    devices::negotiate(&xi, raw_dpy)?;
    let opcode = extension_opcode(&xlib, dpy)?;
    let mut supported = 0;
    // SAFETY: dpy 有效；supported 是有效 out 指针。返回 0 表示服务器不支持可检测重复。
    let detectable = unsafe { (xlib.XkbSetDetectableAutoRepeat)(dpy, 1, &mut supported) };
    let supported_repeat = detectable != 0;
    let devices = devices::query(&xi, dpy)?;
    let modmap = devices::ModifierMap::query(&xlib, dpy)?;
    select_hierarchy(&xlib, &xi, dpy)?;
    select_raw(&xlib, &xi, raw_dpy)?;
    // SAFETY: raw_dpy 有效；同步确保 raw 选择在就绪信号前已被服务器处理。
    unsafe { (xlib.XSync)(raw_dpy, 0) };
    let table = Arc::clone(&xlib);
    let sampler: Box<dyn event::Sampler> = Box::new(move || query_cursor(&table, dpy));
    let modes = modes::Modes::new(Arc::clone(&xlib), xi, dpy, devices.clone(), modmap)?;
    let translator = event::Translator::new(devices, mode.clone(), sampler);
    Ok(LoopState {
        xlib,
        dpy,
        raw_dpy,
        opcode,
        translator,
        mode,
        stopped,
        reader,
        reader_buf: [0; 64],
        supported_repeat,
        safety,
        locked,
        commands,
        modes,
        active: false,
        next_sync: Instant::now(),
    })
}

impl LoopState {
    fn run(&mut self) {
        loop {
            if self.stopped.load(Ordering::SeqCst) {
                break;
            }
            self.drain_commands();
            self.sync_state(Instant::now());
            while self.pending(self.dpy) > 0 {
                self.dispatch(self.dpy);
            }
            while self.pending(self.raw_dpy) > 0 {
                self.dispatch(self.raw_dpy);
            }
            let now = Instant::now();
            if self.active {
                for event in self.translator.flush_motion(now) {
                    let _ = capture_loop::event(&event);
                }
            } else {
                self.translator.discard_motion();
            }
            let (deadline, stopped) = capture_loop::poll(now);
            if stopped || self.stopped.load(Ordering::SeqCst) {
                break;
            }
            let wake_at = Some(match deadline {
                Some(deadline) => deadline.min(self.next_sync),
                None => self.next_sync,
            });
            self.wait(wake_at);
        }
        // 线程退出前如实撤销能力声明；清理由 Drop 完成。
        capture_loop::capture_available(false);
    }

    fn drain_commands(&mut self) {
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::Restore { at, complete } => {
                    if let Ok(mut mode) = self.mode.lock() {
                        *mode = CaptureMode::Local;
                    }
                    let restored = self.modes.force_local(at, &mut self.translator);
                    complete(restored);
                }
            }
        }
    }

    /// 把抓取/遮罩对齐到当前安全状态。未知锁定一律按锁定处理。
    fn sync_state(&mut self, now: Instant) {
        let safety = self.safety.snapshot();
        let lock_known_unlocked = self.locked.load(Ordering::Relaxed) == 0;
        let desired_mode = self.mode.lock().map(|mode| *mode).unwrap_or(CaptureMode::Local);
        let paused = !safety.active();
        let allowed = safety.active() && lock_known_unlocked && capture_loop::capture_requested();
        let desired_remote = matches!(desired_mode, CaptureMode::Remote { .. });
        let was_active = self.active;
        if was_active && (!allowed || (desired_remote && !self.modes.remote())) {
            self.translator.rebase_cursor();
        }
        let park = match desired_mode {
            CaptureMode::Remote { park } => park,
            CaptureMode::Local => Point::new(0.0, 0.0),
        };
        let (resume, hotkeys, guard) = if paused {
            (capture_loop::paused_hotkey(), Vec::new(), false)
        } else if allowed {
            (None, capture_loop::hotkeys(), capture_loop::keyboard_key_blocked(None))
        } else {
            (None, Vec::new(), false)
        };
        let context = modes::Context {
            active: allowed,
            paused,
            resume_allowed: lock_known_unlocked,
            desired_remote,
            park,
            resume,
            hotkeys,
            guard,
        };
        self.modes.apply(&context, &mut self.translator, now);
        // 主动抓取尚未成功时不路由本机事件，避免本地与远端同时收到同一输入。
        self.active = allowed && (!desired_remote || self.modes.remote());
        self.next_sync = now + GATE_INTERVAL;
    }

    fn pending(&self, dpy: *mut Display) -> i32 {
        // SAFETY: dpy 由本线程独占。
        unsafe { (self.xlib.XPending)(dpy) }
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
                // SAFETY: raw_dpy 有效且由本线程独占。
                fd: unsafe { (self.xlib.XConnectionNumber)(self.raw_dpy) },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: fds 指向三个有效 pollfd；-1 表示等待任一 fd 就绪。
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if fds[2].revents & libc::POLLIN != 0 {
            let _ = self.reader.read(&mut self.reader_buf);
        }
    }
}

#[cfg(test)]
mod tests;
