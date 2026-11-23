//! System-output capture from the default sink's monitor source.
//!
//! The stream requests interleaved f32le stereo at 48 kHz and the server
//! resamples the monitor side, so no client-side resampling happens in the
//! read callback. The callback only copies fragments into a fixed scratch
//! buffer and feeds `source::Encoder::supply`; no file, network or unbounded
//! allocation happens there.
//!
//! `mute_source` is a documented platform no-op here: muting the captured
//! sink also silences its monitor (verified by the phase-2 audio probe), so
//! no endpoint lease is taken and the machine cannot be silenced while
//! forwarding. `source_muted()` stays false.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use libpulse_binding as pulse;
use pulse::context::subscribe::{Facility, InterestMaskSet, Operation};
use pulse::stream::PeekResult;

use crate::linux::{OUTPUT_PREFIX, enumerate, sys};
use crate::source::{self, Stamp};
use crate::{CaptureInfo, CaptureScope};

/// 10 ms fragments of f32le stereo at 48 kHz; bounds callback work.
const FRAGSIZE_BYTES: u32 = 480 * 2 * 4;
const START_TIMEOUT: Duration = Duration::from_secs(8);
const CONTROL_INTERVAL: Duration = Duration::from_millis(250);
/// Fixed upper bound of one fragment plus partial-tail room, in samples.
const SCRATCH_SAMPLES: usize = 64 * 480 * 2;

#[derive(Debug)]
pub struct Capture {
    shared: Arc<Shared>,
    worker: Option<thread::JoinHandle<()>>,
}

#[derive(Debug)]
struct Shared {
    stop: AtomicBool,
    error: Mutex<Option<String>>,
    info: Mutex<CaptureInfo>,
    default_dirty: AtomicBool,
    source_dirty: AtomicBool,
    captured_sink: Mutex<Option<String>>,
    // Read-callback diagnostics (counts only, never audio content).
    cb_calls: std::sync::atomic::AtomicU64,
    cb_data_chunks: std::sync::atomic::AtomicU64,
    cb_hole_chunks: std::sync::atomic::AtomicU64,
    cb_data_bytes: std::sync::atomic::AtomicU64,
    cb_last_peek_error: Mutex<Option<String>>,
}

enum Monitor {
    /// Monitor of a concrete default sink, resolved at start.
    Sink(String),
    /// Explicit source name (probe escape hatch for session-manager-less
    /// stacks whose server reports a symbolic default).
    Explicit(String),
}

impl Monitor {
    fn source_name(&self) -> String {
        match self {
            Monitor::Sink(sink) => format!("{sink}.monitor"),
            Monitor::Explicit(source) => source.clone(),
        }
    }
}

fn resolve_monitor() -> Result<Monitor, String> {
    if let Ok(source) = std::env::var("OINK_CAPTURE_SOURCE") {
        if source.is_empty() || source.contains('\0') || source.len() > 512 {
            return Err("OINK_CAPTURE_SOURCE must be a nonempty source name of at most 512 bytes".into());
        }
        return Ok(Monitor::Explicit(source));
    }
    match enumerate::default_sink_name() {
        Some(sink) => Ok(Monitor::Sink(sink)),
        None => Err(
            "pulse server reports no concrete default sink; set OINK_CAPTURE_SOURCE to capture an explicit source"
                .into(),
        ),
    }
}

/// Everything the worker thread must keep alive until stop: the connection
/// (dropping it disconnects and stops the mainloop) and the stream handle.
struct Running {
    conn: sys::Connection,
    stream: Rc<RefCell<pulse::stream::Stream>>,
    /// START_CORKED handshake: the uncork operation must outlive the wait for
    /// its ACK, so it is kept until teardown.
    _uncork_op: pulse::operation::Operation<dyn FnMut(bool)>,
}

impl Capture {
    pub fn start_encoded(
        encoder: source::Encoder,
        mute_source: bool,
        recovery_dir: std::path::PathBuf,
        scope: CaptureScope,
    ) -> Result<Self, String> {
        if scope == CaptureScope::Global {
            return Err("global capture scope is unsupported on Linux".into());
        }
        // Linux takes no endpoint lease (see module docs); both flags are
        // intentionally inert until default-device leases land (mic wave).
        let _ = (mute_source, recovery_dir);
        let monitor = resolve_monitor()?;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            error: Mutex::new(None),
            info: Mutex::new(CaptureInfo {
                backend: "pulse".into(),
                sample_rate: sys::RATE,
                ..Default::default()
            }),
            default_dirty: AtomicBool::new(false),
            source_dirty: AtomicBool::new(false),
            captured_sink: Mutex::new(match &monitor {
                Monitor::Sink(sink) => Some(sink.clone()),
                Monitor::Explicit(_) => None,
            }),
            cb_calls: std::sync::atomic::AtomicU64::new(0),
            cb_data_chunks: std::sync::atomic::AtomicU64::new(0),
            cb_hole_chunks: std::sync::atomic::AtomicU64::new(0),
            cb_data_bytes: std::sync::atomic::AtomicU64::new(0),
            cb_last_peek_error: Mutex::new(None),
        });
        let (tx, rx) = mpsc::channel();
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("oink-pulse-capture".into())
            .spawn(move || worker_main(encoder, monitor, worker_shared, tx))
            .map_err(|e| format!("spawn capture thread: {e}"))?;
        // The worker walks connect (bounded) then stream startup (bounded);
        // the handshake wait must cover both plus scheduling margin.
        let started = match rx.recv_timeout(START_TIMEOUT + sys::CONNECT_TIMEOUT + Duration::from_secs(4)) {
            Ok(started) => started?,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("capture worker exited during startup".into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err("timed out starting pulse capture".into());
            }
        };
        *shared.info.lock().unwrap_or_else(|e| e.into_inner()) = started;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    pub fn info(&self) -> CaptureInfo {
        self.shared.info.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    /// Read-callback diagnostics for native probes: counts and the last peek
    /// error string only; never audio content.
    pub fn callback_stats(&self) -> serde_json::Value {
        serde_json::json!({
            "calls": self.shared.cb_calls.load(Ordering::Relaxed),
            "data_chunks": self.shared.cb_data_chunks.load(Ordering::Relaxed),
            "hole_chunks": self.shared.cb_hole_chunks.load(Ordering::Relaxed),
            "data_bytes": self.shared.cb_data_bytes.load(Ordering::Relaxed),
            "last_peek_error": self
                .shared
                .cb_last_peek_error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        })
    }
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    /// Endpoint leases never run on Linux; see module docs.
    pub fn source_muted(&self) -> bool {
        false
    }
    pub fn restart_reason(&self) -> Option<String> {
        None
    }
    pub fn default_output_changed(&self) -> bool {
        if !self.shared.default_dirty.swap(false, Ordering::Acquire) {
            return false;
        }
        let captured = self
            .shared
            .captured_sink
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match (captured, enumerate::default_sink_name()) {
            (Some(captured), Some(current)) => captured != current,
            _ => true,
        }
    }
    pub fn local_controls_changed(&self) -> bool {
        // Sink volume/mute changes do not invalidate the monitor capture
        // stream; route and volume waves own that reaction.
        false
    }
    pub fn finish(&mut self) -> Option<String> {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.error()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn set_error(shared: &Shared, error: String) {
    *shared.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
}

fn worker_main(
    encoder: source::Encoder,
    monitor: Monitor,
    shared: Arc<Shared>,
    started: mpsc::Sender<Result<CaptureInfo, String>>,
) {
    let running = match run(encoder, &monitor, &shared) {
        Ok((info, keep)) => {
            let _ = started.send(Ok(info));
            keep
        }
        Err(error) => {
            set_error(&shared, error.clone());
            let _ = started.send(Err(error));
            return;
        }
    };
    // Keep the connection and stream alive until stop; the read callback
    // owns the encoder and feeds it from the mainloop thread.
    let mut running = running;
    while !shared.stop.load(Ordering::Acquire) {
        thread::sleep(CONTROL_INTERVAL);
        running.conn.mainloop.lock();
        let stream_state = running.stream.borrow().get_state();
        let context_state = running.conn.context.get_state();
        running.conn.mainloop.unlock();
        if !matches!(context_state, pulse::context::State::Ready) {
            set_error(&shared, format!("pulse context {context_state:?} while capturing"));
            break;
        }
        if matches!(
            stream_state,
            pulse::stream::State::Failed | pulse::stream::State::Terminated
        ) {
            set_error(&shared, format!("pulse capture stream {stream_state:?} while running"));
            break;
        }
        if shared.source_dirty.swap(false, Ordering::Acquire)
            && enumerate::monitor_exists(&monitor.source_name()) == Some(false)
        {
            set_error(&shared, "captured monitor source removed".into());
            break;
        }
    }
    shared.stop.store(true, Ordering::Release);
    // Drop the stream first, then the connection (disconnect + mainloop stop).
    let Running { stream, conn, .. } = running;
    drop(stream);
    conn.close();
}

fn run(
    mut encoder: source::Encoder,
    monitor: &Monitor,
    shared: &Arc<Shared>,
) -> Result<(CaptureInfo, Running), String> {
    let mut conn = sys::connect("oink-capture")?;
    // Threaded-mainloop contract: every pa_* call from this worker thread is
    // made under the mainloop lock; without it stream commands are never
    // dispatched (observed stuck in State::Creating, errno 0).
    conn.mainloop.lock();
    conn.context.set_subscribe_callback(Some(Box::new({
        let shared = shared.clone();
        move |facility, operation, _| match (facility, operation) {
            (Some(Facility::Server), Some(Operation::Changed)) => {
                shared.default_dirty.store(true, Ordering::Release);
            }
            (Some(Facility::Source), Some(Operation::Removed)) => {
                shared.source_dirty.store(true, Ordering::Release);
            }
            (Some(Facility::Sink), Some(Operation::Removed)) => {
                shared.source_dirty.store(true, Ordering::Release);
            }
            _ => {}
        }
    })));
    conn.context.subscribe(
        InterestMaskSet::SERVER | InterestMaskSet::SINK | InterestMaskSet::SOURCE,
        |_| {},
    );

    let spec = sys::sample_spec();
    // (u32::MAX means "server default"; 0 is not a default and pipewire-pulse
    // never answers the stream request, leaving it stuck in Creating.)
    let attr = pulse::def::BufferAttr {
        maxlength: u32::MAX,
        tlength: u32::MAX,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        fragsize: FRAGSIZE_BYTES,
    };
    // `stream.connect=false` stops pipewire-pulse's internal auto-linker from
    // racing manual port wiring on session-manager-less stacks. Unknown on
    // other servers (ignored); on hosted pipewire-pulse stacks session
    // managers (not this flag) own linking, so nothing changes there.
    let mut proplist = pulse::proplist::Proplist::new().ok_or("pa_proplist_new failed")?;
    proplist
        .set("stream.connect", b"false")
        .map_err(|_| "pa_proplist_set failed")?;
    let stream = Rc::new(RefCell::new(
        pulse::stream::Stream::new_with_proplist(&mut conn.context, "oink-capture", &spec, None, &mut proplist)
            .ok_or("pa_stream_new failed")?,
    ));
    {
        let mut stream = stream.borrow_mut();
        // Start corked: in a session-manager-less pipewire-pulse stack an
        // uncorked record stream never reaches READY (the node has no route
        // and the server never replies). READY first, then uncork; the probe
        // layer wires ports before data is expected.
        stream
            .connect_record(
                Some(&monitor.source_name()),
                Some(&attr),
                pulse::stream::FlagSet::START_CORKED,
            )
            .map_err(|e| format!("connect_record failed: {e:?}"))?;
    }
    conn.mainloop.unlock();
    let deadline = Instant::now() + START_TIMEOUT;
    let mut last_log = Instant::now();
    loop {
        conn.mainloop.lock();
        let state = stream.borrow().get_state();
        let context_state = conn.context.get_state();
        let err_no = conn.context.errno();
        conn.mainloop.unlock();
        if last_log.elapsed() >= Duration::from_secs(1) {
            last_log = Instant::now();
            eprintln!("probe: stream={state:?} context={context_state:?} errno={err_no:?}");
        }
        match state {
            pulse::stream::State::Ready => break,
            pulse::stream::State::Failed | pulse::stream::State::Terminated => {
                return Err(format!("pulse stream {state:?} while starting capture"));
            }
            _ if Instant::now() >= deadline => {
                return Err(format!(
                    "timed out waiting for capture stream READY (stuck in {state:?}, context {context_state:?}, errno {err_no:?})"
                ));
            }
            _ => thread::sleep(Duration::from_millis(5)),
        }
    }
    let uncork_op = {
        conn.mainloop.lock();
        let op = stream.borrow_mut().uncork(Some(Box::new(|success| {
            if !success {
                tracing::warn!("pulse capture uncork was not acknowledged");
            }
        })));
        conn.mainloop.unlock();
        op
    };

    let source_name = monitor.source_name();
    let mut scratch: Vec<f32> = Vec::with_capacity(SCRATCH_SAMPLES);
    {
        let read_stream = stream.clone();
        let shared_cb = shared.clone();
        let mut stream = stream.borrow_mut();
        stream.set_read_callback(Some(Box::new(move |_hint| {
            let callback_at = Instant::now();
            let mut discontinuity = false;
            let mut stream = read_stream.borrow_mut();
            shared_cb.cb_calls.fetch_add(1, Ordering::Relaxed);
            loop {
                match stream.peek() {
                    Ok(PeekResult::Data(bytes)) => {
                        if bytes.len() % 4 != 0 {
                            discontinuity = true;
                            let _ = stream.discard();
                            continue;
                        }
                        shared_cb.cb_data_chunks.fetch_add(1, Ordering::Relaxed);
                        shared_cb.cb_data_bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                        scratch.clear();
                        scratch.extend(bytes.as_chunks::<4>().0.iter().map(|chunk| f32::from_le_bytes(*chunk)));
                        let _ = stream.discard();
                        if !scratch.is_empty() {
                            encoder.supply(
                                &scratch,
                                Stamp {
                                    first_sample_at: None,
                                    callback_at: Some(callback_at),
                                    delivered_at: Instant::now(),
                                    source_frame_position: None,
                                    discontinuity,
                                },
                            );
                            discontinuity = false;
                        }
                    }
                    Ok(PeekResult::Hole(_)) => {
                        shared_cb.cb_hole_chunks.fetch_add(1, Ordering::Relaxed);
                        discontinuity = true;
                        if stream.discard().is_err() {
                            break;
                        }
                    }
                    Ok(PeekResult::Empty) => break,
                    Err(error) => {
                        *shared_cb.cb_last_peek_error.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(format!("{error:?}"));
                        break;
                    }
                }
            }
        })));
    }

    let info = {
        conn.mainloop.lock();
        let latency_frames = stream.borrow().get_latency().ok().and_then(|latency| match latency {
            pulse::stream::Latency::Positive(usec) => {
                Some((usec.0 as f64 * sys::RATE as f64 / 1_000_000.0).round() as u32)
            }
            _ => None,
        });
        conn.mainloop.unlock();
        let physical_output_id = match monitor {
            Monitor::Sink(sink) => Some(format!("{OUTPUT_PREFIX}{sink}")),
            Monitor::Explicit(_) => None,
        };
        CaptureInfo {
            diagnostics: serde_json::json!({
                "monitor": source_name,
                "requested_buffer_frames": FRAGSIZE_BYTES / 8,
            }),
            backend: "pulse".into(),
            sample_rate: sys::RATE,
            requested_buffer_frames: Some(FRAGSIZE_BYTES / 8),
            actual_buffer_frames: FRAGSIZE_BYTES / 8,
            reported_latency_frames: latency_frames,
            physical_output_id,
            timestamp_source: "pulse server clock unavailable; dequeue timestamps only".into(),
            ..Default::default()
        }
    };
    Ok((
        info,
        Running {
            conn,
            stream,
            _uncork_op: uncork_op,
        },
    ))
}
