//! Foreign-demand observation for the virtual recording source.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use libpulse_binding as pulse;
use pulse::callbacks::ListResult;
use pulse::context::subscribe::{Facility, InterestMaskSet, Operation};

use crate::linux::{mic_virtual, sys};
use crate::microphone::Demand;

/// Read-only demand observation for the virtual recording source. A foreign
/// process holding an uncorked recording stream of `oink_input` is demand;
/// our own capture streams and corked clients never count.
pub struct DemandMonitor {
    conn: sys::Connection,
    epoch: Arc<AtomicU64>,
    source_index: Option<u32>,
}

impl std::fmt::Debug for DemandMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DemandMonitor").finish_non_exhaustive()
    }
}

impl DemandMonitor {
    pub fn new() -> Result<Self, String> {
        Self::with_wake(Arc::new(|| {}))
    }

    pub fn with_wake(wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        let mut conn = sys::connect("oink-mic-demand")?;
        let epoch = Arc::new(AtomicU64::new(1));
        conn.mainloop.lock();
        conn.context.set_subscribe_callback(Some(Box::new({
            let epoch = epoch.clone();
            let wake = wake.clone();
            move |facility, operation, _| {
                let relevant = matches!(facility, Some(Facility::SourceOutput))
                    || (facility == Some(Facility::Source)
                        && matches!(operation, Some(Operation::Removed) | Some(Operation::New)));
                if relevant {
                    epoch.fetch_add(1, Ordering::AcqRel);
                    wake();
                }
            }
        })));
        conn.context
            .subscribe(InterestMaskSet::SOURCE_OUTPUT | InterestMaskSet::SOURCE, |_| {});
        conn.mainloop.unlock();
        Ok(Self {
            conn,
            epoch,
            source_index: None,
        })
    }

    pub fn revision(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    fn refresh_source_index(&mut self) -> Result<u32, String> {
        let (tx, rx) = mpsc::channel();
        let _op = {
            let name = mic_virtual::VIRTUAL_SOURCE.to_string();
            self.conn
                .context
                .introspect()
                .get_source_info_by_name(&name, move |result| match result {
                    ListResult::Item(info) => {
                        let _ = tx.send(Some(info.index));
                    }
                    ListResult::Error => {
                        let _ = tx.send(None);
                    }
                    ListResult::End => {}
                })
        };
        match sys::wait_op(&rx, "virtual source index")? {
            Some(index) => {
                self.source_index = Some(index);
                Ok(index)
            }
            None => Err("virtual recording source is missing".into()),
        }
    }

    pub fn sample(&mut self) -> Demand {
        let sample = self.observe();
        Demand {
            active: sample.as_ref().is_ok_and(|active| *active),
            epoch: self.revision(),
            error: sample.err(),
        }
    }

    fn observe(&mut self) -> Result<bool, String> {
        let index = match self.source_index {
            Some(index) => index,
            None => self.refresh_source_index()?,
        };
        let (tx, rx) = mpsc::channel::<()>();
        let foreign: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        let _op = {
            let foreign = foreign.clone();
            let pid = std::process::id().to_string();
            self.conn
                .context
                .introspect()
                .get_source_output_info_list(move |result| {
                    if let ListResult::Item(info) = result
                        && info.source == index
                        && !info.corked
                        && info
                            .proplist
                            .get_str("application.process.id")
                            .is_some_and(|owner| owner != pid)
                    {
                        foreign.fetch_add(1, Ordering::Relaxed);
                    }
                    if matches!(result, ListResult::End | ListResult::Error) {
                        let _ = tx.send(());
                    }
                })
        };
        sys::wait_op(&rx, "source output list")?;
        Ok(foreign.load(Ordering::Relaxed) > 0)
    }
}

impl Drop for DemandMonitor {
    fn drop(&mut self) {
        // Clear the callback before teardown: a late event on a stopped
        // mainloop aborts in pa_mutex_unlock. Drop only gets &mut self, so
        // mirror sys::Connection's manual teardown. Disconnect first so no
        // callback is dispatched after the mainloop stops.
        self.conn.mainloop.lock();
        self.conn.context.set_subscribe_callback(None);
        self.conn.mainloop.unlock();
        self.conn.context.disconnect();
        // Drain in-flight events before stopping: a context freed with
        // undelivered subscription events aborts in pa_queue_free
        // (q->length == 0).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            self.conn.mainloop.lock();
            let state = self.conn.context.get_state();
            self.conn.mainloop.unlock();
            if matches!(state, pulse::context::State::Terminated) || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        self.conn.mainloop.stop();
    }
}
