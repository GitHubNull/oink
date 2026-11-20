//! XRandR 枚举：活动监视器、名称 atom、CRTC 旋转与物理尺寸；没有 RandR 监视器时退回根窗口几何。

use std::ffi::CStr;

use oink_foundation::display::{DisplayInfo, REFERENCE_DIAGONAL_METERS};
use oink_foundation::{Rect, Size};
use x11_dl::xlib::{Display, Xlib};
use x11_dl::xrandr::{self as xrr, XRRMonitorInfo, Xrandr};

use crate::DisplayError;

pub(super) fn enumerate(xlib: &Xlib, xrandr: &Xrandr, dpy: *mut Display) -> Result<Vec<DisplayInfo>, DisplayError> {
    let mut event_base = 0;
    let mut error_base = 0;
    // SAFETY: dpy is an open connection and the two out-pointers are valid storage.
    let has_randr = unsafe { (xrandr.XRRQueryExtension)(dpy, &mut event_base, &mut error_base) } != 0;
    if !has_randr {
        return Err(DisplayError::Platform("X 服务器没有 XRandR 扩展".into()));
    }
    // SAFETY: the root window of the default screen exists for this connection.
    let root = unsafe { (xlib.XDefaultRootWindow)(dpy) };
    let mut count = 0;
    // SAFETY: get_active=1 asks only for attached, active monitors; count receives the array length.
    let monitors = unsafe { (xrandr.XRRGetMonitors)(dpy, root, 1, &mut count) };
    if monitors.is_null() || count <= 0 {
        return Ok(vec![default_screen(xlib, dpy)]);
    }
    // Rotation lives on the CRTC, not the monitor: consult the bounded output list.
    // SAFETY: root belongs to this connection; a null result is handled below.
    let resources = unsafe { (xrandr.XRRGetScreenResourcesCurrent)(dpy, root) };
    let mut out = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        // SAFETY: index is within the array returned with length count.
        let monitor = unsafe { &*monitors.add(index) };
        let atom_name = atom_name(xlib, dpy, monitor.name);
        let fallback = || format!("Monitor {}", index + 1);
        let name = atom_name.filter(|n| !n.is_empty()).unwrap_or_else(fallback);
        let rotation_deg = rotation_of(xrandr, dpy, resources, monitor);
        let reference_extent = reference_extent(monitor, rotation_deg);
        out.push(DisplayInfo {
            key: format!("x11:{name}"),
            name: name.clone(),
            bounds: Rect::new(
                f64::from(monitor.x),
                f64::from(monitor.y),
                f64::from(monitor.width.max(0)),
                f64::from(monitor.height.max(0)),
            ),
            pixel_width: monitor.width.max(0) as u32,
            pixel_height: monitor.height.max(0) as u32,
            reference_extent,
            rotation_deg,
            is_primary: monitor.primary != 0,
            is_builtin: is_builtin(&name),
            is_virtual: looks_virtual(&name),
        });
    }
    // SAFETY: both arrays were allocated by XRandR for this connection and are freed once here.
    unsafe {
        (xrandr.XRRFreeMonitors)(monitors);
        if !resources.is_null() {
            (xrandr.XRRFreeScreenResources)(resources);
        }
    }
    Ok(out)
}

fn atom_name(xlib: &Xlib, dpy: *mut Display, atom: x11_dl::xlib::Atom) -> Option<String> {
    if atom == 0 {
        return None;
    }
    // SAFETY: atom belongs to this connection; a null name is checked before CStr reads it.
    unsafe {
        let name = (xlib.XGetAtomName)(dpy, atom);
        if name.is_null() {
            return None;
        }
        let text = CStr::from_ptr(name).to_string_lossy().into_owned();
        (xlib.XFree)(name.cast());
        Some(text)
    }
}

/// Rotation of the monitor's first connected output; 0 when no CRTC or no outputs.
fn rotation_of(
    xrandr: &Xrandr,
    dpy: *mut Display,
    resources: *mut xrr::XRRScreenResources,
    monitor: &XRRMonitorInfo,
) -> u16 {
    if resources.is_null() || monitor.noutput <= 0 {
        return 0;
    }
    for index in 0..monitor.noutput {
        // SAFETY: index is within the monitor's own output array.
        let output = unsafe { *monitor.outputs.add(index as usize) };
        // SAFETY: resources is the live screen resources array; output came from this server.
        unsafe {
            let info = (xrandr.XRRGetOutputInfo)(dpy, resources, output);
            if info.is_null() {
                continue;
            }
            let crtc = (*info).crtc;
            let connection = (*info).connection;
            (xrandr.XRRFreeOutputInfo)(info);
            if crtc == 0 || i32::from(connection) != xrr::RR_Connected {
                continue;
            }
            let crtc_info = (xrandr.XRRGetCrtcInfo)(dpy, resources, crtc);
            if crtc_info.is_null() {
                continue;
            }
            let rotation = (*crtc_info).rotation;
            (xrandr.XRRFreeCrtcInfo)(crtc_info);
            return rotation_deg(rotation);
        }
    }
    0
}

pub(super) fn rotation_deg(rotation: x11_dl::xrandr::Rotation) -> u16 {
    let rotation = i32::from(rotation);
    if rotation & xrr::RR_Rotate_90 != 0 {
        90
    } else if rotation & xrr::RR_Rotate_180 != 0 {
        180
    } else if rotation & xrr::RR_Rotate_270 != 0 {
        270
    } else {
        0
    }
}

/// Physical size from the monitor's millimetre fields, normalized like other platforms.
/// XRandR reports unrotated panel dimensions, so swap them for 90/270.
fn reference_extent(monitor: &XRRMonitorInfo, rotation_deg: u16) -> Option<Size> {
    let (mw, mh) = (f64::from(monitor.mwidth) / 1000.0, f64::from(monitor.mheight) / 1000.0);
    if mw <= 0.001 || mh <= 0.001 {
        return None;
    }
    let (mw, mh) = if rotation_deg == 90 || rotation_deg == 270 {
        (mh, mw)
    } else {
        (mw, mh)
    };
    Some(Size::new(
        mw / REFERENCE_DIAGONAL_METERS,
        mh / REFERENCE_DIAGONAL_METERS,
    ))
}

/// Some minimal servers report no RandR monitors; the root screen geometry is still real.
fn default_screen(xlib: &Xlib, dpy: *mut Display) -> DisplayInfo {
    // SAFETY: dpy is open; the default screen index and its geometry are read-only queries.
    let (width, height, mm_width, mm_height) = unsafe {
        let screen = (xlib.XDefaultScreen)(dpy);
        (
            (xlib.XDisplayWidth)(dpy, screen),
            (xlib.XDisplayHeight)(dpy, screen),
            (xlib.XDisplayWidthMM)(dpy, screen),
            (xlib.XDisplayHeightMM)(dpy, screen),
        )
    };
    let (width, height) = (width.max(0), height.max(0));
    let reference_extent = if mm_width > 0 && mm_height > 0 {
        Some(Size::new(
            f64::from(mm_width) / 1000.0 / REFERENCE_DIAGONAL_METERS,
            f64::from(mm_height) / 1000.0 / REFERENCE_DIAGONAL_METERS,
        ))
    } else {
        None
    };
    DisplayInfo {
        key: "x11:screen".into(),
        name: "X11 Screen".into(),
        bounds: Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
        pixel_width: width as u32,
        pixel_height: height as u32,
        reference_extent,
        rotation_deg: 0,
        is_primary: true,
        is_builtin: false,
        is_virtual: false,
    }
}

fn is_builtin(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["edp", "lvds", "dsi", "embedded"].iter().any(|k| name.starts_with(k))
}

fn looks_virtual(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "virtual",
        "dummy",
        "headless",
        "vnc",
        "remote display",
        "parsec",
        "deskpad",
    ]
    .iter()
    .any(|k| name.contains(k))
}
