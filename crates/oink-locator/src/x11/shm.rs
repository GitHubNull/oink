//! ARGB 像素上传：XShm（MIT-SHM）优先，失败或不可用时退回 XPutImage。
//!
//! 两种上传器都持有 `Rc<Connection>`，因此只能在连接所属线程使用；Drop 会 Detach
//! 共享段或释放 XImage。像素是预乘 BGRA，长度必须恰为 `width × height`。

use std::ffi::c_void;
use std::ptr;
use std::rc::Rc;

use libc::{IPC_CREAT, IPC_PRIVATE, IPC_RMID, shmat, shmctl, shmdt, shmget};
use x11_dl::xlib::{self, GC, Window, XImage, ZPixmap};
use x11_dl::xshm::XShmSegmentInfo;

use super::Connection;

/// 覆盖窗口最多 32M 像素（128 MiB 的 BGRA）；拖尾窗口只覆盖拖尾本身，远小于此。
const MAX_PIXELS: usize = 32 * 1024 * 1024;

/// 当前帧的像素上传通道。
pub(crate) enum Upload {
    Shm(ShmBuffer),
    Put(PutImage),
}

impl Upload {
    /// 优先 XShm；共享内存不可用或附着失败时退回 XPutImage。
    pub(crate) fn new(conn: &Rc<Connection>, width: usize, height: usize) -> Option<Self> {
        let pixels = width.checked_mul(height)?;
        if width == 0 || height == 0 || pixels > MAX_PIXELS {
            return None;
        }
        if conn.xshm.is_some() {
            match ShmBuffer::new(Rc::clone(conn), width, height) {
                Some(buffer) => return Some(Self::Shm(buffer)),
                None => tracing::debug!(
                    width,
                    height,
                    "cursor trail: XShm attach failed; falling back to XPutImage"
                ),
            }
        }
        PutImage::new(Rc::clone(conn), width, height).map(Self::Put)
    }

    pub(crate) fn dimensions(&self) -> (usize, usize) {
        match self {
            Self::Shm(buffer) => buffer.dimensions(),
            Self::Put(image) => image.dimensions(),
        }
    }

    pub(crate) fn write(&mut self, pixels: &[u32]) -> bool {
        match self {
            Self::Shm(buffer) => buffer.write(pixels),
            Self::Put(image) => image.write(pixels),
        }
    }

    pub(crate) fn put(&mut self, window: Window, gc: GC) -> bool {
        match self {
            Self::Shm(buffer) => buffer.put(window, gc),
            Self::Put(image) => image.put(window, gc),
        }
    }
}

/// XImage 的 `byte_order` 必须与本机一致，u32 像素才能按原样上传。
fn byte_order_matches(byte_order: i32) -> bool {
    let little = cfg!(target_endian = "little");
    (byte_order == xlib::LSBFirst) == little
}

/// 把每格一个 u32 的预乘 BGRA 图复制进 XImage 的 `data` 缓冲（按 `bytes_per_line` 逐行）。
fn write_rows(image: *mut XImage, pixels: &[u32], width: usize, height: usize) -> bool {
    if image.is_null() || pixels.len() != width * height {
        return false;
    }
    // SAFETY: image 由本模块创建且未释放；读取其不可变的布局字段，并且只写入
    // Xlib 为该 XImage 分配、至少 bytes_per_line × height 字节的 data 缓冲。
    unsafe {
        let data = (*image).data;
        let stride = (*image).bytes_per_line;
        let bits = (*image).bits_per_pixel;
        if data.is_null() || bits != 32 || stride <= 0 || (stride as usize) < width * 4 {
            return false;
        }
        let stride = stride as usize;
        if stride == width * 4 {
            ptr::copy_nonoverlapping(pixels.as_ptr().cast::<u8>(), data.cast::<u8>(), width * height * 4);
        } else {
            for y in 0..height {
                ptr::copy_nonoverlapping(
                    pixels[y * width..].as_ptr().cast::<u8>(),
                    data.cast::<u8>().add(y * stride),
                    width * 4,
                );
            }
        }
    }
    true
}

/// 一块附着到服务器的 SysV 共享内存，配合 XShmPutImage 使用。
pub(crate) struct ShmBuffer {
    conn: Rc<Connection>,
    info: XShmSegmentInfo,
    image: *mut XImage,
    address: *mut c_void,
    width: usize,
    height: usize,
}

impl ShmBuffer {
    fn new(conn: Rc<Connection>, width: usize, height: usize) -> Option<Self> {
        let (info, image, address) = attach(&conn, width, height)?;
        // SAFETY: image 非空且由 XShmCreateImage 创建；布局字段不可变。
        let (bits, byte_order, stride, data) = unsafe {
            (
                (*image).bits_per_pixel,
                (*image).byte_order,
                (*image).bytes_per_line,
                (*image).data,
            )
        };
        // 共享段按稠密行分配，所以这里也要求 Xlib 不填充行尾。
        if bits != 32 || !byte_order_matches(byte_order) || data.is_null() || stride as usize != width * 4 {
            tracing::debug!(bits, byte_order, stride, "cursor trail: unexpected XShm image layout");
            let mut info = info;
            detach(&conn, &mut info, image, address);
            return None;
        }
        Some(Self {
            conn,
            info,
            image,
            address,
            width,
            height,
        })
    }

    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn write(&mut self, pixels: &[u32]) -> bool {
        write_rows(self.image, pixels, self.width, self.height)
    }

    fn put(&mut self, window: Window, gc: GC) -> bool {
        let Some(xshm) = self.conn.xshm.as_ref() else {
            return false;
        };
        // SAFETY: image/info 已附着在这条连接上；window 与 gc 同属该连接。
        let ok = unsafe {
            (xshm.XShmPutImage)(
                self.conn.dpy,
                window,
                gc,
                self.image,
                0,
                0,
                0,
                0,
                self.width as u32,
                self.height as u32,
                0,
            )
        };
        if ok == 0 {
            return false;
        }
        // 服务器读完共享内存后才能覆盖同一块内存，因此每帧同步一次。
        // SAFETY: 标准 XSync，连接有效。
        unsafe { (self.conn.xlib.XSync)(self.conn.dpy, 0) };
        true
    }

    fn cleanup(&mut self) {
        if !self.image.is_null() {
            // SAFETY: image 与 info 属于此连接；在连接存活时 Detach，并把 data 置空，
            // 避免 XDestroyImage 释放 shmat 得到的内存。
            unsafe {
                if let Some(xshm) = self.conn.xshm.as_ref() {
                    (xshm.XShmDetach)(self.conn.dpy, &mut self.info);
                    (self.conn.xlib.XSync)(self.conn.dpy, 0);
                }
                (*self.image).data = ptr::null_mut();
                (self.conn.xlib.XDestroyImage)(self.image);
            }
            self.image = ptr::null_mut();
        }
        if !self.address.is_null() {
            // SAFETY: address 来自 shmat，且已从服务器 Detach。
            unsafe {
                shmctl(self.info.shmid, IPC_RMID, ptr::null_mut());
                shmdt(self.address);
            }
            self.address = ptr::null_mut();
        }
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// 创建、附着共享段并建立 XShm XImage；失败时释放所有已取得的资源。
fn attach(conn: &Connection, width: usize, height: usize) -> Option<(XShmSegmentInfo, *mut XImage, *mut c_void)> {
    let xshm = conn.xshm.as_ref()?;
    let size = width.checked_mul(height)?.checked_mul(4)?;
    let width = u32::try_from(width).ok()?;
    let height = u32::try_from(height).ok()?;
    // SAFETY: SysV 共享内存的常规创建；IPC_PRIVATE 每次产生新段。
    let shmid = unsafe { shmget(IPC_PRIVATE, size, IPC_CREAT | 0o600) };
    if shmid < 0 {
        tracing::debug!(size, "cursor trail: shmget failed");
        return None;
    }
    // SAFETY: 段由上一行创建；null 让内核选择地址。
    let address = unsafe { shmat(shmid, ptr::null(), 0) };
    if address as isize == -1 {
        tracing::debug!("cursor trail: shmat failed");
        // SAFETY: 段没有任何附着者，立即删除。
        unsafe { shmctl(shmid, IPC_RMID, ptr::null_mut()) };
        return None;
    }
    let mut info = XShmSegmentInfo {
        shmseg: 0,
        shmid,
        shmaddr: address.cast(),
        readOnly: 0,
    };
    // SAFETY: dpy/visual 属于此连接；address 是至少 size 字节的已附着段。
    let image = unsafe {
        (xshm.XShmCreateImage)(
            conn.dpy,
            conn.visual,
            conn.depth as u32,
            ZPixmap,
            address.cast(),
            &mut info,
            width,
            height,
        )
    };
    if image.is_null() {
        tracing::debug!("cursor trail: XShmCreateImage failed");
        cleanup_unattached(shmid, address);
        return None;
    }
    // SAFETY: image 刚创建；读取布局字段。
    let stride = unsafe { (*image).bytes_per_line };
    if stride as usize != (width as usize) * 4 {
        tracing::debug!(stride, width, "cursor trail: XShm image has padded rows");
        // SAFETY: image 的 data 指向共享段，置空后只释放 XImage 结构本身。
        unsafe {
            (*image).data = ptr::null_mut();
            (conn.xlib.XDestroyImage)(image);
        }
        cleanup_unattached(shmid, address);
        return None;
    }
    // SAFETY: info/image 在 Detach 前一直存活；服务器附着共享段。
    let attached = unsafe { (xshm.XShmAttach)(conn.dpy, &mut info) };
    if attached == 0 {
        tracing::debug!("cursor trail: XShmAttach failed");
        // SAFETY: 附着失败，data 指向共享段，置空后再释放 XImage。
        unsafe {
            (*image).data = ptr::null_mut();
            (conn.xlib.XDestroyImage)(image);
        }
        cleanup_unattached(shmid, address);
        return None;
    }
    // 服务器在 XSync 后已附着。立刻把段标记为待删除，最后一个 shmdt 会释放内存。
    // SAFETY: XSync 等待服务器处理完附着；shmctl 只做标记。
    unsafe {
        (conn.xlib.XSync)(conn.dpy, 0);
        shmctl(shmid, IPC_RMID, ptr::null_mut());
    }
    Some((info, image, address))
}

fn cleanup_unattached(shmid: i32, address: *mut c_void) {
    // SAFETY: 段未被服务器附着；标记删除后 detach 即释放。
    unsafe {
        shmctl(shmid, IPC_RMID, ptr::null_mut());
        shmdt(address);
    }
}

/// 析构或布局不符时：Detach、释放 XImage、释放共享段。
fn detach(conn: &Connection, info: &mut XShmSegmentInfo, image: *mut XImage, address: *mut c_void) {
    if !image.is_null() {
        // SAFETY: image 由 XShmCreateImage 创建；data 指向共享段，置空后只释放结构体。
        unsafe {
            if let Some(xshm) = conn.xshm.as_ref() {
                (xshm.XShmDetach)(conn.dpy, info);
                (conn.xlib.XSync)(conn.dpy, 0);
            }
            (*image).data = ptr::null_mut();
            (conn.xlib.XDestroyImage)(image);
        }
    }
    if !address.is_null() {
        // SAFETY: address 来自 shmat，且已从服务器 Detach。
        unsafe {
            shmctl(info.shmid, IPC_RMID, ptr::null_mut());
            shmdt(address);
        }
    }
}

/// XPutImage 的退路：XCreateImage 自己分配像素缓冲，每帧复制后整图上传。
pub(crate) struct PutImage {
    conn: Rc<Connection>,
    image: *mut XImage,
    width: usize,
    height: usize,
}

impl PutImage {
    fn new(conn: Rc<Connection>, width: usize, height: usize) -> Option<Self> {
        let uw = u32::try_from(width).ok()?;
        let uh = u32::try_from(height).ok()?;
        // SAFETY: data 传 null 让 Xlib 分配像素缓冲（由 XImage 持有）；参数来自有效连接。
        let image = unsafe {
            (conn.xlib.XCreateImage)(
                conn.dpy,
                conn.visual,
                conn.depth as u32,
                ZPixmap,
                0,
                ptr::null_mut(),
                uw,
                uh,
                32,
                0,
            )
        };
        if image.is_null() {
            tracing::warn!(width, height, "cursor trail: XCreateImage failed");
            return None;
        }
        // SAFETY: image 刚创建；读取布局字段。
        let (bits, byte_order, data) = unsafe { ((*image).bits_per_pixel, (*image).byte_order, (*image).data) };
        if bits != 32 || !byte_order_matches(byte_order) || data.is_null() {
            tracing::debug!(bits, byte_order, "cursor trail: unexpected XImage layout");
            // SAFETY: image 未共享，直接释放。
            unsafe { (conn.xlib.XDestroyImage)(image) };
            return None;
        }
        Some(Self {
            conn,
            image,
            width,
            height,
        })
    }

    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn write(&mut self, pixels: &[u32]) -> bool {
        write_rows(self.image, pixels, self.width, self.height)
    }

    fn put(&mut self, window: Window, gc: GC) -> bool {
        // SAFETY: image、window 与 gc 都属于此连接；XPutImage 在返回前已把像素复制进请求。
        unsafe {
            (self.conn.xlib.XPutImage)(
                self.conn.dpy,
                window,
                gc,
                self.image,
                0,
                0,
                0,
                0,
                self.width as u32,
                self.height as u32,
            );
            (self.conn.xlib.XFlush)(self.conn.dpy);
        }
        true
    }
}

impl Drop for PutImage {
    fn drop(&mut self) {
        if !self.image.is_null() {
            // SAFETY: XCreateImage 分配了图像与像素缓冲，这里释放一次。
            unsafe { (self.conn.xlib.XDestroyImage)(self.image) };
            self.image = ptr::null_mut();
        }
    }
}
