//! Native enumeration evidence probe; run only inside linux-native-test.py's
//! private audio scope. Emits JSON lines; never prints audio content.
use std::io::Write;

fn emit(value: &serde_json::Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

fn main() {
    #[cfg(not(target_os = "linux"))]
    {
        emit(&serde_json::json!({"stage": "unsupported_platform"}));
        return;
    }
    #[cfg(target_os = "linux")]
    {
        match oink_audio::playback::outputs() {
            Ok(outputs) => {
                emit(&serde_json::json!({"stage": "outputs", "count": outputs.len()}));
                for output in &outputs {
                    emit(&serde_json::json!({
                        "stage": "output",
                        "id": output.id,
                        "name": output.name,
                        "is_default": output.is_default,
                    }));
                }
            }
            Err(error) => emit(&serde_json::json!({"stage": "outputs_error", "error": error})),
        }
        match oink_audio::capture_sample_rate() {
            Ok(rate) => emit(&serde_json::json!({"stage": "default_sink_rate", "rate": rate})),
            Err(error) => emit(&serde_json::json!({"stage": "default_sink_rate_error", "error": error})),
        }
        emit(&serde_json::json!({"stage": "enumerate_done"}));
    }
}
