//! Linux X11 公共封装：连接、ARGB 覆盖窗口与像素上传。
//!
//! 这里只放拖尾、投影与通知后端共用的最小 Xlib 包装：
//!
//! - [`Connection`]：一条到 `$DISPLAY` 的连接；校验 XFixes 与默认屏幕上的 32 位
//!   TrueColor（ARGB）visual，创建私有 colormap，并记录 XShm 是否可用；
//! - [`OverlayWindow`]：override-redirect、无背景、depth 32 的覆盖窗口；输入形状为空
//!   （鼠标穿透）、不请求焦点，映射后置顶；像素用 XShm（可用时）或 XPutImage 上传。
//!
//! 线程模型：两个类型都持有原始 `Display*`，因此是 `!Send`/`!Sync`。所有 Xlib 调用必须
//! 在创建连接的线程上执行；拖尾渲染线程自己创建并独占它们。探测可用性的调用方只能打开
//! 自己的短连接（[`Connection::open`]），不得跨线程共享这里的对象。

mod shape;
mod shm;

#[cfg(test)]
pub(crate) mod probe;

use std::ptr;
use std::rc::Rc;
use std::sync::Once;

use x11_dl::xfixes::Xlib as Xfixes;
use x11_dl::xlib::{self, Display, TrueColor, Visual, Xlib};
use x11_dl::xshm::Xext;

use crate::effect::CellRect;

/// X 窗口坐标在协议里是 16 位有符号，尺寸是 16 位无符号。
const MAX_COORD: i64 = i16::MAX as i64;
const MIN_COORD: i64 = i16::MIN as i64;
const MAX_DIMENSION: u32 = u16::MAX as u32;

static INIT: Once = Once::new();

/// `XInitThreads` 必须先于进程里的任何 X 请求；[`Once`] 保证本 crate 只调用一次。
fn init_threads(xlib: &Xlib) {
    INIT.call_once(|| {
        // SAFETY: 无参数、进程内只初始化一次；此时本 crate 尚未发出任何 X 请求。
        unsafe { (xlib.XInitThreads)() };
    });
}

/// 一条已校验的 X11 连接。只在打开它的线程上使用。
pub(crate) struct Connection {
    xlib: Xlib,
    xfixes: Xfixes,
    /// `None` 表示服务器或库不支持 MIT-SHM；上传退回 XPutImage。
    xshm: Option<Xext>,
    dpy: *mut Display,
    root: xlib::Window,
    visual: *mut Visual,
    depth: i32,
    colormap: xlib::Colormap,
}

impl Connection {
    /// 打开并校验一条连接；任何一项不可用时返回 `None` 并记录原因。
    pub(crate) fn open() -> Option<Rc<Self>> {
        let xlib = match Xlib::open() {
            Ok(xlib) => xlib,
            Err(e) => {
                tracing::debug!(error = %e, "cursor trail: libX11 unavailable");
                return None;
            }
        };
        init_threads(&xlib);
        // SAFETY: null 表示沿用 DISPLAY 环境变量；返回的连接由此函数独占，失败时立即关闭。
        let dpy = unsafe { (xlib.XOpenDisplay)(ptr::null()) };
        if dpy.is_null() {
            tracing::debug!(display = ?std::env::var("DISPLAY").ok(), "cursor trail: cannot open the X11 display");
            return None;
        }
        // SAFETY: dpy 是刚打开的有效连接；默认屏幕与根窗口总是存在。
        let (screen, root) = unsafe { ((xlib.XDefaultScreen)(dpy), (xlib.XDefaultRootWindow)(dpy)) };
        let xfixes = match Xfixes::open() {
            Ok(xfixes) => xfixes,
            Err(e) => {
                tracing::debug!(error = %e, "cursor trail: libXfixes unavailable");
                close_display(&xlib, dpy);
                return None;
            }
        };
        let mut event_base = 0;
        let mut error_base = 0;
        // SAFETY: dpy 已打开；两个输出指针都是有效的栈存储。
        if unsafe { (xfixes.XFixesQueryExtension)(dpy, &mut event_base, &mut error_base) } == 0 {
            tracing::debug!("cursor trail: the X server has no XFixes extension");
            close_display(&xlib, dpy);
            return None;
        }
        let xshm = match Xext::open() {
            Ok(xext) => {
                // SAFETY: dpy 已打开。
                if unsafe { (xext.XShmQueryExtension)(dpy) } != 0 {
                    tracing::debug!("cursor trail: XShm available");
                    Some(xext)
                } else {
                    tracing::debug!("cursor trail: no XShm; pixel uploads use XPutImage");
                    None
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "cursor trail: libXext unavailable; pixel uploads use XPutImage");
                None
            }
        };
        let Some((visual, depth)) = find_argb_visual(&xlib, dpy, screen) else {
            close_display(&xlib, dpy);
            return None;
        };
        // SAFETY: dpy 已打开，visual 属于它的默认屏幕。
        let colormap = unsafe { (xlib.XCreateColormap)(dpy, root, visual, xlib::AllocNone) };
        if colormap == 0 {
            tracing::warn!("cursor trail: XCreateColormap for the ARGB visual failed");
            close_display(&xlib, dpy);
            return None;
        }
        Some(Rc::new(Self {
            xlib,
            xfixes,
            xshm,
            dpy,
            root,
            visual,
            depth,
            colormap,
        }))
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if self.dpy.is_null() {
            return;
        }
        // SAFETY: 此对象独占 display 与 colormap；OverlayWindow 持有 Rc<Connection>，
        // 所以最后一个引用到这里时所有窗口都已销毁。
        unsafe {
            if self.colormap != 0 {
                (self.xlib.XFreeColormap)(self.dpy, self.colormap);
            }
            (self.xlib.XCloseDisplay)(self.dpy);
        }
        self.dpy = ptr::null_mut();
    }
}

/// 默认屏幕上的 32 位 TrueColor visual（ARGB 覆盖层的前提）。
fn find_argb_visual(xlib: &Xlib, dpy: *mut Display, screen: i32) -> Option<(*mut Visual, i32)> {
    // SAFETY: XVisualInfo 是全零合法的 C POD。
    let mut info: xlib::XVisualInfo = unsafe { std::mem::zeroed() };
    // SAFETY: dpy 已打开；info 是有效的输出存储。
    let found = unsafe { (xlib.XMatchVisualInfo)(dpy, screen, 32, TrueColor, &mut info) };
    if found == 0 || info.depth != 32 || info.class != TrueColor || info.visual.is_null() {
        tracing::debug!(screen, "cursor trail: no depth-32 TrueColor visual");
        return None;
    }
    if info.red_mask != 0xff_0000 || info.green_mask != 0xff00 || info.blue_mask != 0xff {
        tracing::debug!(
            red = info.red_mask,
            green = info.green_mask,
            blue = info.blue_mask,
            "cursor trail: unexpected ARGB visual masks"
        );
        return None;
    }
    Some((info.visual, info.depth))
}

fn close_display(xlib: &Xlib, dpy: *mut Display) {
    if !dpy.is_null() {
        // SAFETY: dpy 由 XOpenDisplay 返回，此后不再使用。
        unsafe { (xlib.XCloseDisplay)(dpy) };
    }
}

/// override-redirect 的 ARGB 覆盖窗口。只在创建它的线程上使用，Drop 负责销毁。
pub(crate) struct OverlayWindow {
    conn: Rc<Connection>,
    id: xlib::Window,
    gc: xlib::GC,
    upload: Option<shm::Upload>,
    upload_failed: bool,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    mapped: bool,
}

impl Connection {
    /// 排空这条连接上的待处理事件（只在连接所属线程调用）。
    pub(crate) fn drain_events(&self, mut handle: impl FnMut(&xlib::XEvent)) {
        loop {
            // SAFETY: XPending 只读连接状态，不消耗事件。
            if unsafe { (self.xlib.XPending)(self.dpy) } <= 0 {
                return;
            }
            // SAFETY: XEvent 是全零合法的 C POD；XNextEvent 写入一个完整事件。
            let mut event: xlib::XEvent = unsafe { std::mem::zeroed() };
            // SAFETY: dpy 有效，event 是有效的输出存储。
            unsafe { (self.xlib.XNextEvent)(self.dpy, &mut event) };
            handle(&event);
        }
    }

    /// 测试专用：等服务器处理完这条连接上的上传与输入 shape（XShm 是异步的）。
    #[cfg(test)]
    pub(crate) fn sync(&self) {
        // SAFETY: dpy 有效；discard=false。
        unsafe { (self.xlib.XSync)(self.dpy, 0) };
    }
}

impl OverlayWindow {
    /// 窗口 ID，用于把收到的事件对回所属覆盖窗口。
    pub(crate) fn id(&self) -> xlib::Window {
        self.id
    }

    /// 订阅事件（在映射前调用）。
    pub(crate) fn select_events(&self, mask: i64) {
        // SAFETY: id 属于同一条连接；掩码只包含 Xlib 事件位。
        unsafe { (self.conn.xlib.XSelectInput)(self.conn.dpy, self.id, mask) };
    }

    /// 按缓存的像素缓冲重新上传（Expose 重绘，不重建像素）。
    pub(crate) fn redraw(&mut self) -> bool {
        if self.upload.is_none() {
            return false;
        }
        let Some(upload) = self.upload.as_mut() else {
            return false;
        };
        if !upload.put(self.id, self.gc) {
            self.upload = None;
            self.upload_failed = true;
            return false;
        }
        true
    }

    /// 设置窗口的输入区域；空切片即完全穿透。
    pub(crate) fn set_input_shape(&self, rects: &[xlib::XRectangle]) {
        shape::input_shape(&self.conn, self.id, rects);
    }

    /// 在根窗口的 `rect`（设备像素，X11 根坐标）处创建窗口；不映射。
    pub(crate) fn new(conn: Rc<Connection>, rect: CellRect) -> Option<Self> {
        let (x, y, width, height) = window_geometry(rect)?;
        // SAFETY: XSetWindowAttributes 是全零合法的 C POD；下面只设置已知字段。
        let mut attributes: xlib::XSetWindowAttributes = unsafe { std::mem::zeroed() };
        attributes.override_redirect = 1;
        // None：没有背景，内容完全由每帧上传写入。
        attributes.background_pixmap = 0;
        attributes.colormap = conn.colormap;
        attributes.border_pixel = 0;
        let mask = xlib::CWOverrideRedirect | xlib::CWBackPixmap | xlib::CWColormap | xlib::CWBorderPixel;
        // SAFETY: conn 持有打开的 display 与该屏幕的 depth-32 visual/colormap；
        // 参数描述根窗口下的一个 override-redirect 32 位子窗口。
        let id = unsafe {
            (conn.xlib.XCreateWindow)(
                conn.dpy,
                conn.root,
                x,
                y,
                width,
                height,
                0,
                conn.depth,
                xlib::InputOutput as u32,
                conn.visual,
                mask,
                &mut attributes,
            )
        };
        if id == 0 {
            tracing::warn!(x, y, width, height, "cursor trail: XCreateWindow failed");
            return None;
        }
        // SAFETY: id 是此连接上的有效窗口。
        let gc = unsafe { (conn.xlib.XCreateGC)(conn.dpy, id, 0, ptr::null_mut()) };
        if gc.is_null() {
            tracing::warn!("cursor trail: XCreateGC failed");
            // SAFETY: id 刚创建，此后不再使用。
            unsafe { (conn.xlib.XDestroyWindow)(conn.dpy, id) };
            return None;
        }
        // SAFETY: gc 属于此连接，values 是有效的栈存储。
        let mut values: xlib::XGCValues = unsafe { std::mem::zeroed() };
        values.graphics_exposures = 0;
        // SAFETY: gc 与 values 都有效；只改 GraphicsExpose 行为。
        unsafe { (conn.xlib.XChangeGC)(conn.dpy, gc, xlib::GCGraphicsExposures as xlib::Mask, &mut values) };
        shape::empty_input_shape(&conn, id);
        // SAFETY: 标准的连接刷新。
        unsafe { (conn.xlib.XFlush)(conn.dpy) };
        Some(Self {
            conn,
            id,
            gc,
            upload: None,
            upload_failed: false,
            x,
            y,
            width,
            height,
            mapped: false,
        })
    }

    /// 移动到 `rect` 并调整大小；尺寸变化时丢弃旧的像素上传缓冲。
    pub(crate) fn configure(&mut self, rect: CellRect) -> bool {
        let Some((x, y, width, height)) = window_geometry(rect) else {
            return false;
        };
        if (x, y, width, height) == (self.x, self.y, self.width, self.height) {
            return true;
        }
        if (width, height) != (self.width, self.height) {
            self.upload = None;
            self.upload_failed = false;
        }
        // SAFETY: id 是有效窗口；尺寸已限制在 16 位范围内。
        unsafe { (self.conn.xlib.XMoveResizeWindow)(self.conn.dpy, self.id, x, y, width, height) };
        (self.x, self.y, self.width, self.height) = (x, y, width, height);
        true
    }

    /// 上传一帧预乘 BGRA 像素（`width × height`，与本窗口当前尺寸一致）。
    pub(crate) fn upload(&mut self, pixels: &[u32]) -> bool {
        let (width, height) = (self.width as usize, self.height as usize);
        if pixels.len() != width * height {
            tracing::debug!(
                pixels = pixels.len(),
                expected = width * height,
                "cursor trail: pixel buffer size mismatch"
            );
            return false;
        }
        if self.upload.as_ref().is_none_or(|u| u.dimensions() != (width, height)) {
            self.upload = None;
        }
        if self.upload.is_none() {
            if self.upload_failed {
                return false;
            }
            self.upload = shm::Upload::new(&self.conn, width, height);
            if self.upload.is_none() {
                self.upload_failed = true;
                tracing::warn!(width, height, "cursor trail: cannot allocate an X11 pixel uploader");
                return false;
            }
        }
        let Some(upload) = self.upload.as_mut() else {
            return false;
        };
        if !upload.write(pixels) {
            tracing::debug!("cursor trail: writing into the X11 pixel buffer failed");
            return false;
        }
        if !upload.put(self.id, self.gc) {
            self.upload = None;
            self.upload_failed = true;
            tracing::warn!("cursor trail: X11 pixel upload failed");
            return false;
        }
        true
    }

    /// 映射并置顶（overlay 窗口不经过窗口管理器，XMapRaised 即最上层）。
    pub(crate) fn map_raised(&mut self) {
        if self.mapped {
            return;
        }
        // SAFETY: id 是此连接上的有效 override-redirect 窗口。
        unsafe {
            (self.conn.xlib.XMapRaised)(self.conn.dpy, self.id);
            (self.conn.xlib.XFlush)(self.conn.dpy);
        }
        self.mapped = true;
    }

    /// 撤掉窗口但保留它，下一次显示时重新映射。
    pub(crate) fn unmap(&mut self) {
        if !self.mapped {
            return;
        }
        // SAFETY: id 是此连接上的有效窗口。
        unsafe {
            (self.conn.xlib.XUnmapWindow)(self.conn.dpy, self.id);
            (self.conn.xlib.XFlush)(self.conn.dpy);
        }
        self.mapped = false;
    }

    /// 显式销毁；Drop 也会做同样的事。
    pub(crate) fn destroy(mut self) {
        self.cleanup();
    }

    fn cleanup(&mut self) {
        // 先在连接还活着时释放共享内存/XImage，再销毁窗口。
        self.upload = None;
        self.upload_failed = true;
        if !self.gc.is_null() {
            // SAFETY: gc 由此对象创建，此后不再使用。
            unsafe { (self.conn.xlib.XFreeGC)(self.conn.dpy, self.gc) };
            self.gc = ptr::null_mut();
        }
        if self.id != 0 {
            // SAFETY: id 由此对象创建，此后不再使用。
            unsafe {
                (self.conn.xlib.XDestroyWindow)(self.conn.dpy, self.id);
                (self.conn.xlib.XFlush)(self.conn.dpy);
            }
            self.id = 0;
        }
        self.mapped = false;
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn window_geometry(rect: CellRect) -> Option<(i32, i32, u32, u32)> {
    if rect.is_empty() || rect.w > MAX_DIMENSION || rect.h > MAX_DIMENSION {
        return None;
    }
    if !(MIN_COORD..=MAX_COORD).contains(&rect.x) || !(MIN_COORD..=MAX_COORD).contains(&rect.y) {
        return None;
    }
    Some((rect.x as i32, rect.y as i32, rect.w, rect.h))
}
