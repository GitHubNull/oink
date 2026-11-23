//! Native playback evidence probe; run only inside linux-native-test.py's
//! private audio scope. Starts the product playback path (source encoder ->
//! sink decoder -> pulse playback) rendering a 440 Hz tone in the middle and
//! prints one JSON tick per second. The probe measures the tone at the sink
//! monitor with parec; only levels/counts are recorded, never audio content.
use std::io::Write;
use std::time::{Duration, Instant};

fn emit(value: &serde_json::Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

fn main() {
    let seconds = std::env::args()
        .position(|arg| arg == "--seconds")
        .and_then(|index| std::env::args().nth(index + 1))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(12);
    let output = std::env::args()
        .position(|arg| arg == "--output")
        .and_then(|index| std::env::args().nth(index + 1))
        .unwrap_or_else(|| "pulse:sink:oink_test".into());
    #[cfg(not(target_os = "linux"))]
    {
        emit(&serde_json::json!({"stage": "unsupported_platform"}));
        return;
    }
    #[cfg(target_os = "linux")]
    {
        let config = oink_audio::dsp::Config::opus_10ms();
        let Ok((mut encoder, mut encoded)) = oink_audio::source::channel(config, 100, false) else {
            emit(&serde_json::json!({"stage": "channel_error"}));
            std::process::exit(2);
        };
        let mut sink = match oink_audio::sink::Sink::start(&output) {
            Ok(sink) => sink,
            Err(error) => {
                emit(&serde_json::json!({"stage": "start_error", "error": error}));
                std::process::exit(2);
            }
        };
        let info = sink.info();
        emit(&serde_json::json!({
            "stage": "started",
            "backend": info.backend.get("backend").cloned().unwrap_or(serde_json::Value::Null),
            "sample_rate": info.sample_rate,
            "channels": info.channels,
            "reported_output_latency_ms": info.reported_output_latency_ms,
        }));
        let mut source = match sink.source(config) {
            Ok(source) => source,
            Err(error) => {
                emit(&serde_json::json!({"stage": "source_error", "error": error}));
                std::process::exit(2);
            }
        };
        // Feeder: realtime-paced 440 Hz tone from 1.5s for 4s, silence before
        // and after; the sink decoder bridges everything below.
        let feeder = std::thread::spawn(move || {
            let rate = 48_000u32;
            let hz = 440.0f64;
            let mut tick = 0u64;
            let started = Instant::now();
            let mut sequence = 0u64;
            let mut frame = 0u64;
            loop {
                tick += 1;
                if started.elapsed() >= Duration::from_secs(seconds) {
                    break;
                }
                let tone =
                    started.elapsed() >= Duration::from_millis(1500) && started.elapsed() < Duration::from_millis(5500);
                let mut samples = vec![0.0f32; 480 * 2];
                if tone {
                    for index in 0..480usize {
                        let value = (0.25
                            * (2.0 * std::f64::consts::PI * hz * (frame + index as u64) as f64 / rate as f64).sin())
                            as f32;
                        samples[index * 2] = value;
                        samples[index * 2 + 1] = value;
                    }
                    frame += 480;
                }
                encoder.supply(
                    &samples,
                    oink_audio::source::Stamp {
                        first_sample_at: None,
                        callback_at: Some(Instant::now()),
                        delivered_at: Instant::now(),
                        source_frame_position: None,
                        discontinuity: false,
                    },
                );
                while let Some(packet) = encoded.try_recv() {
                    sequence += 1;
                    let _ = source.push(
                        sequence,
                        &packet.payload[..packet.len],
                        packet.delivered_at,
                        Some(packet.ordinal),
                        None,
                    );
                }
                let target = Duration::from_millis(10 * tick);
                if started.elapsed() < target {
                    std::thread::sleep(target - started.elapsed());
                }
            }
            let _ = source;
        });
        for tick in 1..=seconds {
            std::thread::sleep(Duration::from_secs(1));
            let metrics = sink.take_metrics();
            emit(&serde_json::json!({
                "stage": "tick",
                "tick": tick,
                "error": sink.error(),
                "device_invalidated": sink.device_invalidated(),
                "rendered_dropped": metrics.attribution_dropped,
                "underrun_frames": metrics.underrun_frames,
                "queued_frames": metrics.queued_frames,
            }));
        }
        feeder.join().unwrap_or(());
        let error = None::<String>;
        drop(sink);
        emit(&serde_json::json!({"stage": "finished", "error": error}));
    }
}
