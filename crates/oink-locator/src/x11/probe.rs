//! 私有 Xephyr 中的第二条 Xlib 连接：X 级读回、根窗口事件观察与 XTest 注入。
//!
//! 只在测试编译；任何调用都必须运行在 `linux-native-test.py` 起的隔离服务器里，
//! 绝不连接用户真实会话。

use x11_dl::xlib as xl;
use x11_dl::xtest::Xf86vmode as Xtest;

pub(crate) struct Probe {
    xlib: xl::Xlib,
    xtest: Xtest,
    dpy: *mut xl::Display,
    root: xl::Window,
}

impl Probe {
    pub(crate) fn open() -> Self {
        let xlib = xl::Xlib::open().expect("libX11");
        // SAFETY: null 表示沿用 DISPLAY 环境变量；返回的连接由此结构独占。
        let dpy = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
        assert!(!dpy.is_null(), "XOpenDisplay");
        // SAFETY: dpy 是刚打开的有效连接。
        let root = unsafe { (xlib.XDefaultRootWindow)(dpy) };
        let xtest = Xtest::open().expect("libXtst");
        let (mut event, mut error, mut major, mut minor) = (0, 0, 0, 0);
        // SAFETY: 四个输出参数都是可写栈变量；只在私有服务器里调用。
        let present = unsafe { (xtest.XTestQueryExtension)(dpy, &mut event, &mut error, &mut major, &mut minor) };
        assert!(present != 0, "XTEST extension");
        // SAFETY: root 属于同一条连接；只订阅按钮事件用于穿透取证。
        unsafe { (xlib.XSelectInput)(dpy, root, xl::ButtonPressMask | xl::ButtonReleaseMask) };
        // SAFETY: 标准连接刷新。
        unsafe { (xlib.XSync)(dpy, 0) };
        Self { xlib, xtest, dpy, root }
    }

    pub(crate) fn sync(&self) {
        // SAFETY: dpy 有效；discard=false。
        unsafe { (self.xlib.XSync)(self.dpy, 0) };
    }

    pub(crate) fn move_to(&self, x: i32, y: i32) {
        // SAFETY: screen=-1 表示当前屏幕；坐标为私有 Xephyr 根坐标。
        unsafe { (self.xtest.XTestFakeMotionEvent)(self.dpy, -1, x, y, 0) };
    }

    pub(crate) fn click(&self) {
        // SAFETY: 按钮 1 的按下/释放注入到私有 Xephyr。
        unsafe {
            (self.xtest.XTestFakeButtonEvent)(self.dpy, 1, 1, 0);
            (self.xtest.XTestFakeButtonEvent)(self.dpy, 1, 0, 0);
        }
    }

    pub(crate) fn focus(&self) -> (xl::Window, i32) {
        let mut window: xl::Window = 0;
        let mut revert = 0;
        // SAFETY: 两个输出参数都是可写栈变量。
        unsafe { (self.xlib.XGetInputFocus)(self.dpy, &mut window, &mut revert) };
        (window, revert)
    }

    /// 排空队列；返回是否收到落在根窗口上的 ButtonPress。
    pub(crate) fn take_root_press(&self) -> bool {
        let mut pressed = false;
        // SAFETY: 仅当 XPending 报告事件时才取，XNextEvent 写入完整事件。
        while unsafe { (self.xlib.XPending)(self.dpy) } > 0 {
            // SAFETY: XEvent 是全零合法的 C POD；XNextEvent 写入完整事件。
            let mut event: xl::XEvent = unsafe { std::mem::zeroed() };
            // SAFETY: dpy 有效，event 是有效的输出存储。
            unsafe { (self.xlib.XNextEvent)(self.dpy, &mut event) };
            // SAFETY: 先读 type_ 再解释联合体成员。
            let kind = unsafe { event.type_ };
            if kind == xl::ButtonPress {
                // SAFETY: kind 是 ButtonPress，button 是活动成员。
                pressed |= unsafe { event.button.window } == self.root;
            }
        }
        pressed
    }

    pub(crate) fn attributes(&self, window: xl::Window) -> Option<xl::XWindowAttributes> {
        // SAFETY: XWindowAttributes 是全零合法的 C POD。
        let mut attributes: xl::XWindowAttributes = unsafe { std::mem::zeroed() };
        // SAFETY: window 属于测试服务器；输出写到栈上结构。
        let ok = unsafe { (self.xlib.XGetWindowAttributes)(self.dpy, window, &mut attributes) };
        (ok != 0).then_some(attributes)
    }

    /// 按窗口全尺寸清空并要求服务器发 Expose，用于验证重绘路径。
    pub(crate) fn expose(&self, window: xl::Window) {
        // SAFETY: window 属于测试服务器；0 尺寸表示整个窗口。
        unsafe { (self.xlib.XClearArea)(self.dpy, window, 0, 0, 0, 0, 1) };
    }

    pub(crate) fn window_image(&self, window: xl::Window) -> Option<WindowImage<'_>> {
        let attributes = self.attributes(window)?;
        let (width, height) = (attributes.width, attributes.height);
        if width <= 0 || height <= 0 {
            return None;
        }
        // SAFETY: 窗口已映射且尺寸非零；图像由 WindowImage 的 Drop 释放。
        let image = unsafe {
            (self.xlib.XGetImage)(
                self.dpy,
                window,
                0,
                0,
                width as u32,
                height as u32,
                (self.xlib.XAllPlanes)(),
                xl::ZPixmap,
            )
        };
        (!image.is_null()).then(|| WindowImage {
            xlib: &self.xlib,
            image,
            width,
            height,
        })
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        // SAFETY: dpy 由 Probe 独占。
        unsafe { (self.xlib.XCloseDisplay)(self.dpy) };
    }
}

/// 窗口像素读回；使用后靠 Drop 释放，不向测试暴露 XImage 指针。
pub(crate) struct WindowImage<'a> {
    xlib: &'a xl::Xlib,
    image: *mut xl::XImage,
    width: i32,
    height: i32,
}

impl WindowImage<'_> {
    pub(crate) fn size(&self) -> (i32, i32) {
        (self.width, self.height)
    }

    pub(crate) fn pixel(&self, x: i32, y: i32) -> u32 {
        assert!(
            (0..self.width).contains(&x) && (0..self.height).contains(&y),
            "pixel ({x}, {y}) outside {}x{}",
            self.width,
            self.height
        );
        // SAFETY: 坐标已检查在图像范围内。
        unsafe { (self.xlib.XGetPixel)(self.image, x, y) as u32 }
    }
}

impl Drop for WindowImage<'_> {
    fn drop(&mut self) {
        // SAFETY: image 由 window_image 创建，只在此释放一次。
        unsafe { (self.xlib.XDestroyImage)(self.image) };
        self.image = std::ptr::null_mut();
    }
}
