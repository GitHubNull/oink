//! Clipboard owner thread: publishes one binding and serves conversion
//! requests. Lazy sources resolve on bounded worker threads; this thread only
//! touches Xlib, so a slow provider can never stall the selection owner.
use crate::linux::atoms::Atoms;
use crate::linux::binding::Binding;
use crate::linux::serve::{self, Context, JobDone, Ownership, State};
use crate::linux::sys::Wakeup;
use crate::linux::x11::{self, Conn};
use crate::linux::{Engine, Identity, platform};
use crate::ClipboardError;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
const LOOP_TICK: Duration = Duration::from_millis(250);

enum Command {
    Publish {
        binding: Binding,
        reply: mpsc::Sender<Result<u64, ClipboardError>>,
    },
    Stop,
}

/// One owner thread bound to a selection (CLIPBOARD or XdndSelection).
pub(crate) struct Owner {
    commands: mpsc::Sender<Command>,
    wake: Arc<Wakeup>,
    thread: Option<JoinHandle<()>>,
}

impl Owner {
    pub(crate) fn spawn(engine: Arc<Engine>) -> Result<Self, ClipboardError> {
        Self::spawn_for("CLIPBOARD", engine)
    }

    pub(crate) fn spawn_for(selection: &str, engine: Arc<Engine>) -> Result<Self, ClipboardError> {
        let selection = selection.to_owned();
        let wake = Arc::new(Wakeup::new().map_err(platform)?);
        let (commands, command_rx) = mpsc::channel();
        let (jobs, job_rx) = mpsc::channel();
        let (ready, startup) = mpsc::channel();
        let thread_wake = wake.clone();
        let thread = std::thread::Builder::new()
            .name("oink-x11-clipboard-owner".into())
            .spawn(move || run(engine, selection, command_rx, job_rx, thread_wake, ready, jobs))
            .map_err(|error| ClipboardError::Platform(format!("X11 owner: {error}")))?;
        match startup.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                commands,
                wake,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(ClipboardError::Platform("X11 owner startup timed out".into()))
            }
        }
    }

    pub(crate) fn publish(&self, binding: Binding) -> Result<u64, ClipboardError> {
        let (reply, wait) = mpsc::channel();
        self.commands
            .send(Command::Publish { binding, reply })
            .map_err(|_| ClipboardError::Platform("X11 owner stopped".into()))?;
        self.wake.wake();
        wait.recv_timeout(REPLY_TIMEOUT)
            .map_err(|_| ClipboardError::Platform("X11 owner did not answer".into()))?
    }

    pub(crate) fn stop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        self.wake.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(
    engine: Arc<Engine>,
    selection_name: String,
    commands: mpsc::Receiver<Command>,
    job_rx: mpsc::Receiver<JobDone>,
    wake: Arc<Wakeup>,
    ready: mpsc::Sender<Result<(), ClipboardError>>,
    jobs: mpsc::Sender<JobDone>,
) {
    let setup = || -> Result<(Conn, Atoms, u64, u64), ClipboardError> {
        let conn = Conn::open().map_err(platform)?;
        let atoms = Atoms::load(&conn).map_err(platform)?;
        let selection = conn.atom(&selection_name, false).map_err(platform)?;
        if selection == 0 {
            return Err(ClipboardError::Platform("selection atom is absent".into()));
        }
        let window = conn.create_window(x11::PROPERTY_CHANGE_MASK).map_err(platform)?;
        Ok((conn, atoms, selection, window))
    };
    let (conn, atoms, selection, window) = match setup() {
        Ok(state) => {
            let _ = ready.send(Ok(()));
            state
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let ctx = Context {
        window,
        selection,
        wake: wake.clone(),
        jobs,
    };
    let mut state = State::default();
    loop {
        let mut stopping = false;
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Publish { binding, reply } => {
                    let result =
                        publish_binding(&conn, &atoms, selection, &engine, &mut state, binding, window);
                    let _ = reply.send(result);
                }
                Command::Stop => stopping = true,
            }
        }
        let mut events = std::mem::take(&mut state.stash);
        while conn.pending() {
            events.push(conn.next_event());
        }
        for event in events {
            serve::handle_event(&conn, &atoms, &ctx, &mut state, &event);
        }
        while let Ok(done) = job_rx.try_recv() {
            serve::finish_job(&conn, &atoms, selection, &mut state, done);
        }
        serve::expire(&conn, &mut state);
        conn.flush();
        if stopping {
            break;
        }
        let timeout = serve::next_tick(&state, LOOP_TICK);
        let _ = crate::linux::sys::wait_readable2(conn.fd(), wake.fd(), timeout);
        wake.drain();
    }
    state.transfers.clear();
    // Another client may own the selection by now; only release our own window.
    if conn.selection_owner(selection) == window {
        conn.set_selection_owner(selection, 0, 0);
    }
    conn.destroy_window(window);
}

fn publish_binding(
    conn: &Conn,
    atoms: &Atoms,
    selection: u64,
    engine: &Engine,
    state: &mut State,
    binding: Binding,
    window: u64,
) -> Result<u64, ClipboardError> {
    let installed = binding.install(|name| conn.atom(name, false)).map_err(platform)?;
    let timestamp = conn
        .server_timestamp(window, atoms.oink_timestamp, atoms.integer, &mut state.stash)
        .map_err(platform)?;
    let identity = Identity {
        owner: window,
        timestamp,
    };
    engine.expect_self(identity);
    conn.set_selection_owner(selection, window, u64::from(timestamp));
    conn.sync();
    if conn.selection_owner(selection) != window {
        return Err(ClipboardError::Busy);
    }
    let count = engine.note_write(identity);
    state.generation += 1;
    serve::release_transfers(conn, state);
    state.ownership = Some(Ownership {
        generation: state.generation,
        installed,
        timestamp,
    });
    Ok(count)
}

/// Install a binding and take ownership of a selection without touching the
/// CLIPBOARD engine: the XDND source serves XdndSelection from the drag loop.
pub(super) fn install_binding(
    conn: &Conn,
    atoms: &Atoms,
    selection: u64,
    state: &mut State,
    binding: Binding,
    window: u64,
) -> Result<u32, ClipboardError> {
    let installed = binding.install(|name| conn.atom(name, false)).map_err(platform)?;
    let timestamp = conn
        .server_timestamp(window, atoms.oink_timestamp, atoms.integer, &mut state.stash)
        .map_err(platform)?;
    conn.set_selection_owner(selection, window, u64::from(timestamp));
    conn.sync();
    if conn.selection_owner(selection) != window {
        return Err(ClipboardError::Busy);
    }
    state.generation += 1;
    serve::release_transfers(conn, state);
    state.ownership = Some(Ownership {
        generation: state.generation,
        installed,
        timestamp,
    });
    Ok(timestamp)
}

/// Release our own ownership of a selection after a drag or publication ends.
pub(super) fn release_selection(conn: &Conn, selection: u64, window: u64) {
    if conn.selection_owner(selection) == window {
        conn.set_selection_owner(selection, 0, 0);
    }
}
