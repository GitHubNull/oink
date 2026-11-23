//! Native volume endpoint evidence probe; run only inside linux-native-test.py's
//! private audio scope. Opens the product Linux volume endpoint on the test
//! sink, applies an absolute write, observes an external change through the
//! subscribe latch, verifies a missing sink fails closed, then restores the
//! original reading. Only percents/mute flags are recorded, never audio.
use std::io::{BufRead, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn emit(value: &serde_json::Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

fn reading_json(reading: &oink_audio::volume::Reading) -> serde_json::Value {
    serde_json::json!({"percent": reading.percent, "muted": reading.muted})
}

fn main() {
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
        let wakes = Arc::new(AtomicU64::new(0));
        let wake_wakes = wakes.clone();
        let wake = Arc::new(move || {
            wake_wakes.fetch_add(1, Ordering::Relaxed);
        });
        let mut endpoint = match oink_audio::volume::open(&output, wake) {
            Ok(endpoint) => endpoint,
            Err(fault) => {
                emit(&serde_json::json!({"stage": "open_error", "status": format!("{:?}", fault.status)}));
                std::process::exit(2);
            }
        };
        let initial = match endpoint.read() {
            Ok(reading) => reading,
            Err(fault) => {
                emit(&serde_json::json!({"stage": "read_error", "status": format!("{:?}", fault.status)}));
                std::process::exit(2);
            }
        };
        emit(&serde_json::json!({"stage": "initial", "reading": reading_json(&initial)}));
        // Absolute write: 40%, muted. The owner layer would fence this with
        // claim/generation/revision; here we exercise the endpoint directly.
        if let Err(fault) = endpoint.write(oink_audio::volume::Reading {
            percent: 40,
            muted: true,
        }) {
            emit(&serde_json::json!({"stage": "write_error", "status": format!("{:?}", fault.status)}));
            std::process::exit(2);
        }
        let after_write = endpoint.read().expect("readback after write");
        emit(&serde_json::json!({"stage": "write_ack", "reading": reading_json(&after_write)}));
        // Wait for the probe to apply an external change with pactl.
        let mut stdin = std::io::stdin().lock();
        let mut line = String::new();
        let _ = stdin.read_line(&mut line);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut latched = false;
        while Instant::now() < deadline {
            if endpoint.changed() {
                latched = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let external = endpoint.read().expect("readback after external change");
        emit(&serde_json::json!({
            "stage": "external",
            "latched": latched,
            "wakes": wakes.load(Ordering::Relaxed),
            "reading": reading_json(&external),
        }));
        // A sink that does not exist must fail closed, never guess.
        let missing =
            oink_audio::volume::open("pulse:sink:oink_missing_probe", Arc::new(|| {})).expect("open missing endpoint");
        let mut missing = missing;
        let missing_result = missing.read();
        emit(&serde_json::json!({
            "stage": "missing_sink",
            "error": missing_result.err().map(|fault| format!("{:?}", fault.status)),
        }));
        // Restore the lease: put the original reading back and report it.
        if let Err(fault) = endpoint.write(initial) {
            emit(&serde_json::json!({"stage": "restore_error", "status": format!("{:?}", fault.status)}));
            std::process::exit(2);
        }
        let restored = endpoint.read().expect("readback after restore");
        emit(&serde_json::json!({"stage": "restored", "reading": reading_json(&restored)}));
        emit(&serde_json::json!({"stage": "finished", "error": null}));
    }
}
