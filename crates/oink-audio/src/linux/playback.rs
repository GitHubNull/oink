//! Pulse playback: the write callback pulls interleaved f32 stereo from
//! `NativeRender` and hands little-endian bytes to the server, which
//! resamples the device side. The named sink is opened directly; the server
//! default sink is never read or changed.
//!
//! The callback owns fixed scratch buffers and never logs audio content.
//! Underruns are the server's to bridge (it plays silence); a callback that
//! cannot keep up is visible through `Playback::error`'s stalled-callback
//! check, not through hidden dropouts.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use libpulse_binding as pulse;
use pulse::context::subscribe::{Facility, InterestMaskSet, Operation};
use pulse::stream::SeekMode;
use serde::Serialize;

use crate::linux::{OUTPUT_PREFIX, sys};
use crate::playback::NativeRender;

/// Default 10 ms period of f32le stereo at 48 kHz.
const PERIOD_FRAMES: u32 = 480;
const PERIOD_BYTES: u32 = PERIOD_FRAMES * 2 * 4;
/// Upper bound of one pull; bounds callback work.
const MAX_PULL_FRAMES: u32 = 4 * PERIOD_FRAMES;
const START_TIMEOUT: Duration = Duration::from_secs(8);
const CONTROL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug)]
pub struct NativeOutputTiming {
    pub period_frames: u32,
    pub native_gap_frames: Option<u64>,
    pub callback_at: Instant,
    /// Pulse exposes no per-block hardware presentation timestamp.
    pub playback_at: Option<Instant>,
    pub sample_rate: u32,
    pub channels: u16,
    pub padding_frames: u32,
    pub buffer_frames: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeOutputInfo {
    pub backend: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub requested_buffer_frames: Option<u32>,
    pub actual_buffer_frames: u32,
    pub stream_output_latency_frames: Option<u32>,
    pub timestamp_source: String,
    pub fallback_reason: Option<String>,
}

#[derive(Debug)]
struct Shared {
    stop: AtomicBool,
    error: Mutex<Option<String>>,
    invalidated: AtomicBool,
}

/// Everything the worker thread must keep alive until stop.
struct Running {
    conn: sys::Connection,
    stream: Rc<RefCell<pulse::stream::Stream>>,
    _uncork_op: pulse::operation::Operation<dyn FnMut(bool)>,
}

pub struct NativePlayback {
    shared: Arc<Shared>,
    info: NativeOutputInfo,
    worker: Option<thread::JoinHandle<()>>,
}

fn set_error(shared: &Shared, error: String) {
    *shared.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
}

impl NativePlayback {
    pub fn start(
        endpoint_id: &str,
        requested: Option<u32>,
        callback: Box<dyn NativeRender>,
        on_error: Box<dyn FnMut(String) + Send>,
    ) -> Result<Self, String> {
        let sink_name = endpoint_id
            .strip_prefix(OUTPUT_PREFIX)
            .ok_or("expected pulse:sink:<name> output ID")?
            .to_string();
        if sink_name.is_empty() || sink_name.contains('\0') || sink_name.len() > 512 {
            return Err("pulse sink name must be nonempty, NUL-free and at most 512 bytes".into());
        }
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            error: Mutex::new(None),
            invalidated: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::channel();
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("oink-pulse-playback".into())
            .spawn(move || worker_main(callback, sink_name, requested, worker_shared, on_error, tx))
            .map_err(|e| format!("spawn playback thread: {e}"))?;
        let info = match rx.recv_timeout(START_TIMEOUT + sys::CONNECT_TIMEOUT + Duration::from_secs(4)) {
            Ok(started) => started?,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("playback worker exited during startup".into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                shared.stop.store(true, Ordering::Release);
                return Err("timed out starting pulse playback".into());
            }
        };
        Ok(Self {
            shared,
            info,
            worker: Some(worker),
        })
    }

    pub fn shutdown(&mut self) -> bool {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        true
    }

    pub fn device_invalidated(&self) -> bool {
        self.shared.invalidated.swap(false, Ordering::Acquire)
            || self.shared.error.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    pub fn info(&self) -> &NativeOutputInfo {
        &self.info
    }
}

impl Drop for NativePlayback {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn worker_main(
    callback: Box<dyn NativeRender>,
    sink_name: String,
    requested: Option<u32>,
    shared: Arc<Shared>,
    mut on_error: Box<dyn FnMut(String) + Send>,
    started: mpsc::Sender<Result<NativeOutputInfo, String>>,
) {
    let mut running = match run(callback, &sink_name, requested, &shared) {
        Ok((info, keep)) => {
            let _ = started.send(Ok(info));
            keep
        }
        Err(error) => {
            set_error(&shared, error.clone());
            let _ = started.send(Err(error.clone()));
            on_error(error);
            return;
        }
    };
    while !shared.stop.load(Ordering::Acquire) {
        thread::sleep(CONTROL_INTERVAL);
        running.conn.mainloop.lock();
        let stream_state = running.stream.borrow().get_state();
        let context_state = running.conn.context.get_state();
        running.conn.mainloop.unlock();
        if !matches!(context_state, pulse::context::State::Ready) {
            let message = format!("pulse context {context_state:?} while playing");
            set_error(&shared, message.clone());
            on_error(message);
            break;
        }
        if matches!(
            stream_state,
            pulse::stream::State::Failed | pulse::stream::State::Terminated
        ) {
            let message = format!("pulse playback stream {stream_state:?} while running");
            set_error(&shared, message.clone());
            on_error(message);
            break;
        }
    }
    shared.stop.store(true, Ordering::Release);
    // Clear the callback first: the closure holds an Rc clone of the stream,
    // so dropping the stream handle alone leaks the pair and never releases
    // the native stream at teardown.
    running.conn.mainloop.lock();
    running.stream.borrow_mut().set_write_callback(None);
    running.conn.mainloop.unlock();
    let Running { stream, conn, .. } = running;
    drop(stream);
    conn.close();
}

fn run(
    mut callback: Box<dyn NativeRender>,
    sink_name: &str,
    requested: Option<u32>,
    shared: &Arc<Shared>,
) -> Result<(NativeOutputInfo, Running), String> {
    let mut conn = sys::connect("oink-playback")?;
    // Threaded-mainloop contract: every pa_* call from this worker thread is
    // made under the mainloop lock.
    conn.mainloop.lock();
    conn.context.set_subscribe_callback(Some(Box::new({
        let shared = shared.clone();
        move |facility, operation, _| {
            if let (Some(Facility::Sink), Some(Operation::Removed)) = (facility, operation) {
                shared.invalidated.store(true, Ordering::Release);
            }
        }
    })));
    conn.context
        .subscribe(InterestMaskSet::SINK | InterestMaskSet::SERVER, |_| {});

    let spec = sys::sample_spec();
    let tlength_frames = match requested {
        // Some(0) requests the supported minimum baseline.
        Some(0) => 2 * PERIOD_FRAMES,
        Some(frames) => frames.clamp(PERIOD_FRAMES, 32 * PERIOD_FRAMES),
        None => 6 * PERIOD_FRAMES,
    };
    let attr = pulse::def::BufferAttr {
        maxlength: u32::MAX,
        tlength: tlength_frames * 8,
        prebuf: PERIOD_BYTES,
        minreq: PERIOD_BYTES,
        fragsize: u32::MAX,
    };
    // `stream.connect=false` stops pipewire-pulse's internal auto-linker from
    // racing manual port wiring on session-manager-less stacks; ignored
    // elsewhere. See linux/capture.rs for the full rationale.
    let mut proplist = pulse::proplist::Proplist::new().ok_or("pa_proplist_new failed")?;
    proplist
        .set("stream.connect", b"false")
        .map_err(|_| "pa_proplist_set failed")?;
    let stream = Rc::new(RefCell::new(
        pulse::stream::Stream::new_with_proplist(&mut conn.context, "oink-playback", &spec, None, &mut proplist)
            .ok_or("pa_stream_new failed")?,
    ));
    let mut float_scratch = vec![0.0f32; (MAX_PULL_FRAMES * 2) as usize];
    let mut byte_scratch = vec![0u8; (MAX_PULL_FRAMES * 8) as usize];
    {
        let write_stream = stream.clone();
        let shared_cb = shared.clone();
        let mut stream = stream.borrow_mut();
        // The write callback must be installed BEFORE connect: playback
        // requests are event-based — pipewire-pulse emits the initial
        // writable request as the stream becomes ready, and a callback
        // installed later never sees it (the sink only requests again after
        // consuming data). Record streams tolerate late installation because
        // monitor data keeps arriving; playback does not.
        stream.set_write_callback(Some(Box::new(move |_writable| {
            let callback_at = Instant::now();
            let mut stream = write_stream.borrow_mut();
            loop {
                if shared_cb.stop.load(Ordering::Acquire) {
                    break;
                }
                let Some(writable) = stream.writable_size() else {
                    break;
                };
                let frames = (writable as u32 / 8).min(MAX_PULL_FRAMES);
                if frames == 0 {
                    break;
                }
                let timing = NativeOutputTiming {
                    period_frames: frames,
                    native_gap_frames: None,
                    callback_at,
                    playback_at: None,
                    sample_rate: sys::RATE,
                    channels: sys::CHANNELS as u16,
                    padding_frames: 0,
                    buffer_frames: tlength_frames,
                };
                callback.fill(&mut float_scratch[..(frames * 2) as usize], timing);
                let samples = &float_scratch[..(frames * 2) as usize];
                for (index, sample) in samples.iter().enumerate() {
                    byte_scratch[index * 4..index * 4 + 4].copy_from_slice(&sample.to_le_bytes());
                }
                if stream
                    .write(&byte_scratch[..(frames * 8) as usize], None, 0, SeekMode::Relative)
                    .is_err()
                {
                    break;
                }
            }
            callback.complete(callback_at.elapsed());
        })));
        // START_CORKED: READY arrives once the node has a route; the probe
        // layer wires ports (session-manager-less stacks) before data flows.
        stream
            .connect_playback(
                Some(sink_name),
                Some(&attr),
                pulse::stream::FlagSet::START_CORKED,
                None,
                None,
            )
            .map_err(|e| format!("connect_playback failed: {e:?}"))?;
    }
    conn.mainloop.unlock();

    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        conn.mainloop.lock();
        let state = stream.borrow().get_state();
        conn.mainloop.unlock();
        match state {
            pulse::stream::State::Ready => break,
            pulse::stream::State::Failed | pulse::stream::State::Terminated => {
                return Err(format!("pulse stream {state:?} while starting playback"));
            }
            _ if Instant::now() >= deadline => {
                return Err(format!(
                    "timed out waiting for playback stream READY (stuck in {state:?})"
                ));
            }
            _ => thread::sleep(Duration::from_millis(5)),
        }
    }
    let (latency_frames, uncork_op) = {
        conn.mainloop.lock();
        let latency = stream.borrow().get_latency().ok().and_then(|latency| match latency {
            pulse::stream::Latency::Positive(usec) => {
                Some((usec.0 as f64 * sys::RATE as f64 / 1_000_000.0).round() as u32)
            }
            _ => None,
        });
        let op = stream.borrow_mut().uncork(Some(Box::new(|success| {
            if !success {
                tracing::warn!("pulse playback uncork was not acknowledged");
            }
        })));
        conn.mainloop.unlock();
        (latency, op)
    };

    let info = NativeOutputInfo {
        backend: "pulse".into(),
        sample_rate: sys::RATE,
        channels: sys::CHANNELS as u16,
        requested_buffer_frames: requested,
        actual_buffer_frames: tlength_frames,
        stream_output_latency_frames: latency_frames,
        timestamp_source: "pulse server clock unavailable; write-callback dequeue only".into(),
        fallback_reason: None,
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
