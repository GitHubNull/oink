//! Linux X11 光标拖尾后端。
//!
//! 与 macOS/Windows 相同的渲染线程接口：上游给出本机单位下的格子矩形与预乘 BGRA 像素，
//! 这里把矩形限制在所有活动显示器的外接矩形内（与 `windows.rs` 相同），按格子边长放大到
//! 设备像素，放进一个 override-redirect、鼠标穿透、不抢焦点的 ARGB 覆盖窗口。
//!
//! 这是阶段 4 W1 的拖尾骨架：窗口在第一个有用帧到来时按实际拖尾范围创建；投影与通知仍走
//! 各自 stub，后续波次复用 [`crate::x11`] 的同一套连接/窗口/上传封装。

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use oink_foundation::display::DisplayInfo;

use crate::diagnostics::Health;
use crate::effect::{CellRect, cells_of};
use crate::x11::{Connection, OverlayWindow};

/// 只有连接、depth-32 ARGB visual 与 XFixes 都可用时才算有 Linux 拖尾后端。
///
/// 这里只打开一条短连接做探测，不创建窗口；XShm 不存在不影响可用性（上传会退回
/// `XPutImage`）。
pub(crate) fn available() -> bool {
    let Some(connection) = Connection::open() else {
        return false;
    };
    drop(connection);
    true
}

/// 本帧的一个覆盖区域。Linux 目前只有一个覆盖窗口，`surface` 总是 0。
pub(crate) struct Layer {
    #[allow(dead_code)]
    pub surface: usize,
    pub rect: CellRect,
    pub pixels: Vec<u32>,
}

pub(crate) struct Backend {
    connection: Rc<Connection>,
    displays: Vec<DisplayInfo>,
    window: Option<OverlayWindow>,
    /// 窗口创建失败后停止每帧重试；显示器变化时再给一次机会。
    window_failed: bool,
    /// k > 1 时的双线性放大结果，复用避免每帧分配。
    scaled: Vec<u32>,
}

impl Backend {
    pub(crate) fn new(_health: Arc<Health>) -> Option<Self> {
        // available() 已经验证过环境；这里在渲染线程上重新打开一条独占连接。
        // 窗口由 present 在知道实际拖尾范围后创建，available() 从不创建窗口。
        let connection = Connection::open()?;
        Some(Self {
            connection,
            displays: Vec::new(),
            window: None,
            window_failed: false,
            scaled: Vec::new(),
        })
    }

    pub(crate) fn set_displays(&mut self, displays: &[DisplayInfo]) {
        self.displays = displays.to_vec();
        self.window_failed = false;
    }

    /// 把拖尾格子矩形限制在所有活动显示器的外接矩形内，语义与 Windows 后端一致：
    /// 一个窗口覆盖整个拖尾，不按屏幕拆分；跨屏空隙也包含在内。
    pub(crate) fn regions(&self, bounds: CellRect, cell: f64) -> Vec<(usize, CellRect)> {
        regions_of(&self.displays, bounds, cell)
    }

    pub(crate) fn present(&mut self, cell: f64, layers: Vec<Layer>) {
        let Some(layer) = layers.into_iter().next() else {
            self.hide();
            return;
        };
        if self.window_failed {
            return;
        }
        if layer.pixels.len() != layer.rect.w as usize * layer.rect.h as usize {
            tracing::warn!(
                pixels = layer.pixels.len(),
                w = layer.rect.w,
                h = layer.rect.h,
                "cursor trail: malformed layer"
            );
            return;
        }
        let Some(rect) = physical_rect(layer.rect, cell) else {
            self.hide();
            return;
        };
        let (width, height) = (rect.w as usize, rect.h as usize);
        let k = cell.round().max(1.0) as usize;
        let pixels: &[u32] = if k <= 1 {
            &layer.pixels
        } else {
            self.scaled.resize(width * height, 0);
            crate::effect::upscale_bilinear(
                &layer.pixels,
                layer.rect.w as usize,
                layer.rect.h as usize,
                k,
                &mut self.scaled,
                width,
            );
            &self.scaled
        };
        if self.window.is_none() {
            match OverlayWindow::new(self.connection.clone(), rect) {
                Some(window) => self.window = Some(window),
                None => {
                    self.window_failed = true;
                    tracing::warn!(
                        x = rect.x,
                        y = rect.y,
                        width,
                        height,
                        "cursor trail: cannot create the X11 overlay"
                    );
                    return;
                }
            }
        }
        let Some(window) = self.window.as_mut() else {
            return;
        };
        if !window.configure(rect) {
            tracing::debug!(
                x = rect.x,
                y = rect.y,
                width,
                height,
                "cursor trail: overlay geometry rejected"
            );
            return;
        }
        if !window.upload(pixels) {
            return;
        }
        window.map_raised();
    }

    pub(crate) fn hide(&mut self) {
        if let Some(window) = self.window.as_mut() {
            window.unmap();
        }
    }

    /// Linux 没有可等待的合成器接口：按 60 Hz 限速，与 Windows 的兜底一致。
    pub(crate) fn wait_frame(&mut self, frame_start: Instant) {
        let elapsed = frame_start.elapsed();
        if elapsed < crate::DEFAULT_FRAME {
            std::thread::sleep(crate::DEFAULT_FRAME - elapsed);
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        if let Some(window) = self.window.take() {
            window.destroy();
        }
    }
}

/// 所有活动显示器的格子外接矩形；没有显示器时 `None`。
fn desktop(displays: &[DisplayInfo], cell: f64) -> Option<CellRect> {
    let mut it = displays.iter().map(|d| cells_of(d.bounds, cell));
    let first = it.next()?;
    Some(it.fold(first, union))
}

fn union(a: CellRect, b: CellRect) -> CellRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    CellRect {
        x,
        y,
        w: (a.x + i64::from(a.w)).max(b.x + i64::from(b.w)).saturating_sub(x) as u32,
        h: (a.y + i64::from(a.h)).max(b.y + i64::from(b.h)).saturating_sub(y) as u32,
    }
}

fn regions_of(displays: &[DisplayInfo], bounds: CellRect, cell: f64) -> Vec<(usize, CellRect)> {
    let Some(desktop) = desktop(displays, cell) else {
        return Vec::new();
    };
    let rect = bounds.intersect(&desktop);
    if rect.is_empty() { Vec::new() } else { vec![(0, rect)] }
}

/// 把格子矩形换算成根窗口设备像素（X11 本机坐标就是设备像素，cell 通常是 1）。
fn physical_rect(rect: CellRect, cell: f64) -> Option<CellRect> {
    if !cell.is_finite() || cell <= 0.0 {
        return None;
    }
    let k = cell.round().max(1.0);
    if k > 64.0 {
        return None;
    }
    let k = k as i64;
    let x = rect.x.checked_mul(k)?;
    let y = rect.y.checked_mul(k)?;
    let w = i64::from(rect.w).checked_mul(k)?;
    let h = i64::from(rect.h).checked_mul(k)?;
    let w = u32::try_from(w).ok()?;
    let h = u32::try_from(h).ok()?;
    if w == 0 || h == 0 || w > u32::from(u16::MAX) || h > u32::from(u16::MAX) {
        return None;
    }
    Some(CellRect { x, y, w, h })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oink_foundation::Rect;

    fn display(bounds: Rect) -> DisplayInfo {
        DisplayInfo {
            key: "test".into(),
            name: "test".into(),
            bounds,
            pixel_width: bounds.w as u32,
            pixel_height: bounds.h as u32,
            reference_extent: None,
            rotation_deg: 0,
            is_primary: true,
            is_builtin: false,
            is_virtual: false,
        }
    }

    fn rect(x: i64, y: i64, w: u32, h: u32) -> CellRect {
        CellRect { x, y, w, h }
    }

    #[test]
    fn regions_follow_the_display_union_and_clip_to_it() {
        let displays = [
            display(Rect::new(0., 0., 1000., 800.)),
            display(Rect::new(1600., 0., 800., 600.)),
        ];
        assert_eq!(desktop(&displays, 1.0), Some(rect(0, 0, 2400, 800)));
        // 跨屏空隙连同两侧显示器一起保留（与 Windows 的单窗口语义一致）。
        assert_eq!(
            regions_of(&displays, rect(900, 100, 800, 400), 1.0),
            vec![(0, rect(900, 100, 800, 400))]
        );
        // 超出桌面的部分被裁掉。
        assert_eq!(
            regions_of(&displays, rect(-50, 750, 100, 100), 1.0),
            vec![(0, rect(0, 750, 50, 50))]
        );
        assert!(regions_of(&displays, rect(2600, 0, 10, 10), 1.0).is_empty());
    }

    #[test]
    fn display_union_handles_negative_origins_and_fractional_cells() {
        let displays = [
            display(Rect::new(-800., -100., 800., 600.)),
            display(Rect::new(0., 0., 1000., 800.)),
        ];
        assert_eq!(desktop(&displays, 1.0), Some(rect(-800, -100, 1800, 900)));
        assert_eq!(
            regions_of(&displays, rect(-900, -200, 200, 200), 1.0),
            vec![(0, rect(-800, -100, 100, 100))]
        );

        let hidpi = [display(Rect::new(0., 0., 1000., 800.))];
        assert_eq!(desktop(&hidpi, 2.0), Some(rect(0, 0, 500, 400)));
        assert_eq!(
            regions_of(&hidpi, rect(499, 399, 10, 10), 2.0),
            vec![(0, rect(499, 399, 1, 1))]
        );
    }

    #[test]
    fn empty_displays_and_invalid_cells_produce_no_regions() {
        assert_eq!(desktop(&[], 1.0), None);
        assert!(regions_of(&[], rect(0, 0, 1, 1), 1.0).is_empty());
        let displays = [display(Rect::new(0., 0., 100., 100.))];
        assert!(regions_of(&displays, rect(0, 0, 0, 0), 1.0).is_empty());
        assert_eq!(physical_rect(rect(1, 2, 3, 4), f64::NAN), None);
        assert_eq!(physical_rect(rect(1, 2, 3, 4), -1.0), None);
        assert_eq!(physical_rect(rect(1, 2, 3, 4), 2.0), Some(rect(2, 4, 6, 8)));
    }
}
