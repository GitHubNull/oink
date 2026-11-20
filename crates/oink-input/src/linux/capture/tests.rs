//! 运行探针：只在私有 Xephyr 中执行，验证 raw 观察、暂停/恢复、Remote 抓取与锁屏门控。
use super::*;

use crate::{CapturedEvent, InputEvent};
use oink_foundation::input::{InjectEvent, KeyCode};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Counters {
    events: AtomicUsize,
    keys: AtomicUsize,
    motion: AtomicUsize,
    available_true: AtomicUsize,
    available_false: AtomicUsize,
}

struct Probe {
    counters: Arc<Counters>,
    paused: Option<crate::capture_loop::ResumeHotkey>,
    resumes: Arc<AtomicUsize>,
}

impl Probe {
    fn plain(counters: &Arc<Counters>) -> Self {
        Self {
            counters: Arc::clone(counters),
            paused: None,
            resumes: Arc::new(AtomicUsize::new(0)),
        }
    }
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
    fn paused_hotkey(&self) -> Option<crate::capture_loop::ResumeHotkey> {
        self.paused
    }
    fn resume_hotkey(&mut self) {
        self.resumes.fetch_add(1, Ordering::Relaxed);
    }
    fn capture_available(&mut self, available: bool) {
        if available {
            self.counters.available_true.fetch_add(1, Ordering::Relaxed);
        } else {
            self.counters.available_false.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn wait_for(counter: &AtomicUsize, at_least: usize, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if counter.load(Ordering::Relaxed) >= at_least {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn wait_position(backend: &X11Input, target: Point, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(point) = backend.cursor_position()
            && (point.x - target.x).abs() <= 1.0
            && (point.y - target.y).abs() <= 1.0
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn tap(backend: &X11Input, usage: u16) {
    for pressed in [true, false] {
        backend
            .inject(&InjectEvent::Key {
                code: KeyCode::keyboard(usage),
                pressed,
            })
            .expect("注入按键");
    }
}

/// 只允许在私有 Xephyr 中运行；绝不对宿主 DISPLAY=:0 注入。
#[test]
#[ignore = "需要私有 Xephyr：DISPLAY=:0 python3 build-support/linux-native-test.py --timeout 120 -- \
                cargo test -p oink-input linux::capture::tests::xephyr -- --ignored --test-threads=1"]
fn xephyr_raw_observation_translates_events_and_filters_own_xtest() {
    let Ok(backend) = crate::linux::test_backend() else {
        return;
    };
    // 第一步允许 XTEST 来源穿过过滤：证明 raw → 翻译 → owner 的完整链路。
    event::ACCEPT_XTEST.store(true, Ordering::Relaxed);
    let counters = Arc::new(Counters::default());
    backend
        .start_loop(Box::new(Probe::plain(&counters)))
        .expect("截获线程启动");
    tap(&backend, 0x04);
    backend
        .inject(&InjectEvent::MoveRelative { dx: 5.0, dy: 3.0 })
        .expect("注入相对位移");
    let delivered = wait_for(&counters.keys, 1, Duration::from_millis(1500))
        && wait_for(&counters.motion, 1, Duration::from_millis(1500));
    backend.stop();
    assert!(
        delivered,
        "keys={} motion={}",
        counters.keys.load(Ordering::Relaxed),
        counters.motion.load(Ordering::Relaxed)
    );
    assert_eq!(counters.available_true.load(Ordering::Relaxed), 1, "W3 上报真实接管");
    assert_eq!(counters.available_false.load(Ordering::Relaxed), 1, "停止时撤销能力");

    // 第二步恢复默认过滤：同样的 XTEST 注入不得再进入 owner。
    event::ACCEPT_XTEST.store(false, Ordering::Relaxed);
    let filtered = Arc::new(Counters::default());
    backend
        .start_loop(Box::new(Probe::plain(&filtered)))
        .expect("截获线程重启");
    tap(&backend, 0x04);
    backend
        .inject(&InjectEvent::MoveRelative { dx: 4.0, dy: 2.0 })
        .expect("注入相对位移");
    std::thread::sleep(Duration::from_millis(250));
    backend.stop();
    assert_eq!(filtered.events.load(Ordering::Relaxed), 0, "XTEST 来源必须被丢弃");
    assert_eq!(filtered.available_false.load(Ordering::Relaxed), 1);
}

#[test]
#[ignore = "需要私有 Xephyr：见 xephyr_raw_observation..."]
fn xephyr_pause_withdraws_routing_and_resume_chord_reenables() {
    let Ok(backend) = crate::linux::test_backend() else {
        return;
    };
    backend.force_lock_state(Some(false));
    event::ACCEPT_XTEST.store(true, Ordering::Relaxed);
    let counters = Arc::new(Counters::default());
    let resumes = Arc::new(AtomicUsize::new(0));
    backend
        .start_loop(Box::new(Probe {
            counters: Arc::clone(&counters),
            paused: Some(crate::capture_loop::ResumeHotkey {
                key: KeyCode::keyboard(0x29),
                control: false,
                alt: false,
                shift: false,
                meta: false,
            }),
            resumes: Arc::clone(&resumes),
        }))
        .expect("截获线程启动");
    tap(&backend, 0x04);
    assert!(
        wait_for(&counters.keys, 1, Duration::from_millis(1500)),
        "暂停前应路由按键"
    );

    let baseline = counters.keys.load(Ordering::Relaxed);
    backend.set_input_paused(true);
    std::thread::sleep(Duration::from_millis(200));
    tap(&backend, 0x05);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        counters.keys.load(Ordering::Relaxed),
        baseline,
        "暂停期间不得路由普通按键"
    );

    tap(&backend, 0x29);
    assert!(
        wait_for(&resumes, 1, Duration::from_millis(1500)),
        "紧急恢复组合必须命中并回调 resume_hotkey"
    );

    backend.set_input_paused(false);
    std::thread::sleep(Duration::from_millis(200));
    tap(&backend, 0x06);
    assert!(
        wait_for(&counters.keys, baseline + 1, Duration::from_millis(1500)),
        "恢复后应重新路由按键"
    );
    backend.stop();
}

#[test]
#[ignore = "需要私有 Xephyr：见 xephyr_raw_observation..."]
fn xephyr_remote_engages_active_grab_and_restores_verified_local_cursor() {
    let Ok(backend) = crate::linux::test_backend() else {
        return;
    };
    backend.force_lock_state(Some(false));
    event::ACCEPT_XTEST.store(true, Ordering::Relaxed);
    let counters = Arc::new(Counters::default());
    backend
        .start_loop(Box::new(Probe::plain(&counters)))
        .expect("截获线程启动");
    let park = Point::new(40.0, 30.0);
    backend.set_mode(CaptureMode::Remote { park }).expect("进入 Remote");
    assert!(
        wait_position(&backend, park, Duration::from_millis(1500)),
        "Remote 应先隐藏并停靠指针"
    );

    backend
        .inject(&InjectEvent::MoveRelative { dx: 6.0, dy: 4.0 })
        .expect("Remote 相对位移");
    assert!(
        wait_for(&counters.motion, 1, Duration::from_millis(1500)),
        "主动抓取期间 raw 运动仍须路由"
    );

    let (tx, rx) = mpsc::channel();
    backend
        .restore_local_with_completion(
            Point::new(120.0, 90.0),
            Box::new(move |restored| {
                let _ = tx.send(restored);
            }),
        )
        .expect("提交换原请求");
    assert!(
        rx.recv_timeout(Duration::from_millis(2500)).expect("恢复完成回调"),
        "恢复必须经 XSync 后重读核验"
    );
    assert!(
        wait_position(&backend, Point::new(120.0, 90.0), Duration::from_millis(500)),
        "恢复后光标应位于 at"
    );
    backend.stop();
}

#[test]
#[ignore = "需要私有 Xephyr：见 xephyr_raw_observation..."]
fn xephyr_locked_and_unknown_state_stop_routing_until_unlocked() {
    let Ok(backend) = crate::linux::test_backend() else {
        return;
    };
    backend.force_lock_state(Some(false));
    event::ACCEPT_XTEST.store(true, Ordering::Relaxed);
    let counters = Arc::new(Counters::default());
    backend
        .start_loop(Box::new(Probe::plain(&counters)))
        .expect("截获线程启动");
    tap(&backend, 0x04);
    assert!(wait_for(&counters.keys, 1, Duration::from_millis(1500)));

    backend.force_lock_state(Some(true));
    std::thread::sleep(Duration::from_millis(250));
    let locked_baseline = counters.keys.load(Ordering::Relaxed);
    tap(&backend, 0x05);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        counters.keys.load(Ordering::Relaxed),
        locked_baseline,
        "锁定期间不得路由"
    );

    backend.force_lock_state(Some(false));
    std::thread::sleep(Duration::from_millis(250));
    tap(&backend, 0x06);
    assert!(wait_for(
        &counters.keys,
        locked_baseline + 1,
        Duration::from_millis(1500)
    ));

    backend.force_lock_state(None);
    std::thread::sleep(Duration::from_millis(250));
    let unknown_baseline = counters.keys.load(Ordering::Relaxed);
    tap(&backend, 0x07);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        counters.keys.load(Ordering::Relaxed),
        unknown_baseline,
        "未知锁定状态不得当作解锁"
    );
    backend.stop();
}

/// 私有总线上的假 logind；Drop 时恢复环境变量、丢弃总线缓存并回收子进程。
struct FakeLogind {
    bus: Child,
    service: Child,
    state: PathBuf,
    root: PathBuf,
    original: Option<String>,
    _bus_output: BufReader<std::process::ChildStdout>,
}

impl FakeLogind {
    fn set_locked(&self, locked: bool) {
        std::fs::write(&self.state, if locked { "1" } else { "0" }).expect("切换假 logind 状态");
    }
}

impl Drop for FakeLogind {
    fn drop(&mut self) {
        let _ = self.service.kill();
        let _ = self.service.wait();
        let _ = self.bus.kill();
        let _ = self.bus.wait();
        match &self.original {
            // SAFETY: 探针串行收尾，没有其他线程同时读写环境变量。
            Some(value) => unsafe { std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", value) },
            // SAFETY: 同上，探针串行收尾，环境变量无并发访问。
            None => unsafe { std::env::remove_var("DBUS_SYSTEM_BUS_ADDRESS") },
        }
        crate::linux::session::reset_bus_cache();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fake_logind() -> FakeLogind {
    let root = std::env::temp_dir().join(format!("oink-fake-logind-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("创建假 logind 目录");
    let state = root.join("locked");
    std::fs::write(&state, "0").expect("写入初始解锁状态");
    let mut bus = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .arg(format!("--address=unix:dir={}", root.display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动私有 D-Bus");
    let mut bus_output = BufReader::new(bus.stdout.take().expect("读取 D-Bus 地址"));
    let mut address = String::new();
    bus_output.read_line(&mut address).expect("读取 D-Bus 地址");
    let address = address.trim().to_string();
    assert!(address.starts_with("unix:"), "私有 D-Bus 地址无效：{address:?}");
    let service = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../build-support/fake_logind_service.py"
        ))
        .arg(&address)
        .arg(&state)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动假 logind 服务");
    let mut service = service;
    let mut service_output = BufReader::new(service.stdout.take().expect("读取假 logind 输出"));
    let mut ready = String::new();
    service_output.read_line(&mut ready).expect("等待假 logind 就绪");
    assert_eq!(ready.trim(), "ready", "假 logind 未就绪");
    let original = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").ok();
    // SAFETY: 探针串行运行，且在任何系统总线连接建立前设置环境变量。
    unsafe { std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &address) };
    crate::linux::session::reset_bus_cache();
    FakeLogind {
        bus,
        service,
        state,
        root,
        original,
        _bus_output: bus_output,
    }
}

/// 假 logind 切换锁定：锁定必须释放主动抓取，解锁后重建 Remote 路由。
#[test]
#[ignore = "需要私有 Xephyr + dbus-daemon/PyGObject：DISPLAY=:0 python3 build-support/linux-native-test.py --timeout 180 -- \
                cargo test -p oink-input linux::capture::tests::xephyr_fake_logind -- --ignored --test-threads=1"]
fn xephyr_fake_logind_lock_releases_remote_and_unlock_resumes() {
    let fake = fake_logind();
    let Ok(backend) = crate::linux::test_backend() else {
        return;
    };
    assert_eq!(backend.observe_lock(), Some(false), "假 logind 应报告解锁");
    event::ACCEPT_XTEST.store(true, Ordering::Relaxed);
    let counters = Arc::new(Counters::default());
    backend
        .start_loop(Box::new(Probe::plain(&counters)))
        .expect("截获线程启动");
    tap(&backend, 0x04);
    assert!(
        wait_for(&counters.keys, 1, Duration::from_millis(1500)),
        "解锁状态应路由按键"
    );

    let park = Point::new(35.0, 25.0);
    backend.set_mode(CaptureMode::Remote { park }).expect("进入 Remote");
    assert!(
        wait_position(&backend, park, Duration::from_millis(1500)),
        "Remote 应停靠指针"
    );
    let motion_before = counters.motion.load(Ordering::Relaxed);
    backend
        .inject(&InjectEvent::MoveRelative { dx: 5.0, dy: 3.0 })
        .expect("注入相对位移");
    assert!(
        wait_for(&counters.motion, motion_before + 1, Duration::from_millis(1500)),
        "Remote 应路由 raw 运动"
    );

    fake.set_locked(true);
    assert_eq!(backend.observe_lock(), Some(true), "假 logind 锁定必须被观察到");
    std::thread::sleep(Duration::from_millis(400));
    let keys = counters.keys.load(Ordering::Relaxed);
    let motion = counters.motion.load(Ordering::Relaxed);
    tap(&backend, 0x05);
    backend
        .inject(&InjectEvent::MoveRelative { dx: 4.0, dy: 2.0 })
        .expect("锁定期间注入");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(counters.keys.load(Ordering::Relaxed), keys, "锁定后不得路由按键");
    assert_eq!(
        counters.motion.load(Ordering::Relaxed),
        motion,
        "锁定后必须释放主动抓取"
    );

    fake.set_locked(false);
    assert_eq!(backend.observe_lock(), Some(false), "假 logind 解锁必须被观察到");
    let deadline = Instant::now() + Duration::from_millis(3000);
    while counters.motion.load(Ordering::Relaxed) == motion && Instant::now() < deadline {
        backend
            .inject(&InjectEvent::MoveRelative { dx: 1.0, dy: 1.0 })
            .expect("解锁后注入");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        counters.motion.load(Ordering::Relaxed) > motion,
        "解锁后应重建抓取并恢复路由"
    );
    backend.stop();
}
