//! Ownership observation: XFixes selection events plus a 250 ms identity poll.
//! The poll covers servers or owners that XFixes cannot describe (lazy owners
//! refusing TIMESTAMP report zero and are still deduplicated by owner).
use crate::linux::atoms::Atoms;
use crate::linux::selection::Requestor;
use crate::linux::sys::{Wakeup, wait_readable2};
use crate::linux::x11::{self, Conn};
use crate::linux::{Engine, Identity, platform};
use crate::ClipboardError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct Observer {
    stop: Arc<AtomicBool>,
    wake: Arc<Wakeup>,
    thread: Option<JoinHandle<()>>,
}

impl Observer {
    pub(crate) fn spawn(engine: Arc<Engine>) -> Result<Self, ClipboardError> {
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Wakeup::new().map_err(platform)?);
        let (ready, startup) = mpsc::channel();
        let thread_stop = stop.clone();
        let thread_wake = wake.clone();
        let thread = std::thread::Builder::new()
            .name("oink-x11-clipboard-watch".into())
            .spawn(move || run(engine, thread_stop, thread_wake, ready))
            .map_err(|error| ClipboardError::Platform(format!("X11 observer: {error}")))?;
        let started = startup.recv_timeout(STARTUP_TIMEOUT);
        match started {
            Ok(Ok(())) => Ok(Self {
                stop,
                wake,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                wake.wake();
                let _ = thread.join();
                Err(ClipboardError::Platform("X11 observer startup timed out".into()))
            }
        }
    }

    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.wake.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        self.stop();
    }
}

enum Observed {
    Ownership(Option<Identity>, (u64, u32)),
    Destroyed,
}

/// XFixes events carry no arm in the x11-dl union; read the documented prefix.
///
/// SAFETY: a dispatched `XEvent` buffer is at least as large as the struct.
unsafe fn decode(event: &x11::XEvent, base: i32, clipboard: u64) -> Option<Observed> {
    // SAFETY: the union is plain data; type_ is always initialized.
    let type_ = unsafe { event.type_ };
    if type_ != base + x11::XFIXES_SELECTION_NOTIFY {
        return None;
    }
    let raw = event as *const x11::XEvent as *const x11_dl::xfixes::XFixesSelectionNotifyEvent;
    // SAFETY: the XFixes layout begins with the generic event header followed
    // by the fields read below.
    let event = unsafe { &*raw };
    if event.selection != clipboard {
        return None;
    }
    match event.subtype {
        x11::XFIXES_SUBTYPE_SET_SELECTION_OWNER => {
            let identity = (event.owner != 0).then_some(Identity {
                owner: event.owner,
                timestamp: event.selection_timestamp as u32,
            });
            Some(Observed::Ownership(
                identity,
                (event.owner, event.selection_timestamp as u32),
            ))
        }
        x11::XFIXES_SUBTYPE_WINDOW_DESTROY | x11::XFIXES_SUBTYPE_CLIENT_CLOSE => Some(Observed::Destroyed),
        _ => None,
    }
}

fn run(
    engine: Arc<Engine>,
    stop: Arc<AtomicBool>,
    wake: Arc<Wakeup>,
    ready: mpsc::Sender<Result<(), ClipboardError>>,
) {
    let setup = || -> Result<(Conn, Atoms, i32, Requestor), ClipboardError> {
        let conn = Conn::open().map_err(platform)?;
        let atoms = Atoms::load(&conn).map_err(platform)?;
        let window = conn.create_window(x11::PROPERTY_CHANGE_MASK).map_err(platform)?;
        let base = conn
            .select_xfixes_input(
                window,
                atoms.clipboard,
                x11::XFIXES_SET_SELECTION_OWNER
                    | x11::XFIXES_SELECTION_WINDOW_DESTROY
                    | x11::XFIXES_SELECTION_CLIENT_CLOSE,
            )
            .map_err(platform)?;
        // The subscription must reach the server before the initial identity
        // read, otherwise a change could fall between both observations.
        conn.sync();
        let poll = Requestor::open().map_err(platform)?;
        let initial = poll.identity().map_err(platform)?;
        engine.note_initial(initial);
        Ok((conn, atoms, base, poll))
    };
    let (conn, atoms, base, poll) = match setup() {
        Ok(state) => {
            let _ = ready.send(Ok(()));
            state
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let mut next_poll = Instant::now() + POLL_INTERVAL;
    while !stop.load(Ordering::SeqCst) {
        let timeout = next_poll.saturating_duration_since(Instant::now());
        let _ = wait_readable2(conn.fd(), wake.fd(), timeout);
        if stop.load(Ordering::SeqCst) {
            break;
        }
        wake.drain();
        while conn.pending() {
            let event = conn.next_event();
            // SAFETY: see decode.
            match unsafe { decode(&event, base, atoms.clipboard) } {
                Some(Observed::Ownership(identity, key)) => engine.observe(identity, Some(key)),
                Some(Observed::Destroyed) => {
                    if let Ok(identity) = poll.identity() {
                        engine.observe(identity, None);
                    }
                }
                None => {}
            }
        }
        if Instant::now() >= next_poll {
            if let Ok(identity) = poll.identity() {
                engine.observe(identity, None);
            }
            next_poll = Instant::now() + POLL_INTERVAL;
        }
    }
}
