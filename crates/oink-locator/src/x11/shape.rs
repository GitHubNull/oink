//! XFixes 输入形状。
//!
//! 覆盖层默认完全鼠标穿透：把 `ShapeInput` 设为空区域，点击与滚动落到底下的窗口；
//! 通知窗口按可见卡片逐行设置输入区域，装饰与透明像素仍穿透。

use std::ptr;

use x11_dl::xlib;

use super::Connection;

/// `X11/extensions/shapeconst.h`：`ShapeInput` 是窗口接收输入的区域。
const SHAPE_INPUT: i32 = 2;

/// 让 `window` 不接收任何指针输入（鼠标穿透）。
pub(crate) fn empty_input_shape(conn: &Connection, window: xlib::Window) {
    input_shape(conn, window, &[]);
}

/// 用窗口坐标里的矩形并集替换输入区域；空列表即完全穿透。
pub(crate) fn input_shape(conn: &Connection, window: xlib::Window, rects: &[xlib::XRectangle]) {
    // SAFETY: XFixesCreateRegion 只读取这些矩形，不保留指针。
    let region = unsafe {
        (conn.xfixes.XFixesCreateRegion)(
            conn.dpy,
            if rects.is_empty() {
                ptr::null_mut()
            } else {
                rects.as_ptr().cast_mut()
            },
            rects.len() as i32,
        )
    };
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
