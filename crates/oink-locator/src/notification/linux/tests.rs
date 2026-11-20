use super::*;
use crate::notification::{Queue, Style, Toast};
use crate::text;
use crate::x11::probe::Probe;
use oink_foundation::{Rect, display::DisplayInfo};
use std::time::{Duration, Instant};
use x11_dl::xlib as xl;

fn frame(w: usize, h: usize, lit: impl Fn(usize, usize) -> bool) -> Frame {
    let mut pixels = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            if lit(x, y) {
                pixels[y * w + x] = 0xff00_0000;
            }
        }
    }
    Frame {
        id: 1,
        bounds: Rect::new(0., 0., w as f64, h as f64),
        card: Rect::new(0., 0., w as f64, h as f64),
        width: w,
        height: h,
        pixels,
        undo: Some(Rect::new(1., 0., 1., 1.)),
        interactive: true,
    }
}

#[test]
fn transparent_padding_and_rows_are_excluded_from_the_input_shape() {
    // 形状：第 1–2 行只有中间两列；第 3 行两侧都有；透明角落不占输入。
    let f = frame(5, 5, |x, y| match y {
        1 | 2 => x == 2,
        3 => x == 1 || x == 3,
        _ => false,
    });
    let shape = card_shape(&f);
    assert_eq!(
        shape,
        vec![
            xlib::XRectangle {
                x: 2,
                y: 1,
                width: 1,
                height: 2
            },
            xlib::XRectangle {
                x: 1,
                y: 3,
                width: 1,
                height: 1
            },
            xlib::XRectangle {
                x: 3,
                y: 3,
                width: 1,
                height: 1
            },
        ]
    );
    assert!(card_shape(&frame(4, 4, |_, _| false)).is_empty());
    assert_eq!(card_shape(&frame(0, 4, |_, _| true)), Vec::new());
}

#[test]
fn alpha_zero_pixels_never_own_input_even_inside_the_card_rect() {
    let f = frame(3, 1, |x, _| x != 1);
    assert_eq!(
        card_shape(&f),
        vec![
            xlib::XRectangle {
                x: 0,
                y: 0,
                width: 1,
                height: 1
            },
            xlib::XRectangle {
                x: 2,
                y: 0,
                width: 1,
                height: 1
            },
        ],
        "a hole in the middle stays click-through"
    );
}

fn display_info() -> DisplayInfo {
    DisplayInfo {
        key: "xephyr".into(),
        name: "Xephyr".into(),
        bounds: Rect::new(0., 0., 1280., 800.),
        pixel_width: 1280,
        pixel_height: 800,
        reference_extent: None,
        rotation_deg: 0,
        is_primary: true,
        is_builtin: false,
        is_virtual: false,
    }
}

fn toast_frame() -> Frame {
    let mut queue = Queue::default();
    queue.push(
        Toast {
            title: "已禁用本机键盘".into(),
            description: "按住 Ctrl+Alt+Shift+U 1 秒解除".into(),
            undo: Some("撤销".into()),
            persistent: true,
            scale: 1.0,
            display: display_info(),
        },
        Instant::now(),
    );
    let font = text::system_toast();
    let mut frames = queue.frames(
        Instant::now() + Duration::from_millis(1000),
        Style::default(),
        true,
        &*font,
    );
    frames.remove(0)
}

/// 第二条连接：读回窗口像素、观察根窗口事件并注入 XTest 指针。
#[test]
#[ignore = "需要私有 Xephyr：DISPLAY=:0 python3 build-support/linux-native-test.py --timeout 300 -- cargo test -p oink-locator notification::linux::tests::xephyr_ -- --ignored --test-threads=1"]
fn xephyr_notification_argb_chinese_and_input_shape() {
    let frame = toast_frame();
    assert!(frame.interactive);
    let mut backend = Backend::new().expect("notification backend on the private server");
    let probe = Probe::open();
    let focus_before = probe.focus();

    backend.present(vec![frame.clone()]);
    backend.connection.sync();
    probe.sync();

    let window = backend.windows.first().expect("toast window").overlay.id();
    let attributes = probe.attributes(window).expect("XGetWindowAttributes");
    assert_eq!(attributes.depth, 32, "ARGB visual");
    assert_ne!(attributes.override_redirect, 0, "override-redirect");
    assert_eq!(attributes.map_state, xl::IsViewable, "mapped");
    assert_eq!(
        (attributes.x, attributes.y, attributes.width, attributes.height),
        (
            frame.bounds.x.round() as i32,
            frame.bounds.y.round() as i32,
            frame.width as i32,
            frame.height as i32
        )
    );

    let image = probe.window_image(window).expect("XGetImage");
    let (image_width, image_height) = image.size();
    let (mut opaque, mut ink, mut corner) = (0usize, 0usize, None);
    for y in 0..image_height {
        for x in 0..image_width {
            let pixel = image.pixel(x, y);
            let alpha = pixel >> 24;
            if alpha == 0 {
                corner = corner.or(Some((x, y)));
                continue;
            }
            opaque += 1;
            let (r, g, b) = ((pixel >> 16) & 255, (pixel >> 8) & 255, pixel & 255);
            if alpha > 200 && r < 160 && g < 160 && b < 160 {
                ink += 1;
            }
        }
    }
    assert!(opaque > frame.width * frame.height / 2, "opaque card pixels: {opaque}");
    assert!(ink > 200, "dark Chinese glyph pixels in the window: {ink}");
    let (corner_x, corner_y) = corner.expect("rounded corner stays transparent");

    assert_eq!(focus_before, probe.focus(), "mapping must not take focus");

    let (origin_x, origin_y) = (frame.bounds.x.round() as i32, frame.bounds.y.round() as i32);
    probe.move_to(origin_x + corner_x, origin_y + corner_y);
    probe.sync();
    probe.click();
    probe.sync();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut through = false;
    while !through && Instant::now() < deadline {
        through = probe.take_root_press();
        if !through {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(through, "a transparent corner must pass clicks to the root window");
    assert!(backend.clicks().is_empty(), "transparent pixels must not own input");

    let (click_x, click_y) = ((frame.width / 2) as i32, (frame.height / 2) as i32);
    probe.move_to(origin_x + click_x, origin_y + click_y);
    probe.sync();
    probe.click();
    probe.sync();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut clicks = Vec::new();
    while clicks.is_empty() && Instant::now() < deadline {
        clicks = backend.clicks();
        if clicks.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert_eq!(
        clicks,
        vec![Click {
            id: frame.id,
            undo: false
        }]
    );
    assert_eq!(focus_before, probe.focus(), "clicks must not take focus");

    println!(
        "notification_argb_geometry=ok depth={} override_redirect={} mapped={} size={}x{}",
        attributes.depth, attributes.override_redirect, attributes.map_state, attributes.width, attributes.height
    );
    println!("notification_alpha_chinese=ok opaque={opaque} ink={ink}");
    println!("notification_input_shape=ok corner=({corner_x},{corner_y}) passed_to_root={through}");
    println!("notification_card_click=ok undo={}", clicks[0].undo);
    println!(
        "notification_focus=ok window={} revert={}",
        focus_before.0, focus_before.1
    );

    backend.present(Vec::new());
    probe.sync();
}
