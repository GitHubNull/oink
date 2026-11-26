//! Native microphone evidence probe; run only inside linux-native-test.py's
//! private audio scope. Enumerates inputs, starts the product microphone
//! capture on the test source, feeds the virtual render sink through the
//! shared source encoder, then observes foreign demand via DemandMonitor.
//! Only counts/flags/levels are recorded, never audio content.
use std::io::Write;
use std::time::{Duration, Instant};

fn emit(value: &serde_json::Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

fn main() {
    let source = std::env::args()
        .position(|arg| arg == "--source")
        .and_then(|index| std::env::args().nth(index + 1))
        .unwrap_or_else(|| "pulse:source:oink_test_source".into());
    let seconds = std::env::args()
        .position(|arg| arg == "--seconds")
        .and_then(|index| std::env::args().nth(index + 1))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(10);
    #[cfg(not(target_os = "linux"))]
    {
        emit(&serde_json::json!({"stage": "unsupported_platform"}));
        return;
    }
    #[cfg(target_os = "linux")]
    {
        let permission = oink_audio::microphone::permission();
        emit(&serde_json::json!({
            "stage": "permission",
            "permission": format!("{permission:?}"),
        }));
        let inputs = oink_audio::microphone::inputs().unwrap_or_default();
        let candidates = oink_audio::microphone::input_candidates().unwrap_or_default();
        let default_input = oink_audio::microphone::default_input().ok().flatten();
        let virtual_output = oink_audio::microphone::virtual_output().ok();
        let render_id = match virtual_output {
            Some(oink_audio::microphone::VirtualOutput::Ready { render_id, .. }) => Some(render_id),
            _ => None,
        };
        emit(&serde_json::json!({
            "stage": "enumeration",
            "inputs": inputs.len(),
            "candidates": candidates.len(),
            "default_input": default_input.map(|i| i.id),
            "virtual_output_ready": render_id.is_some(),
        }));
        let Some(render_id) = render_id else {
            emit(&serde_json::json!({"stage": "virtual_output_unavailable"}));
            std::process::exit(2);
        };
        // Start a foreign recording of the virtual source to create demand.
        let foreign = std::process::Command::new("parec")
            .args([
                "--device=oink_input",
                "--rate=48000",
                "--channels=1",
                "--format=float32le",
                "--stream-name=oink-foreign-rec",
                "--property=media.name=oink-foreign-rec",
                "--latency-msec=40",
                "/dev/null",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        let mut monitor = match oink_audio::microphone::DemandMonitor::new() {
            Ok(monitor) => monitor,
            Err(error) => {
                emit(&serde_json::json!({"stage": "monitor_error", "error": error}));
                std::process::exit(2);
            }
        };
        // Wait for the foreign recorder to appear as active demand.
        let mut demand_before = monitor.sample();
        for _ in 0..50 {
            if demand_before.active {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            demand_before = monitor.sample();
        }
        emit(&serde_json::json!({
            "stage": "demand_before_capture",
            "active": demand_before.active,
            "epoch": demand_before.epoch,
            "error": demand_before.error,
        }));
        let config = oink_audio::dsp::Config::opus_10ms();
        let Ok((capture_encoder, _capture_encoded)) = oink_audio::source::channel(config, 100, false) else {
            emit(&serde_json::json!({"stage": "channel_error"}));
            std::process::exit(2);
        };
        emit(&serde_json::json!({"stage": "pre_capture"}));
        let capture = match oink_audio::microphone::Capture::start(
            &source,
            oink_audio::microphone::StartPermit {
                user_enabled: true,
                receiver_has_consumer: true,
                audible: true,
            },
            capture_encoder,
        ) {
            Ok(capture) => capture,
            Err(error) => {
                emit(&serde_json::json!({"stage": "capture_start_error", "error": error}));
                std::process::exit(2);
            }
        };
        let info = capture.info().clone();
        emit(&serde_json::json!({
            "stage": "capture_started",
            "device": info.device.id,
            "backend": info.backend,
            "hardware_sample_rate": info.hardware_sample_rate,
            "hardware_channels": info.hardware_channels,
            "resampler_delay_frames": info.resampler_delay_frames,
        }));
        // Feed the virtual render sink so the foreign recorder sees nonzero PCM.
        let mut sink = match oink_audio::sink::Sink::start(&render_id) {
            Ok(sink) => sink,
            Err(error) => {
                emit(&serde_json::json!({"stage": "sink_start_error", "error": error}));
                std::process::exit(2);
            }
        };
        let mut source = match sink.source(config) {
            Ok(source) => source,
            Err(error) => {
                emit(&serde_json::json!({"stage": "source_error", "error": error}));
                std::process::exit(2);
            }
        };
        let Ok((mut encoder, mut encoded)) = oink_audio::source::channel(config, 100, false) else {
            emit(&serde_json::json!({"stage": "render_channel_error"}));
            std::process::exit(2);
        };
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
                "capture_error": capture.error(),
                "sink_error": sink.error(),
                "capture_invalidated": capture.device_invalidated(),
                "rendered_dropped": metrics.attribution_dropped,
                "underrun_frames": metrics.underrun_frames,
            }));
        }
        feeder.join().unwrap_or(());
        let demand_after = monitor.sample();
        emit(&serde_json::json!({
            "stage": "demand_during_capture",
            "active": demand_after.active,
            "epoch": demand_after.epoch,
            "error": demand_after.error,
        }));
        drop(sink);
        drop(capture);
        if let Ok(mut child) = foreign {
            let _ = child.kill();
            let _ = child.wait();
        }
        // After the foreign recorder exits, demand must go quiet.
        let mut quiet = false;
        for _ in 0..20 {
            let demand = monitor.sample();
            if !demand.active {
                quiet = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        emit(&serde_json::json!({
            "stage": "demand_after_foreign_exit",
            "quiet": quiet,
        }));
        emit(&serde_json::json!({"stage": "finished", "error": null}));
    }
}
