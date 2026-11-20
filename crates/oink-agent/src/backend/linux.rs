//! Linux X11 后台入口与会话校验。
//!
//! 阶段 3 只支持真实 X11 会话：Wayland（含 XWayland 客户端）没有可用的全局
//! 输入注入/截获路径，必须在启动时就明确拒绝，而不是带病启动后在运行时失败。

use crate::backend::Launch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Session {
    X11,
    Wayland,
    NoDisplay,
}

pub(crate) fn classify_session(
    session_type: Option<&str>,
    wayland_display: Option<&str>,
    display: Option<&str>,
) -> Session {
    let wayland = session_type.is_some_and(|value| value.eq_ignore_ascii_case("wayland"))
        || wayland_display.is_some_and(|value| !value.is_empty());
    if wayland {
        return Session::Wayland;
    }
    match display {
        Some(value) if !value.is_empty() => Session::X11,
        _ => Session::NoDisplay,
    }
}

pub(crate) fn refusal(session: Session) -> Option<&'static str> {
    match session {
        Session::X11 => None,
        Session::Wayland => Some(
            "oink 当前仅支持 X11 会话：检测到 Wayland（XDG_SESSION_TYPE/WAYLAND_DISPLAY），请在 X11 会话中启动（XWayland 不受支持）",
        ),
        Session::NoDisplay => Some("没有可用的图形会话：DISPLAY 未设置，请在 X11 桌面中启动"),
    }
}

pub(crate) fn run(launch: Launch) -> u8 {
    let _ = launch;
    let session_type = std::env::var("XDG_SESSION_TYPE").ok();
    let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
    let display = std::env::var("DISPLAY").ok();
    let session = classify_session(session_type.as_deref(), wayland_display.as_deref(), display.as_deref());
    if let Some(reason) = refusal(session) {
        tracing::error!("{reason}");
        // UI 以退出状态轮询后台启动结果；日志订阅建立前的拒绝也要在标准错误可见。
        eprintln!("{reason}");
        return 1;
    }
    crate::host::run_service()
}

static MAIN_CONTEXT: std::sync::Mutex<Option<glib::MainContext>> = std::sync::Mutex::new(None);

/// 把主线程工作交给 GLib 主上下文。主循环建立前（或测试中）直接执行。
pub(crate) fn on_main(work: impl FnOnce() + Send + 'static) {
    let context = MAIN_CONTEXT.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match context {
        Some(context) => {
            context.invoke(work);
            // 主线程可能正阻塞在 iteration(true)；显式唤醒，不依赖 attach 的内部行为。
            context.wakeup();
        }
        None => work(),
    }
}

/// 在调用线程（主线程）持有 GLib 主上下文并处理其事件，工作线程执行 `work`。
///
/// 不用 `MainLoop::run/quit`：`g_main_loop_run` 会把 is_running 重新置真，
/// 「quit 先于 run」会永久阻塞；这里用 acquire + iteration 轮询，退出条件由
/// 工作线程完成与 `wakeup` 决定。
pub(crate) fn run_main<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let context = glib::MainContext::new();
    let Ok(_owner) = context.acquire() else {
        // 本进程内不应发生；退回单线程执行，避免两个主循环互相等待。
        return work();
    };
    *MAIN_CONTEXT.lock().unwrap_or_else(|e| e.into_inner()) = Some(context.clone());
    let (done, finished) = std::sync::mpsc::channel();
    let worker_context = context.clone();
    let worker = std::thread::Builder::new()
        .name("oink-backend-main".into())
        .spawn(move || {
            let result = work();
            let _ = done.send(result);
            worker_context.wakeup();
        })
        .expect("无法启动后台主工作线程");
    let result = loop {
        while context.iteration(false) {}
        match finished.try_recv() {
            Ok(result) => {
                let _ = worker.join();
                break result;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // 阻塞到下一个 GLib 事件；工作线程完成时会 wakeup。
                context.iteration(true);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => match worker.join() {
                Ok(()) => unreachable!("后台工作线程未产生结果就结束"),
                Err(payload) => std::panic::resume_unwind(payload),
            },
        }
    };
    // 收尾：执行停机前已排队的 on_main 工作，再撤销主上下文登记。
    while context.iteration(false) {}
    *MAIN_CONTEXT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_classification_prefers_wayland_and_requires_display() {
        assert_eq!(classify_session(Some("x11"), None, Some(":99")), Session::X11);
        assert_eq!(
            classify_session(Some("wayland"), Some("wayland-0"), Some(":99")),
            Session::Wayland
        );
        assert_eq!(classify_session(None, Some("wayland-0"), Some(":99")), Session::Wayland);
        assert_eq!(classify_session(Some("X11"), Some(""), Some(":99")), Session::X11);
        assert_eq!(classify_session(Some("x11"), None, Some("")), Session::NoDisplay);
        assert_eq!(classify_session(None, None, None), Session::NoDisplay);
        assert!(refusal(Session::X11).is_none());
        assert!(refusal(Session::Wayland).unwrap().contains("Wayland"));
        assert!(refusal(Session::NoDisplay).unwrap().contains("DISPLAY"));
    }
}
