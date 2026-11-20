//! 运行探针：只在私有 Xephyr 中执行，验证 raw 观察、自身注入过滤与启停生命周期。
use super::*;

use crate::{CapturedEvent, InputEvent};
use oink_foundation::input::{InjectEvent, KeyCode};
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Counters {
    events: AtomicUsize,
    keys: AtomicUsize,
    motion: AtomicUsize,
    available_false: AtomicUsize,
}

struct Probe {
    counters: Arc<Counters>,
}

impl CaptureLoop for Probe {
    fn set_wake(&mut self, _: Wake) {}
    fn event(&mut self, event: &CapturedEvent) -> crate::Disposition {
        self.counters.events.fetch_add(1, Ordering::Relaxed);
        match event.event {
            InputEvent::Key { .. } => self.counters.keys.fetch_add(1, Ordering::Relaxed),
            InputEvent::PointerDelta { .. } => self.counters.motion.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
        crate::Disposition::Pass
    }
    fn poll(&mut self, _: Instant) -> Option<Instant> {
        None
    }
    fn stopped(&self) -> bool {
        false
    }
    fn capture_available(&mut self, available: bool) {
        if !available {
            self.counters.available_false.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn wait_until(counts: &Counters, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if counts.keys.load(Ordering::Relaxed) > 0 && counts.motion.load(Ordering::Relaxed) > 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// 只允许在私有 Xephyr 中运行；绝不对宿主 DISPLAY=:0 注入。
#[test]
#[ignore = "需要私有 Xephyr：DISPLAY=:0 python3 build-support/linux-native-test.py --timeout 120 -- \
                cargo test -p oink-input linux::capture::tests::xephyr -- --ignored --test-threads=1"]
fn xephyr_raw_observation_translates_events_and_filters_own_xtest() {
    if std::env::var_os("DISPLAY").is_none() {
        return;
    }
    let Ok(backend) = crate::linux::backend() else {
        return;
    };
    // 第一步允许 XTEST 来源穿过过滤：证明 raw → 翻译 → owner 的完整链路。
    event::ACCEPT_XTEST.store(true, Ordering::Relaxed);
    let counters = Arc::new(Counters::default());
    backend
        .start_loop(Box::new(Probe {
            counters: Arc::clone(&counters),
        }))
        .expect("截获线程启动");
    backend
        .inject(&InjectEvent::Key {
            code: KeyCode::keyboard(0x04),
            pressed: true,
        })
        .expect("注入按下");
    backend
        .inject(&InjectEvent::Key {
            code: KeyCode::keyboard(0x04),
            pressed: false,
        })
        .expect("注入抬起");
    backend
        .inject(&InjectEvent::MoveRelative { dx: 5.0, dy: 3.0 })
        .expect("注入相对位移");
    let delivered = wait_until(&counters, Instant::now() + Duration::from_millis(1500));
    backend.stop();
    assert!(
        delivered,
        "keys={} motion={}",
        counters.keys.load(Ordering::Relaxed),
        counters.motion.load(Ordering::Relaxed)
    );
    assert_eq!(counters.available_false.load(Ordering::Relaxed), 1, "W2 如实上报未接管");

    // 第二步恢复默认过滤：同样的 XTEST 注入不得再进入 owner。
    event::ACCEPT_XTEST.store(false, Ordering::Relaxed);
    let filtered = Arc::new(Counters::default());
    backend
        .start_loop(Box::new(Probe {
            counters: Arc::clone(&filtered),
        }))
        .expect("截获线程重启");
    backend
        .inject(&InjectEvent::Key {
            code: KeyCode::keyboard(0x04),
            pressed: true,
        })
        .expect("注入按下");
    backend
        .inject(&InjectEvent::Key {
            code: KeyCode::keyboard(0x04),
            pressed: false,
        })
        .expect("注入抬起");
    backend
        .inject(&InjectEvent::MoveRelative { dx: 4.0, dy: 2.0 })
        .expect("注入相对位移");
    std::thread::sleep(Duration::from_millis(250));
    backend.stop();
    assert_eq!(filtered.events.load(Ordering::Relaxed), 0, "XTEST 来源必须被丢弃");
    assert_eq!(filtered.available_false.load(Ordering::Relaxed), 1);
}
