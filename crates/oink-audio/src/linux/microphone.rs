//! Physical microphone capture and virtual-recording demand on pulse.
//! A capture opens the exact selected `pulse:source:<name>` at its native
//! rate and averages channels to mono through the shared resampler; the
//! server negotiates the format, the device rate is never changed.
//! `DemandMonitor` counts foreign uncorked recordings of `oink_input`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use libpulse_binding as pulse;
use pulse::callbacks::ListResult;
use pulse::stream::PeekResult;

use crate::linux::mic_enumerate::source_device;
use crate::linux::{INPUT_PREFIX, mic_virtual, sys};
use crate::microphone::{CaptureInfo, Permission, StartPermit, permission};

pub use crate::linux::mic_demand::DemandMonitor;
pub use crate::linux::mic_enumerate::{default_input, input_candidates, inputs, switch_default_input, virtual_output};

const START_TIMEOUT: Duration = Duration::from_secs(8);
const CONTROL_INTERVAL: Duration = from_millis();

const fn from_millis() -> Duration {
    Duration::from_millis(250)
}

struct Shared {
    stop: AtomicBool,
    error: Mutex<Option<String>>,
    invalidated: AtomicBool,
}

fn set_error(shared: &Shared, error: String) {
    *shared.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
}

struct SourceInfo {
    rate: u32,
    channels: u8,
    description: String,
}

fn source_info(conn: &sys::Connection, name: &str) -> Result<SourceInfo, String> {
    let (tx, rx) = mpsc::channel();
    let _op = {
        let query = name.to_string();
        let query_ref = query.clone();
        conn.context
            .introspect()
            .get_source_info_by_name(&query, move |result| match result {
                ListResult::Item(info) => {
                    let description = info.description.as_deref().unwrap_or(&query_ref).to_string();
                    let _ = tx.send(SourceInfo {
                        rate: info.sample_spec.rate,
                        channels: info.sample_spec.channels,
                        description,
                    });
                }
                ListResult::End | ListResult::Error => {}
            })
    };
    sys::wait_op(&rx, "microphone source info")
}

/// Explicit-device microphone capture. Never falls back to another input,
/// including after unplug: the stream dies and reports invalidation.
pub struct Capture {
    shared: Arc<Shared>,
    info: CaptureInfo,
    worker: Option<thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MicrophoneCapture")
            .field("info", &self.info)
            .field("running", &self.worker.is_some())
            .finish_non_exhaustive()
    }
}

impl Capture {
    pub fn start(id: &str, permit: StartPermit, encoder: crate::source::Encoder) -> Result<Self, String> {
        permit.check()?;
        if !matches!(permission(), Permission::Granted | Permission::SystemControlled) {
            return Err("microphone permission is not granted; authorization requires user action".into());
        }
        let name = id
            .strip_prefix(INPUT_PREFIX)
            .filter(|name| !name.is_empty() && !name.contains('\0') && name.len() <= 512)
            .ok_or("expected pulse:source:<name> microphone ID")?;
        if name == mic_virtual::VIRTUAL_SOURCE
            || name.ends_with(".monitor")
            || id.ends_with(crate::microphone::LINUX_INPUT_UID)
        {
            return Err("select a hardware microphone, not a forwarding loopback device".into());
        }
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            error: Mutex::new(None),
            invalidated: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::channel();
        let worker_shared = shared.clone();
        let name = name.to_string();
        let worker = thread::Builder::new()
            .name("oink-pulse-mic".into())
            .spawn(move || run(worker_shared, name, encoder, tx))
            .map_err(|error| format!("spawn microphone thread: {error}"))?;
        let info = match rx.recv_timeout(START_TIMEOUT + sys::CONNECT_TIMEOUT + Duration::from_secs(4)) {
            Ok(Ok(info)) => info,
            Ok(Err(error)) => {
                let _ = worker.join();
                return Err(error);
            }
            Err(_) => {
                shared.stop.store(true, Ordering::Release);
                let _ = worker.join();
                return Err("timed out starting pulse microphone".into());
            }
        };
        Ok(Self {
            shared,
            info,
            worker: Some(worker),
        })
    }

    pub fn info(&self) -> &CaptureInfo {
        &self.info
    }
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn device_invalidated(&self) -> bool {
        self.shared.invalidated.load(Ordering::Acquire)
    }
    pub fn stop_after_device_invalidation(&mut self) {
        self.shared.invalidated.store(true, Ordering::Release);
        self.stop();
    }
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Running {
    conn: sys::Connection,
    stream: std::rc::Rc<std::cell::RefCell<pulse::stream::Stream>>,
    /// The START_CORKED handshake's uncork op stays alive until teardown; the
    /// ack may arrive while the connection drains (same contract as capture).
    _uncork_op: pulse::operation::Operation<dyn FnMut(bool)>,
}

#[allow(clippy::too_many_lines)]
fn run(
    shared: Arc<Shared>,
    name: String,
    mut encoder: crate::source::Encoder,
    started: mpsc::Sender<Result<CaptureInfo, String>>,
) {
    let result = (|| {
        // The stream node on pipewire-pulse takes the client name (probe
        // wiring targets it), so keep client and stream name identical.
        let mut conn = sys::connect("oink-microphone")?;
        // Query the source format on a separate short-lived connection: an
        // introspect op on the same context races the stream setup handshake
        // and can leave the record stream stuck in Creating.
        let device = {
            let info_conn = sys::connect("oink-mic-source-info")?;
            let device = source_info(&info_conn, &name)?;
            info_conn.close();
            device
        };
        if !(8_000..=192_000).contains(&device.rate) || device.channels == 0 || device.channels > 32 {
            return Err(format!(
                "unsupported microphone format: {} Hz / {} channels",
                device.rate, device.channels
            ));
        }
        let info = CaptureInfo {
            device: source_device(&name, Some(&device.description)),
            backend: "pulse_record",
            hardware_sample_rate: device.rate,
            hardware_channels: u16::from(device.channels),
            resampler_delay_frames: if device.rate == crate::microphone::SAMPLE_RATE {
                0
            } else {
                (48_u64 * u64::from(crate::microphone::SAMPLE_RATE)).div_ceil(u64::from(device.rate)) as u32
            },
        };
        let spec = pulse::sample::Spec {
            format: pulse::sample::Format::F32le,
            channels: device.channels,
            rate: device.rate,
        };
        let attr = pulse::def::BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: (device.rate as usize / 100 * device.channels as usize * 4) as u32,
        };
        // stream.connect=false stops pipewire-pulse's internal auto-linker from
        // racing manual port wiring on session-manager-less stacks (same as the
        // monitor capture path). Unknown on other servers (ignored).
        let mut proplist = pulse::proplist::Proplist::new().ok_or("pa_proplist_new failed")?;
        proplist
            .set("stream.connect", b"false")
            .map_err(|_| "pa_proplist_set failed")?;
        conn.mainloop.lock();
        let stream = std::rc::Rc::new(std::cell::RefCell::new(
            pulse::stream::Stream::new_with_proplist(&mut conn.context, "oink-microphone", &spec, None, &mut proplist)
                .ok_or("pa_stream_new failed")?,
        ));
        {
            let mut stream = stream.borrow_mut();
            stream
                .connect_record(Some(&name), Some(&attr), pulse::stream::FlagSet::START_CORKED)
                .map_err(|error| format!("connect_record failed: {error:?}"))?;
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
                tracing::warn!("microphone stream={state:?} context={context_state:?} errno={err_no:?}");
            }
            match state {
                pulse::stream::State::Ready => break,
                pulse::stream::State::Failed | pulse::stream::State::Terminated => {
                    return Err(format!(
                        "pulse microphone stream {state:?} (context {context_state:?}, errno {err_no:?}) while starting"
                    ));
                }
                _ if Instant::now() >= deadline => {
                    return Err(format!(
                        "timed out waiting for microphone stream READY (stuck in {state:?}, context {context_state:?}, errno {err_no:?})"
                    ));
                }
                _ => thread::sleep(Duration::from_millis(5)),
            }
        }
        let uncork_op = {
            conn.mainloop.lock();
            let op = stream.borrow_mut().uncork(Some(Box::new(|success| {
                if !success {
                    tracing::warn!("pulse microphone uncork was not acknowledged");
                }
            })));
            conn.mainloop.unlock();
            op
        };
        let channels = usize::from(device.channels);
        let mut resampler = crate::microphone::resample::Resampler::new(device.rate);
        let mut block = [0.0f32; crate::microphone::FRAMES_PER_BLOCK * 2];
        let mut filled = 0usize;
        {
            let read_stream = stream.clone();
            let read_shared = shared.clone();
            let mut stream = stream.borrow_mut();
            stream.set_read_callback(Some(Box::new(move |_hint| {
                if read_shared.stop.load(Ordering::Acquire) {
                    return;
                }
                let callback_at = Instant::now();
                let mut stream = read_stream.borrow_mut();
                loop {
                    match stream.peek() {
                        Ok(PeekResult::Data(bytes)) => {
                            if bytes.len() % (4 * channels) != 0 {
                                let _ = stream.discard();
                                continue;
                            }
                            for frame in bytes.as_chunks::<4>().0.chunks_exact(channels) {
                                let mono =
                                    frame.iter().map(|chunk| f32::from_le_bytes(*chunk)).sum::<f32>() / channels as f32;
                                resampler.push(mono, |sample| {
                                    block[filled] = sample;
                                    block[filled + 1] = sample;
                                    filled += 2;
                                    if filled == block.len() {
                                        encoder.supply(
                                            &block,
                                            crate::source::Stamp {
                                                first_sample_at: None,
                                                callback_at: Some(callback_at),
                                                delivered_at: Instant::now(),
                                                source_frame_position: None,
                                                discontinuity: false,
                                            },
                                        );
                                        filled = 0;
                                    }
                                });
                            }
                            let _ = stream.discard();
                        }
                        Ok(PeekResult::Hole(_)) => {
                            if stream.discard().is_err() {
                                break;
                            }
                        }
                        Ok(PeekResult::Empty) => break,
                        Err(_) => break,
                    }
                }
            })));
        }
        Ok((
            info,
            Running {
                conn,
                stream,
                _uncork_op: uncork_op,
            },
        ))
    })();
    let (info, mut running) = match result {
        Ok(parts) => parts,
        Err(error) => {
            set_error(&shared, error.clone());
            let _ = started.send(Err(error));
            return;
        }
    };
    if started.send(Ok(info)).is_err() {
        return;
    }
    while !shared.stop.load(Ordering::Acquire) {
        thread::sleep(CONTROL_INTERVAL);
        running.conn.mainloop.lock();
        let stream_state = running.stream.borrow().get_state();
        let context_state = running.conn.context.get_state();
        running.conn.mainloop.unlock();
        if !matches!(context_state, pulse::context::State::Ready) {
            set_error(
                &shared,
                format!("pulse context {context_state:?} while capturing microphone"),
            );
            break;
        }
        if matches!(
            stream_state,
            pulse::stream::State::Failed | pulse::stream::State::Terminated
        ) {
            shared.invalidated.store(true, Ordering::Release);
            set_error(
                &shared,
                format!("pulse microphone stream {stream_state:?} while running"),
            );
            break;
        }
    }
    shared.stop.store(true, Ordering::Release);
    // Clear the callback first: the closure holds an Rc clone of the stream;
    // dropping the handle alone would leak the pair and never release it.
    running.conn.mainloop.lock();
    running.stream.borrow_mut().set_read_callback(None);
    running.conn.mainloop.unlock();
    drop(running.stream);
    // Disconnect before stopping the mainloop: a late uncork callback on a
    // stopped mainloop aborts in pa_mutex_unlock.
    running.conn.context.disconnect();
    // Drain in-flight commands before stopping: a context freed with pending
    // work aborts in pa_queue_free (q->length == 0).
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        running.conn.mainloop.lock();
        let state = running.conn.context.get_state();
        running.conn.mainloop.unlock();
        if matches!(state, pulse::context::State::Terminated) || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    running.conn.mainloop.stop();
}
