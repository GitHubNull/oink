//! Linux 布局投影：每个可见投影一个 ARGB 窗口，输入形状为空、完全不抢焦点。
//!
//! 渲染线程独占连接；窗口只在有帧时创建，`hide`/空帧直接销毁。Expose 用缓存的
//! XShm/XImage 缓冲重传，不重新生成像素。

use std::rc::Rc;

use x11_dl::xlib;

use super::{Frame, Rect};
use crate::effect::CellRect;
use crate::x11::{Connection, OverlayWindow};

pub(super) struct Backend {
    connection: Rc<Connection>,
    windows: Vec<OverlayWindow>,
}

impl Backend {
    pub(super) fn new(_health: std::sync::Arc<crate::diagnostics::Health>) -> Option<Self> {
        let connection = Connection::open()?;
        Some(Self {
            connection,
            windows: Vec::new(),
        })
    }

    pub(super) fn present(&mut self, frames: Vec<Frame>) {
        self.windows.truncate(frames.len());
        while self.windows.len() < frames.len() {
            let frame = &frames[self.windows.len()];
            let Some(rect) = window_rect(frame) else {
                return;
            };
            let Some(window) = OverlayWindow::new(self.connection.clone(), rect) else {
                tracing::warn!("layout projection: cannot create the X11 overlay");
                return;
            };
            // Only redraws after Expose; this window never receives input.
            window.select_events(xlib::ExposureMask);
            self.windows.push(window);
        }
        for (window, frame) in self.windows.iter_mut().zip(frames) {
            let Some(rect) = window_rect(&frame) else {
                continue;
            };
            if !window.configure(rect) {
                continue;
            }
            // 服务器不保证保留映射前的窗口内容（Xephyr 实测会丢弃），先映射再上传。
            window.map_raised();
            if !window.upload(&frame.pixels) {
                window.unmap();
            }
        }
    }

    pub(super) fn hide(&mut self) {
        self.windows.clear();
    }

    pub(super) fn pump(&mut self) {
        let mut exposed = Vec::new();
        self.connection.drain_events(|event| {
            // SAFETY: XNextEvent 写入完整事件；先读 type_ 再按匹配成员解释联合体。
            if unsafe { event.type_ } == xlib::Expose {
                // SAFETY: type_ 已确认是 Expose，联合体的 expose 成员有效。
                let expose = unsafe { event.expose };
                // 多个 Expose 只重绘一次，等服务器报完。
                if expose.count == 0 {
                    exposed.push(expose.window);
                }
            }
        });
        for id in exposed {
            if let Some(window) = self.windows.iter_mut().find(|window| window.id() == id) {
                window.redraw();
            }
        }
    }
}

fn window_rect(frame: &Frame) -> Option<CellRect> {
    let (x, y) = (frame.bounds.x.round(), frame.bounds.y.round());
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let w = u32::try_from(frame.width).ok()?;
    let h = u32::try_from(frame.height).ok()?;
    if w == 0 || h == 0 || frame.pixels.len() != frame.width.checked_mul(frame.height)? {
        return None;
    }
    Some(CellRect {
        x: x as i64,
        y: y as i64,
        w,
        h,
    })
}

// X11 desktop coordinates are device pixels; density is not applied separately.
pub(super) fn annotation_unit(_screen: Rect) -> f64 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{Health, Kind};
    use crate::projection::Frame;
    use crate::x11::probe::Probe;
    use oink_foundation::Rect;
    use std::time::Instant;
    use x11_dl::xlib as xl;

    fn frame(bounds: Rect, w: usize, h: usize, pixel: u32) -> Frame {
        Frame {
            bounds,
            width: w,
            height: h,
            pixels: vec![pixel; w * h],
        }
    }

    #[test]
    #[ignore = "需要私有 Xephyr：DISPLAY=:0 python3 build-support/linux-native-test.py --timeout 300 -- cargo test -p oink-locator projection::linux::tests::xephyr_ -- --ignored --test-threads=1"]
    fn xephyr_projection_windows_upload_argb_and_pass_input() {
        let mut backend = Backend::new(Health::new(Kind::Projection)).expect("projection backend");
        let probe = Probe::open();
        let focus_before = probe.focus();

        backend.present(vec![
            frame(Rect::new(300., 200., 3., 2.), 3, 2, 0x8080_ff00),
            frame(Rect::new(0., 0., 2., 1.), 2, 1, 0x4000_ff00),
        ]);
        backend.connection.sync();
        probe.sync();

        let geometries = [(300, 200, 3, 2), (0, 0, 2, 1)];
        assert_eq!(backend.windows.len(), 2, "one window per frame");
        let ids: Vec<xl::Window> = backend.windows.iter().map(|window| window.id()).collect();
        for (id, geometry) in ids.iter().zip(geometries) {
            let attributes = probe.attributes(*id).expect("XGetWindowAttributes");
            assert_eq!(attributes.depth, 32, "ARGB visual");
            assert_ne!(attributes.override_redirect, 0, "override-redirect");
            assert_eq!(attributes.map_state, xl::IsViewable, "mapped");
            assert_eq!(
                (attributes.x, attributes.y, attributes.width, attributes.height),
                geometry
            );
        }
        assert_eq!(focus_before, probe.focus(), "mapping must not take focus");

        let first = ids[0];
        let image = probe.window_image(first).expect("XGetImage");
        assert_eq!(image.size(), (3, 2));
        for y in 0..2 {
            for x in 0..3 {
                assert_eq!(image.pixel(x, y), 0x8080_ff00, "pixel ({x}, {y})");
            }
        }
        drop(image);

        // 空输入 shape：投影窗口不接收点击。
        probe.move_to(301, 201);
        probe.sync();
        probe.click();
        probe.sync();
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        let mut through = false;
        while !through && Instant::now() < deadline {
            through = probe.take_root_press();
            if !through {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        assert!(through, "projection windows must pass clicks through");
        assert_eq!(focus_before, probe.focus(), "clicks must not take focus");

        // Expose 后用缓存的像素缓冲重传。
        probe.expose(first);
        probe.sync();
        backend.pump();
        backend.connection.sync();
        probe.sync();
        let image = probe.window_image(first).expect("XGetImage after Expose");
        for y in 0..2 {
            for x in 0..3 {
                assert_eq!(image.pixel(x, y), 0x8080_ff00, "redrawn pixel ({x}, {y})");
            }
        }
        drop(image);

        backend.hide();
        assert!(backend.windows.is_empty(), "hide tears the windows down");

        println!(
            "projection_argb_windows=ok windows={} depth=32 override_redirect=1 size={}x{}",
            ids.len(),
            geometries[0].2,
            geometries[0].3
        );
        println!("projection_expose_redraw=ok pixel={:08x}", 0x8080_ff00u32);
        println!("projection_click_through=ok passed_to_root={through}");
        println!(
            "projection_focus=ok window={} revert={}",
            focus_before.0, focus_before.1
        );
    }
}
