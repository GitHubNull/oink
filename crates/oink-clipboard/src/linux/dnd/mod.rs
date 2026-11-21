//! Linux XDND backend: one thread owns the X connection, a tiny override-
//! redirect InputOnly catcher window and an unmapped source window that owns
//! XdndSelection while our own drag lives. Every Xlib call happens here.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use oink_foundation::Point;
use oink_foundation::dnd::DragAnchor;

use super::sys::Wakeup;
use super::platform;
use crate::content;
use crate::dnd::{
    DndBackend, DndSink, DragOperation, DragSpec, Injector, PlatformDndEvent, SourcePress,
    SourceStart,
};
use crate::ClipboardError;

mod foreign;
mod own;
mod run;
mod source;
mod target;
mod window;

/// Reply deadline for commands the drag thread must acknowledge.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn backend() -> Result<Box<dyn DndBackend>, ClipboardError> {
    Ok(Box::new(LinuxDnd::spawn()?))
}

enum Command {
    Probe(Point),
    Unprobe,
    UnprobeSource(DragOperation),
    HoldSource {
        operation: DragOperation,
        reply: mpsc::Sender<Result<DragAnchor, ClipboardError>>,
    },
    ResumeSource {
        operation: DragOperation,
        at: Option<Point>,
        reply: mpsc::Sender<bool>,
    },
    FinishSource {
        operation: DragOperation,
        accept: bool,
        reply: mpsc::Sender<bool>,
    },
    Begin {
        spec: Box<DragSpec>,
        reply: mpsc::Sender<Result<(), ClipboardError>>,
    },
    Cancel {
        reply: mpsc::Sender<bool>,
    },
    Stop,
}

struct Shared {
    sink: OnceLock<DndSink>,
    inject: OnceLock<Injector>,
    press: OnceLock<SourcePress>,
    start: OnceLock<SourceStart>,
    source_data: Mutex<Option<(DragOperation, Arc<dyn content::Provider>)>>,
}

impl Shared {
    fn emit(&self, event: PlatformDndEvent) {
        if let Some(sink) = self.sink.get() {
            sink(event);
        }
    }

    fn inject(&self, event: &oink_foundation::input::InjectEvent) -> Result<(), String> {
        self.inject
            .get()
            .ok_or_else(|| "injector unavailable".to_string())?(event)
    }
}

pub(crate) struct LinuxDnd {
    shared: Arc<Shared>,
    commands: mpsc::Sender<Command>,
    wake: Arc<Wakeup>,
    thread: Mutex<Option<JoinHandle<()>>>,
    started: AtomicBool,
}

impl LinuxDnd {
    fn spawn() -> Result<Self, ClipboardError> {
        let shared = Arc::new(Shared {
            sink: OnceLock::new(),
            inject: OnceLock::new(),
            press: OnceLock::new(),
            start: OnceLock::new(),
            source_data: Mutex::new(None),
        });
        let wake = Arc::new(Wakeup::new().map_err(platform)?);
        let (commands, command_rx) = mpsc::channel();
        let thread_shared = shared.clone();
        let thread_wake = wake.clone();
        let thread = std::thread::Builder::new()
            .name("oink-x11-dnd".into())
            .spawn(move || run::run(thread_shared, command_rx, thread_wake))
            .map_err(|error| ClipboardError::Platform(format!("X11 dnd: {error}")))?;
        Ok(Self {
            shared,
            commands,
            wake,
            thread: Mutex::new(Some(thread)),
            started: AtomicBool::new(false),
        })
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
        self.wake.wake();
    }

    fn ask<T>(&self, command: impl FnOnce(mpsc::Sender<T>) -> Command) -> Option<T> {
        let (reply, wait) = mpsc::channel();
        self.send(command(reply));
        wait.recv_timeout(REPLY_TIMEOUT).ok()
    }
}

impl DndBackend for LinuxDnd {
    fn start(&self, sink: DndSink, inject: Injector) -> Result<(), ClipboardError> {
        let _ = self.shared.sink.set(sink);
        let _ = self.shared.inject.set(inject);
        // The thread is already running; a retry only refreshes the callbacks.
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn set_source_press(&self, press: SourcePress) {
        let _ = self.shared.press.set(press);
    }

    fn set_source_start(&self, start: SourceStart) {
        let _ = self.shared.start.set(start);
    }

    fn source_content(&self, operation: DragOperation) -> Option<Arc<dyn content::Provider>> {
        self.shared
            .source_data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|(current, _)| *current == operation)
            .map(|(_, provider)| provider.clone())
    }

    fn forget_content(&self, operation: DragOperation) {
        let mut data = self.shared.source_data.lock().unwrap_or_else(|e| e.into_inner());
        if data.as_ref().is_some_and(|(current, _)| *current == operation) {
            *data = None;
        }
    }

    fn probe(&self, at: Point) {
        self.send(Command::Probe(at));
    }

    fn unprobe(&self) {
        self.send(Command::Unprobe);
    }

    fn unprobe_source(&self, operation: DragOperation) {
        self.send(Command::UnprobeSource(operation));
    }

    fn hold_source(&self, operation: DragOperation) -> Result<DragAnchor, ClipboardError> {
        self.ask(|reply| Command::HoldSource { operation, reply })
            .unwrap_or_else(|| Err(ClipboardError::Platform("dnd thread stopped".into())))
    }

    fn resume_source_at(&self, operation: DragOperation, at: Option<Point>) -> bool {
        self.ask(|reply| Command::ResumeSource { operation, at, reply })
            .unwrap_or(false)
    }

    fn finish_source(
        &self,
        operation: DragOperation,
        accept: bool,
        release: &dyn Fn() -> Result<(), String>,
    ) -> bool {
        let ok = self
            .ask(|reply| Command::FinishSource { operation, accept, reply })
            .unwrap_or(false);
        if !ok {
            return false;
        }
        // The release raises the source's button; the target side then sees
        // the drop (accept) or ends silently after our Leave (cancel).
        release().is_ok()
    }

    fn source_completion_is_async(&self) -> bool {
        true
    }

    /// The legacy unpositioned release has no press position to work from.
    fn release(&self) -> bool {
        false
    }

    fn begin(&self, spec: DragSpec) -> Result<(), ClipboardError> {
        self.ask(|reply| Command::Begin {
            spec: Box::new(spec),
            reply,
        })
        .unwrap_or_else(|| Err(ClipboardError::Platform("dnd thread stopped".into())))
    }

    fn cancel(&self) -> bool {
        self.ask(|reply| Command::Cancel { reply }).unwrap_or(false)
    }
}

impl Drop for LinuxDnd {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        self.wake.wake();
        if let Some(thread) = self.thread.get_mut().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = thread.join();
        }
    }
}
