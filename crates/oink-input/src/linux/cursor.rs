//! Remote 模式的光标遮罩：全覆盖 ARGB 覆盖窗 + 空输入形状 + XFixes 隐藏/显示。
//!
//! 窗口在每次隐藏时按当前根窗口尺寸重建，避免分辨率变化后留下未覆盖区域；
//! 显示时立即销毁，不长期占用合成器资源。

use std::sync::Arc;

use oink_foundation::Point;
use x11_dl::xfixes::Xlib as Xfixes;
use x11_dl::xlib::{
    AllocNone, CWBackPixel, CWBorderPixel, CWColormap, CWOverrideRedirect, Colormap, Display, InputOutput, TrueColor,
    Window, XSetWindowAttributes, XVisualInfo, Xlib,
};

use crate::InputError;

/// X Shape 的输入形状种类（XShape.h 的 ShapeInput）。
const SHAPE_INPUT: i32 = 2;

pub(super) struct CursorMask {
    xlib: Arc<Xlib>,
    fixes: Xfixes,
    dpy: *mut Display,
    window: Option<Window>,
    colormap: Option<Colormap>,
    hidden: bool,
}

impl CursorMask {
    pub(super) fn new(xlib: Arc<Xlib>, dpy: *mut Display) -> Result<Self, InputError> {
        let fixes = Xfixes::open().map_err(|e| InputError::Platform(format!("加载 libXfixes 失败：{e}")))?;
        let (mut event_base, mut error_base) = (0, 0);
        // SAFETY: dpy 有效且由截获线程独占；两个 out 指针有效。
        let present = unsafe { (fixes.XFixesQueryExtension)(dpy, &mut event_base, &mut error_base) };
        if present == 0 {
            return Err(InputError::Platform("X 服务器缺少 XFixes 扩展，无法隐藏光标".into()));
        }
        Ok(Self {
            xlib,
            fixes,
            dpy,
            window: None,
            colormap: None,
            hidden: false,
        })
    }

    pub(super) fn hidden(&self) -> bool {
        self.hidden
    }

    fn ensure_window(&mut self) -> Result<(), InputError> {
        if self.window.is_some() {
            return Ok(());
        }
        // SAFETY: dpy 有效且由本结构独占；screen 索引来自同一连接。
        let screen = unsafe { (self.xlib.XDefaultScreen)(self.dpy) };
        // SAFETY: dpy/screen 有效；vinfo 是有效 out 指针。
        let mut vinfo: XVisualInfo = unsafe { std::mem::zeroed() };
        // SAFETY: 同上；返回 0 表示没有 32 位 TrueColor visual。
        let matched = unsafe { (self.xlib.XMatchVisualInfo)(self.dpy, screen, 32, TrueColor, &mut vinfo) };
        if matched == 0 {
            return Err(InputError::Platform("X 服务器没有 32 位 ARGB visual".into()));
        }
        // SAFETY: dpy 有效；root 来自同一连接。
        let root = unsafe { (self.xlib.XDefaultRootWindow)(self.dpy) };
        let (mut root_return, mut x_return, mut y_return) = (0, 0, 0);
        let (mut width, mut height, mut border, mut depth) = (0, 0, 0, 0);
        // SAFETY: 所有 out 指针都是有效栈变量。
        let geometry = unsafe {
            (self.xlib.XGetGeometry)(
                self.dpy,
                root,
                &mut root_return,
                &mut x_return,
                &mut y_return,
                &mut width,
                &mut height,
                &mut border,
                &mut depth,
            )
        };
        if geometry == 0 || width == 0 || height == 0 {
            return Err(InputError::Platform("XGetGeometry 读取根窗口失败".into()));
        }
        // SAFETY: visual 来自 XMatchVisualInfo，root 来自同一连接。
        let colormap = unsafe { (self.xlib.XCreateColormap)(self.dpy, root, vinfo.visual, AllocNone) };
        if colormap == 0 {
            return Err(InputError::Platform("XCreateColormap 失败".into()));
        }
        // SAFETY: 零初始化的属性结构随后逐字段填写，只使用 mask 声明的字段。
        let mut attributes: XSetWindowAttributes = unsafe { std::mem::zeroed() };
        attributes.override_redirect = 1;
        attributes.background_pixel = 0;
        attributes.border_pixel = 0;
        attributes.colormap = colormap;
        attributes.event_mask = 0;
        let mask = CWOverrideRedirect | CWBackPixel | CWBorderPixel | CWColormap;
        // SAFETY: dpy/root/visual 有效；attributes 在调用期间存活。
        let window = unsafe {
            (self.xlib.XCreateWindow)(
                self.dpy,
                root,
                0,
                0,
                width,
                height,
                0,
                32,
                InputOutput as u32,
                vinfo.visual,
                mask,
                &mut attributes,
            )
        };
        if window == 0 {
            // SAFETY: colormap 由本函数创建且尚未保存到 self。
            unsafe { (self.xlib.XFreeColormap)(self.dpy, colormap) };
            return Err(InputError::Platform("XCreateWindow 创建光标遮罩失败".into()));
        }
        self.window = Some(window);
        self.colormap = Some(colormap);
        Ok(())
    }

    /// 映射遮罩、清空输入形状（点击穿透）并隐藏光标。随后应尽快停靠指针。
    pub(super) fn hide(&mut self) -> Result<(), InputError> {
        if self.hidden {
            return Ok(());
        }
        self.ensure_window()?;
        let window = self.window.expect("遮罩窗口已创建");
        // SAFETY: dpy 有效；rectangles=NULL/0 表示空区域，即不接收任何输入。
        let region = unsafe { (self.fixes.XFixesCreateRegion)(self.dpy, std::ptr::null_mut(), 0) };
        if region == 0 {
            return Err(InputError::Platform("XFixesCreateRegion 失败".into()));
        }
        // SAFETY: window 由本结构创建；region 有效并在设置后立即释放。
        unsafe {
            (self.fixes.XFixesSetWindowShapeRegion)(self.dpy, window, SHAPE_INPUT, 0, 0, region);
            (self.fixes.XFixesDestroyRegion)(self.dpy, region);
            (self.xlib.XMapRaised)(self.dpy, window);
            (self.fixes.XFixesHideCursor)(self.dpy, window);
            (self.xlib.XSync)(self.dpy, 0);
        }
        self.hidden = true;
        Ok(())
    }

    /// 隐藏期间把指针停靠到给定位置；不在隐藏状态时不做任何事。
    pub(super) fn park(&self, at: Point) {
        if !self.hidden {
            return;
        }
        self.warp(at);
    }

    pub(super) fn warp(&self, at: Point) {
        let (x, y) = (round(at.x), round(at.y));
        // SAFETY: dpy 有效；src_w=0 表示不限定源窗口；root 来自同一连接。
        let root = unsafe { (self.xlib.XDefaultRootWindow)(self.dpy) };
        // SAFETY: 同上，坐标是整数物理像素。
        unsafe { (self.xlib.XWarpPointer)(self.dpy, 0, root, 0, 0, 0, 0, x, y) };
        // SAFETY: 立即冲刷，让停靠先于后续显示/核验。
        unsafe { (self.xlib.XFlush)(self.dpy) };
    }

    /// 显示光标并销毁遮罩。幂等；Drop 也会走这里。
    pub(super) fn show(&mut self) {
        if !self.hidden {
            return;
        }
        if let Some(window) = self.window.take() {
            // SAFETY: window 由本结构创建且只销毁一次。
            unsafe {
                (self.fixes.XFixesShowCursor)(self.dpy, window);
                (self.xlib.XUnmapWindow)(self.dpy, window);
                (self.xlib.XDestroyWindow)(self.dpy, window);
            }
        }
        if let Some(colormap) = self.colormap.take() {
            // SAFETY: colormap 由本结构创建且只释放一次。
            unsafe { (self.xlib.XFreeColormap)(self.dpy, colormap) };
        }
        // SAFETY: dpy 有效。
        unsafe { (self.xlib.XFlush)(self.dpy) };
        self.hidden = false;
    }

    /// 同步到服务器后读取真实指针位置；失败返回 None（不能当作核验通过）。
    pub(super) fn position(&self) -> Option<Point> {
        // SAFETY: dpy 由截获线程独占。
        let root = unsafe { (self.xlib.XDefaultRootWindow)(self.dpy) };
        let (mut root_return, mut child_return) = (0, 0);
        let (mut root_x, mut root_y, mut win_x, mut win_y) = (0, 0, 0, 0);
        let mut mask = 0;
        // SAFETY: 所有 out 指针都是有效栈变量；root 来自同一连接。
        let ok = unsafe {
            (self.xlib.XQueryPointer)(
                self.dpy,
                root,
                &mut root_return,
                &mut child_return,
                &mut root_x,
                &mut root_y,
                &mut win_x,
                &mut win_y,
                &mut mask,
            )
        };
        (ok != 0).then_some(Point::new(f64::from(root_x), f64::from(root_y)))
    }
}

impl Drop for CursorMask {
    fn drop(&mut self) {
        self.show();
    }
}

fn round(value: f64) -> i32 {
    value.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}
