//! libpulse-binding re-exports and small shared helpers. Helpers here run on
//! control threads only; realtime stream callbacks must not call into this.
use std::time::{Duration, Instant};

use libpulse_binding as pulse;

pub(crate) use pulse::context::{Context, FlagSet as ContextFlagSet, State};
pub(crate) use pulse::mainloop::threaded::Mainloop;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// A threaded-mainloop connection ready for introspection and stream setup.
pub(crate) struct Connection {
    pub(crate) mainloop: Mainloop,
    pub(crate) context: Context,
}

impl Connection {
    /// Orderly teardown. Every exit path (including early errors) must reach
    /// this: a running threaded mainloop left behind at process exit crashes.
    pub(crate) fn close(self) {
        drop(self);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.context.disconnect();
        self.mainloop.stop();
    }
}

/// Connect to the default PulseAudio server and wait until the context is
/// READY, with a bounded poll (no callbacks, no unbounded wait). Callers own
/// the returned connection and must `close()` it.
pub(crate) fn connect(name: &str) -> Result<Connection, String> {
    let mut mainloop = Mainloop::new().ok_or("pa_threaded_mainloop_new failed")?;
    let mut context = Context::new(&mainloop, name).ok_or("pa_context_new failed")?;
    context
        .connect(None, ContextFlagSet::NOFLAGS, None)
        .map_err(|error| format!("pa_context_connect failed: {error:?}"))?;
    mainloop
        .start()
        .map_err(|error| format!("pa_threaded_mainloop_start failed: {error:?}"))?;
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        mainloop.lock();
        let state = context.get_state();
        mainloop.unlock();
        match state {
            State::Ready => return Ok(Connection { mainloop, context }),
            State::Failed | State::Terminated => {
                mainloop.stop();
                return Err(format!("pulse context {:?} while connecting", state));
            }
            _ if Instant::now() >= deadline => {
                mainloop.stop();
                return Err("timed out waiting for pulse context READY".into());
            }
            _ => std::thread::sleep(POLL_INTERVAL),
        }
    }
}

/// Wait for a one-shot introspect operation to deliver its terminal
/// `ListResult::End`/`Error` (or `()` for `get_server_info`) on the mainloop
/// thread. The operation handle stays alive for the whole wait; a missing
/// server response is an error, never a silent empty result.
pub(crate) fn wait_op<T>(rx: &std::sync::mpsc::Receiver<T>, what: &str) -> Result<T, String> {
    rx.recv_timeout(CONNECT_TIMEOUT)
        .map_err(|_| format!("timed out waiting for {what}"))
}
