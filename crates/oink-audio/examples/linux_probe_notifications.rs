//! Native device-notification evidence probe; run only inside
//! linux-native-test.py's private audio scope. Prints one JSON tick per
//! second for `--seconds N`; the probe loads/unloads a module in between and
//! verifies the generation advanced and the wake callback fired.
use std::io::Write;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

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
        .unwrap_or(10);
    #[cfg(not(target_os = "linux"))]
    {
        emit(&serde_json::json!({"stage": "unsupported_platform"}));
        return;
    }
    #[cfg(target_os = "linux")]
    {
        let wakes = Arc::new(AtomicU64::new(0));
        let wakes_for_callback = wakes.clone();
        let _subscription = oink_audio::device_notifications::subscribe(Arc::new(move || {
            wakes_for_callback.fetch_add(1, Ordering::Release);
        }));
        let initial_generation = oink_audio::device_notifications::generation();
        emit(&serde_json::json!({
            "stage": "initial",
            "generation": initial_generation,
            "service_generation": oink_audio::device_notifications::service_generation(),
        }));
        for tick in 0..seconds {
            std::thread::sleep(Duration::from_secs(1));
            emit(&serde_json::json!({
                "stage": "tick",
                "tick": tick + 1,
                "generation": oink_audio::device_notifications::generation(),
                "wakes": wakes.load(Ordering::Acquire),
            }));
        }
    }
}
