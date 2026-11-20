//! 截获线程用到的 X11 查询与事件选择辅助函数。

use x11_dl::xinput2::{
    XI_HierarchyChanged, XI_RawButtonPress, XI_RawButtonRelease, XI_RawKeyPress, XI_RawKeyRelease, XI_RawMotion,
    XIAllDevices, XIAllMasterDevices, XIEventMask,
};
use x11_dl::xlib::{Display, Xlib};

use oink_foundation::Point;

use crate::InputError;

pub(super) fn extension_opcode(xlib: &Xlib, dpy: *mut Display) -> Result<i32, InputError> {
    let name = std::ffi::CString::new("XInputExtension").map_err(|_| InputError::Platform("扩展名无效".into()))?;
    let (mut opcode, mut first_event, mut first_error) = (0, 0, 0);
    // SAFETY: dpy 有效；name 以 NUL 结尾；三个 out 指针有效。
    let present =
        unsafe { (xlib.XQueryExtension)(dpy, name.as_ptr(), &mut opcode, &mut first_event, &mut first_error) };
    if present == 0 {
        return Err(InputError::Unsupported);
    }
    Ok(opcode)
}

/// 主动抓取期间同一连接收不到 raw 事件，因此 raw 观察必须挂在独立连接上。
pub(super) fn select_raw(xlib: &Xlib, xi: &x11_dl::xinput2::XInput2, dpy: *mut Display) -> Result<(), InputError> {
    let raw_kinds = [
        XI_RawKeyPress,
        XI_RawKeyRelease,
        XI_RawButtonPress,
        XI_RawButtonRelease,
        XI_RawMotion,
    ];
    let mut raw_mask = [0_u8; 4];
    for kind in raw_kinds {
        raw_mask[kind as usize / 8] |= 1 << (kind as usize % 8);
    }
    let mut selection = [XIEventMask {
        deviceid: XIAllMasterDevices,
        mask_len: raw_mask.len() as i32,
        mask: raw_mask.as_mut_ptr(),
    }];
    // SAFETY: dpy 有效；root 来自同一连接；selection 在调用期间存活。
    let root = unsafe { (xlib.XDefaultRootWindow)(dpy) };
    // SAFETY: dpy 有效；selection 指向存活的掩码数组；root 来自同一连接。
    let status = unsafe { (xi.XISelectEvents)(dpy, root, selection.as_mut_ptr(), selection.len() as i32) };
    if status != 0 {
        return Err(InputError::Platform(format!("XI2 选择 raw 事件失败（状态 {status}）")));
    }
    Ok(())
}

/// XI_HierarchyChanged 只能挂在 XIAllDevices 上；master 设备选择会得到 BadValue。
pub(super) fn select_hierarchy(
    xlib: &Xlib,
    xi: &x11_dl::xinput2::XInput2,
    dpy: *mut Display,
) -> Result<(), InputError> {
    let mut hierarchy_mask = [0_u8; 4];
    hierarchy_mask[XI_HierarchyChanged as usize / 8] |= 1 << (XI_HierarchyChanged as usize % 8);
    let mut selection = [XIEventMask {
        deviceid: XIAllDevices,
        mask_len: hierarchy_mask.len() as i32,
        mask: hierarchy_mask.as_mut_ptr(),
    }];
    // SAFETY: dpy 有效；root 来自同一连接；selection 在调用期间存活。
    let root = unsafe { (xlib.XDefaultRootWindow)(dpy) };
    // SAFETY: dpy 有效；selection 指向存活的掩码数组；root 来自同一连接。
    let status = unsafe { (xi.XISelectEvents)(dpy, root, selection.as_mut_ptr(), selection.len() as i32) };
    if status != 0 {
        return Err(InputError::Platform(format!("XI2 选择层级事件失败（状态 {status}）")));
    }
    Ok(())
}

pub(super) fn query_cursor(xlib: &Xlib, dpy: *mut Display) -> Option<Point> {
    // SAFETY: dpy 有效且由截获线程独占。
    let root = unsafe { (xlib.XDefaultRootWindow)(dpy) };
    let (mut root_return, mut child_return) = (0, 0);
    let (mut root_x, mut root_y, mut win_x, mut win_y) = (0, 0, 0, 0);
    let mut mask = 0;
    // SAFETY: 所有 out 指针都是有效栈变量；root 来自同一连接。
    let ok = unsafe {
        (xlib.XQueryPointer)(
            dpy,
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
