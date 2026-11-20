//! XFixes 输入形状。
//!
//! 覆盖层默认完全鼠标穿透：把 `ShapeInput` 设为空区域，点击与滚动落到底下的窗口；
//! 需要点击的通知在后续波次里按可见卡片重新设置输入区域。

use std::ptr;

use x11_dl::xlib;

use super::Connection;

/// `X11/extensions/shapeconst.h`：`ShapeInput` 是窗口接收输入的区域。
const SHAPE_INPUT: i32 = 2;

/// 让 `window` 不接收任何指针输入（鼠标穿透）。
pub(crate) fn empty_input_shape(conn: &Connection, window: xlib::Window) {
    // SAFETY: dpy 已打开且带 XFixes；矩形指针为 null、数量为 0 就是空区域。
    let region = unsafe { (conn.xfixes.XFixesCreateRegion)(conn.dpy, ptr::null_mut(), 0) };
    if region == 0 {
        tracing::debug!("cursor trail: XFixesCreateRegion failed; the overlay may capture input");
        return;
    }
    // SAFETY: region 是存活的服务端区域，window 属于同一条连接；SetWindowShapeRegion
    // 会复制区域，因此随后即可销毁。
    unsafe {
        (conn.xfixes.XFixesSetWindowShapeRegion)(conn.dpy, window, SHAPE_INPUT, 0, 0, region);
        (conn.xfixes.XFixesDestroyRegion)(conn.dpy, region);
    }
}
