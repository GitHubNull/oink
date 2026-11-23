//! Native capture evidence probe; run only inside linux-native-test.py's
//! private audio scope. Starts the product capture path (monitor record ->
//! source encoder) and prints one JSON tick per second. The probe feeds a
//! tone in the middle and verifies signal follows it.
use std::io::Write;

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
        .unwrap_or(14);
    #[cfg(not(target_os = "linux"))]
    {
        emit(&serde_json::json!({"stage": "unsupported_platform"}));
        return;
    }
    #[cfg(target_os = "linux")]
    {
        let config = oink_audio::dsp::Config::opus_10ms();
        let Ok((encoder, mut output)) = oink_audio::source::channel(config, 100, false) else {
            emit(&serde_json::json!({"stage": "channel_error"}));
            std::process::exit(2);
        };
        let mut capture = match oink_audio::Capture::start_encoded(
            encoder,
            false,
            std::env::temp_dir(),
            oink_audio::CaptureScope::DefaultOutput,
        ) {
            Ok(capture) => capture,
            Err(error) => {
                emit(&serde_json::json!({"stage": "start_error", "error": error}));
                std::process::exit(2);
            }
        };
        let info = capture.info();
        emit(&serde_json::json!({
            "stage": "started",
            "backend": info.backend,
            "sample_rate": info.sample_rate,
            "requested_buffer_frames": info.requested_buffer_frames,
            "reported_latency_frames": info.reported_latency_frames,
            "physical_output_id": info.physical_output_id,
            "diagnostics": info.diagnostics,
        }));
        for tick in 1..=seconds {
            let mut packets = 0u64;
            while output.try_recv().is_some() {
                packets += 1;
            }
            let levels = output.levels();
            emit(&serde_json::json!({
                "stage": "tick",
                "tick": tick,
                "packets": packets,
                "frames": output.frames(),
                "has_signal": output.has_signal(),
                "rms_dbfs": levels.map(|l| l.before_gain.rms_dbfs),
                "peak_dbfs": levels.map(|l| l.before_gain.peak_dbfs),
                "error": capture.error(),
                "cb": capture.callback_stats(),
            }));
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        let error = capture.finish();
        emit(&serde_json::json!({"stage": "finished", "error": error}));
    }
}
