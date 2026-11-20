//! Linux 桌面通知：每个 toast 一个 ARGB 窗口，输入区域只覆盖可见卡片像素。
//!
//! 通知 worker 独占连接；窗口不抢焦点、不激活、不捕获输入。Enter/Motion/Button
//! 事件复用共享的 [`HitState`] 纯函数产生 hover/press 反馈与点击；输入区域按卡片
//! 每行不透明像素设置，透明留白与阴影外区域点击穿透。动效开关读 Cinnamon 的
//! `enable-animations`，schema 缺失时按“减少动效”处理。

use std::rc::Rc;

use gio::prelude::SettingsExt;
use x11_dl::xlib;

use oink_foundation::Point;

use super::{
    Click, Feedback, Frame, Surface,
    interaction::{HitState, Input},
};
use crate::effect::CellRect;
use crate::x11::{Connection, OverlayWindow};

const ANIMATIONS_SCHEMA: &str = "org.cinnamon.desktop.interface";
const ANIMATIONS_KEY: &str = "enable-animations";

pub(super) struct Backend {
    connection: Rc<Connection>,
    context: glib::MainContext,
    settings: Option<gio::Settings>,
    windows: Vec<Window>,
    clicks: Vec<Click>,
    feedback: Vec<Feedback>,
}

struct Window {
    overlay: OverlayWindow,
    hit: HitState,
    shape: Vec<xlib::XRectangle>,
}

impl Window {
    fn new(connection: Rc<Connection>) -> Option<Self> {
        let rect = CellRect { x: 0, y: 0, w: 1, h: 1 };
        let overlay = OverlayWindow::new(connection, rect)?;
        overlay.select_events(
            xlib::EnterWindowMask
                | xlib::LeaveWindowMask
                | xlib::PointerMotionMask
                | xlib::ButtonPressMask
                | xlib::ButtonReleaseMask
                | xlib::ExposureMask,
        );
        Some(Self {
            overlay,
            hit: HitState::default(),
            shape: Vec::new(),
        })
    }

    fn present(&mut self, frame: Frame) {
        let Some(rect) = window_rect(&frame) else {
            tracing::warn!("notification: malformed frame");
            return;
        };
        if !self.overlay.configure(rect) {
            return;
        }
        let shape = if frame.interactive {
            card_shape(&frame)
        } else {
            Vec::new()
        };
        if shape != self.shape {
            self.overlay.set_input_shape(&shape);
            self.shape = shape;
        }
        self.hit.update_frame(&frame);
        // 服务器不保证保留映射前的窗口内容（Xephyr 实测会丢弃），先映射再上传；
        // 同一批请求内完成，不会闪出未初始化的一帧。
        self.overlay.map_raised();
        if !self.overlay.upload(&frame.pixels) {
            self.overlay.unmap();
        }
    }
}

impl Backend {
    pub(super) fn new() -> Option<Self> {
        let connection = Connection::open()?;
        let context = glib::MainContext::new();
        let settings = context
            .with_thread_default(|| {
                gio::SettingsSchemaSource::default()
                    .and_then(|source| source.lookup(ANIMATIONS_SCHEMA, true))
                    .map(|_| gio::Settings::new(ANIMATIONS_SCHEMA))
            })
            .ok()
            .flatten();
        if settings.is_none() {
            tracing::debug!("notification: Cinnamon animation setting unavailable; reducing motion");
        }
        Some(Self {
            connection,
            context,
            settings,
            windows: Vec::new(),
            clicks: Vec::new(),
            feedback: Vec::new(),
        })
    }

    fn drain(&mut self) {
        let mut redraws = Vec::new();
        let mut incoming = Vec::new();
        self.connection.drain_events(|event| {
            // SAFETY: XNextEvent 写入完整事件；先读 type_ 再按匹配成员解释联合体。
            let kind = unsafe { event.type_ };
            match kind {
                xlib::Expose => {
                    // SAFETY: kind 是 Expose，expose 是活动成员。
                    let e = unsafe { event.expose };
                    if e.count == 0 {
                        redraws.push(e.window);
                    }
                }
                xlib::EnterNotify | xlib::MotionNotify => {
                    let (window, x, y) = if kind == xlib::EnterNotify {
                        // SAFETY: kind 是 EnterNotify，crossing 是活动成员。
                        let e = unsafe { event.crossing };
                        (e.window, e.x, e.y)
                    } else {
                        // SAFETY: kind 是 MotionNotify，motion 是活动成员。
                        let e = unsafe { event.motion };
                        (e.window, e.x, e.y)
                    };
                    incoming.push((window, Input::Move(Point::new(f64::from(x), f64::from(y)))));
                }
                xlib::LeaveNotify => {
                    // SAFETY: kind 是 LeaveNotify，crossing 是活动成员。
                    let e = unsafe { event.crossing };
                    incoming.push((e.window, Input::Leave));
                }
                xlib::ButtonPress | xlib::ButtonRelease => {
                    // SAFETY: kind 是 ButtonPress/ButtonRelease，button 是活动成员。
                    let e = unsafe { event.button };
                    let point = Point::new(f64::from(e.x), f64::from(e.y));
                    let input = if kind == xlib::ButtonPress {
                        Input::Down(point)
                    } else {
                        Input::Up(point)
                    };
                    incoming.push((e.window, input));
                }
                _ => {}
            }
        });
        for (window_id, input) in incoming {
            let Some(window) = self.windows.iter_mut().find(|w| w.overlay.id() == window_id) else {
                continue;
            };
            let before = window.hit.feedback();
            let (feedback, click) = window.hit.input(input);
            if feedback != before {
                self.feedback.push(feedback);
            }
            if let Some(click) = click {
                self.clicks.push(click);
            }
        }
        for window_id in redraws {
            if let Some(window) = self.windows.iter_mut().find(|w| w.overlay.id() == window_id) {
                window.overlay.redraw();
            }
        }
    }
}

impl Surface for Backend {
    fn present(&mut self, frames: Vec<Frame>) {
        self.windows.truncate(frames.len());
        while self.windows.len() < frames.len() {
            let Some(window) = Window::new(self.connection.clone()) else {
                tracing::warn!("notification: cannot create the X11 overlay");
                return;
            };
            self.windows.push(window);
        }
        for (window, frame) in self.windows.iter_mut().zip(frames) {
            window.present(frame);
        }
        self.drain();
    }

    fn clicks(&mut self) -> Vec<Click> {
        self.drain();
        std::mem::take(&mut self.clicks)
    }

    fn feedback(&mut self) -> Vec<Feedback> {
        self.drain();
        std::mem::take(&mut self.feedback)
    }

    fn reduced_motion(&self) -> bool {
        // GSettings 的变化经创建时线程默认上下文送达；worker 没有自己的主循环，
        // 在每次取用前把挂起信号排空即可保持缓存新鲜。
        while self.context.pending() {
            self.context.iteration(false);
        }
        self.settings
            .as_ref()
            .is_none_or(|settings| !settings.boolean(ANIMATIONS_KEY))
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

/// 按行把不透明像素拆成连续段并纵向合并：卡片圆角、透明留白与动画位移都自然
/// 穿透，内部空洞也不会被整行包围盒吞掉，与 [`Frame::contains`] 的 alpha 命中原点
/// 保持同一语义。
fn card_shape(frame: &Frame) -> Vec<xlib::XRectangle> {
    let (w, h) = (frame.width, frame.height);
    if w == 0 || h == 0 || frame.pixels.len() != w * h {
        return Vec::new();
    }
    if w > i16::MAX as usize || h > i16::MAX as usize {
        return Vec::new();
    }
    let mut rects: Vec<xlib::XRectangle> = Vec::new();
    for y in 0..h {
        let row = &frame.pixels[y * w..(y + 1) * w];
        let mut x = 0;
        while x < w {
            if row[x] >> 24 == 0 {
                x += 1;
                continue;
            }
            let left = x;
            while x < w && row[x] >> 24 != 0 {
                x += 1;
            }
            let width = x - left;
            if let Some(last) = rects.last_mut() {
                let same_run = last.x as usize == left
                    && last.width as usize == width
                    && last.y as usize + last.height as usize == y;
                if same_run {
                    last.height += 1;
                    continue;
                }
            }
            rects.push(xlib::XRectangle {
                x: left as i16,
                y: y as i16,
                width: width as u16,
                height: 1,
            });
        }
    }
    rects
}

#[cfg(test)]
mod tests;
