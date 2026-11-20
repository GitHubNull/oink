//! 截获线程生命周期。阶段 3 只承载路由 owner 的 wake/poll，不注册任何 grab；
//! 阶段 4 将在同一线程上接入 XI2 观察、被动抓取与远端主动抓取。

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::X11Input;
use crate::InputError;
use crate::capture_loop::{self, CaptureLoop, Wake};

pub(crate) struct CaptureThread {
    signal: Arc<Signal>,
    pub(super) handle: JoinHandle<()>,
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

    pub(super) fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        drop(state);
        self.cond.notify_all();
    }
}

pub(super) fn start_loop(input: &X11Input, owner: Box<dyn CaptureLoop>) -> Result<(), InputError> {
    let mut slot = input
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

pub(super) fn stop(input: &X11Input) {
    let thread = input.capture.lock().ok().and_then(|mut slot| slot.take());
    if let Some(thread) = thread {
        thread.signal.stop();
        let _ = thread.handle.join();
    }
}
