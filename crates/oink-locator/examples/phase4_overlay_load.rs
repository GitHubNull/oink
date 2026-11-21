//! Phase 4 mixed-load driver: cycles the three Linux overlay surfaces for a
//! bounded duration inside the private Xephyr harness. It never renders user
//! content, only fixed labels, and never injects input.

use std::time::{Duration, Instant};

use oink_display::{layout::Side, projection::Projection};
use oink_foundation::{DeviceId, Point, Rect, display::DisplayInfo};
use oink_locator::{
    CursorTrail,
    notification::{NotificationOverlay, Toast},
    projection::{NamedProjection, ProjectionOverlay, View},
};

struct Args {
    seconds: u64,
    width: f64,
    height: f64,
}

fn parse_args() -> Args {
    let mut args = Args {
        seconds: 1800,
        width: 1280.0,
        height: 800.0,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let value = iter.next();
        match (flag.as_str(), value) {
            ("--seconds", Some(v)) => args.seconds = v.parse().unwrap_or(args.seconds),
            ("--width", Some(v)) => args.width = v.parse().unwrap_or(args.width),
            ("--height", Some(v)) => args.height = v.parse().unwrap_or(args.height),
            _ => {}
        }
    }
    args
}

fn display(args: &Args) -> DisplayInfo {
    DisplayInfo {
        key: "load".into(),
        name: "load".into(),
        bounds: Rect::new(0., 0., args.width, args.height),
        pixel_width: args.width as u32,
        pixel_height: args.height as u32,
        reference_extent: None,
        rotation_deg: 0,
        is_primary: true,
        is_builtin: false,
        is_virtual: false,
    }
}

fn toast(args: &Args) -> Toast {
    Toast {
        title: "负载通知".into(),
        description: "仅用于界面压力测试".into(),
        undo: None,
        persistent: false,
        display: display(args),
        scale: 1.0,
    }
}

fn view(args: &Args) -> View {
    let screen = display(args).bounds;
    let host = DeviceId([7; 32]);
    let portal = Rect::new(0., screen.h * 0.25, 24., screen.h * 0.5);
    let neighbour = Rect::new(portal.x - 320., portal.y, 320., portal.h);
    View {
        projections: vec![NamedProjection {
            geometry: Projection {
                host: oink_display::layout::ScreenId::new(host, "load"),
                neighbour: oink_display::layout::ScreenId::new(DeviceId([8; 32]), "load-peer"),
                side: Side::Left,
                rect: neighbour,
                clip: Rect::new(0., portal.y, portal.w, portal.h),
                visible: portal,
            },
            name: "负载投影".into(),
            pixels_per_unit: 1.0,
            screen,
        }],
        accent: [255, 107, 142],
    }
}

fn main() {
    let args = parse_args();
    let Some(trail) = CursorTrail::platform() else {
        println!("phase4_overlay_load=unavailable trail");
        std::process::exit(2);
    };
    let Some(notifications) = NotificationOverlay::platform(|_| {}) else {
        println!("phase4_overlay_load=unavailable notification");
        std::process::exit(2);
    };
    let Some(projection) = ProjectionOverlay::platform() else {
        println!("phase4_overlay_load=unavailable projection");
        std::process::exit(2);
    };
    trail.set_displays(vec![display(&args)]);
    trail.set_active(true);

    let start = Instant::now();
    let deadline = start + Duration::from_secs(args.seconds);
    let mut notice_at = start + Duration::from_secs(2);
    let mut notice_id: Option<u64> = None;
    let mut toggle_at = start + Duration::from_secs(6);
    let mut projection_visible = false;
    let mut paused = false;
    let mut pause_until = start;
    let mut report_at = start;
    let mut ticks = 0u64;

    while Instant::now() < deadline {
        let now = Instant::now();
        let t = now.duration_since(start).as_secs_f64();

        let x = (t * 190.0).rem_euclid(args.width - 120.) + 60.;
        let y = args.height / 2. + (t * 2.1).sin() * (args.height / 4.);
        trail.pointer(Point::new(x, y));

        if paused && now >= pause_until {
            paused = false;
            trail.set_paused(false);
            trail.set_active(true);
            println!("phase4_overlay_load trail=resumed t={t:.0}");
        } else if !paused && now.duration_since(start).as_secs() % 90 >= 84 {
            paused = true;
            pause_until = now + Duration::from_secs(6);
            trail.set_paused(true);
            println!("phase4_overlay_load trail=paused t={t:.0}");
        }

        if now >= notice_at {
            if let Some(id) = notice_id.take() {
                notifications.dismiss(id);
            }
            notice_id = Some(notifications.push(toast(&args)));
            notice_at = now + Duration::from_secs(5);
        }

        if now >= toggle_at {
            projection_visible = !projection_visible;
            if projection_visible {
                projection.update(view(&args));
            } else {
                projection.hide();
            }
            toggle_at = now + Duration::from_secs(6);
            println!("phase4_overlay_load projection={projection_visible} t={t:.0}");
        }

        if now >= report_at {
            ticks += 1;
            let stats = trail.stats();
            println!(
                "phase4_overlay_load tick={ticks} t={t:.0} trail_frames={} notification={} projection={}",
                stats.frames,
                notice_id.is_some(),
                projection_visible
            );
            report_at = now + Duration::from_secs(10);
        }

        std::thread::sleep(Duration::from_millis(24));
    }

    if let Some(id) = notice_id {
        notifications.dismiss(id);
    }
    notifications.clear();
    projection.hide();
    projection.shutdown();
    trail.set_paused(true);
    trail.shutdown();
    println!("phase4_overlay_load=ok seconds={} ticks={ticks}", args.seconds);
}
