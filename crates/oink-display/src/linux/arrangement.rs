//! 原生排列事务：XRandR 快照、镜像组、写前重读、失败回滚与新连接核验。
//!
//! 快照以输出名作 key（与 `DisplayInfo` 的 monitor 名一致）。同一 CRTC 上的其余输出是
//! 镜像 follower（`mirror_of` 指向源）；`apply` 只写有变化的源 CRTC，保留 mode、旋转与
//! outputs 数组，XSync 后用新连接重读核验；任何不符都按快照回滚并报 `Changed`。

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CStr;

use oink_foundation::Rect;
use x11_dl::xlib::{self, Display, Xlib};
use x11_dl::xrandr::{self as xrr, RRCrtc, RRMode, RROutput, Rotation, Xrandr};

use crate::DisplayError;
use crate::arrangement::{ArrangementError, NativeConfiguration, NativeDisplay, NativeOrigin};

/// 一个活动 CRTC 的原始参数；写入与回滚时原样带回。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Handle {
    crtc: RRCrtc,
    mode: RRMode,
    rotation: Rotation,
    outputs: Vec<RROutput>,
}

struct Snapshot {
    config: NativeConfiguration,
    /// 源显示 key（非镜像）→ CRTC 参数。
    handles: BTreeMap<String, Handle>,
}

struct Group {
    source: String,
    x: i32,
    y: i32,
    rect: Rect,
    mode: String,
    handle: Handle,
    /// `(显示 key, 是否为 XRandR 主输出)`。
    outputs: Vec<(String, bool)>,
}

pub(super) fn configuration(xlib: &Xlib, xrandr: &Xrandr) -> Result<NativeConfiguration, DisplayError> {
    let dpy = super::open_display(xlib)?;
    let result = snapshot(xlib, xrandr, dpy).map(|s| s.config);
    // SAFETY: dpy was opened in this function and is closed exactly once.
    unsafe {
        (xlib.XCloseDisplay)(dpy);
    }
    result
}

pub(super) fn apply(
    xlib: &Xlib,
    xrandr: &Xrandr,
    expected: &NativeConfiguration,
    origins: &[NativeOrigin],
) -> Result<NativeConfiguration, DisplayError> {
    let dpy = super::open_display(xlib)?;
    let result = apply_on(xlib, xrandr, dpy, expected, origins);
    // SAFETY: dpy was opened in this function and is closed exactly once.
    unsafe {
        (xlib.XCloseDisplay)(dpy);
    }
    result
}

fn apply_on(
    xlib: &Xlib,
    xrandr: &Xrandr,
    dpy: *mut Display,
    expected: &NativeConfiguration,
    origins: &[NativeOrigin],
) -> Result<NativeConfiguration, DisplayError> {
    let (resources, root) = screen_resources(xlib, xrandr, dpy)?;
    let result = try_apply(xlib, xrandr, dpy, resources, root, expected, origins);
    // SAFETY: resources was allocated for this connection and is freed exactly once.
    unsafe {
        (xrandr.XRRFreeScreenResources)(resources);
    }
    result
}

fn try_apply(
    xlib: &Xlib,
    xrandr: &Xrandr,
    dpy: *mut Display,
    resources: *mut xrr::XRRScreenResources,
    root: xlib::Window,
    expected: &NativeConfiguration,
    origins: &[NativeOrigin],
) -> Result<NativeConfiguration, DisplayError> {
    let current = read(xrandr, dpy, resources, root)?;
    if current.config != *expected {
        return Err(ArrangementError::Changed.into());
    }
    let target = current.config.with_origins(origins)?;
    if target == current.config {
        return Ok(target);
    }
    let moves = plan(&current, &target)?;
    let mut writer = XrandrWrites {
        xlib,
        xrandr,
        dpy,
        resources,
    };
    commit(&mut writer, &moves)?;
    match configuration(xlib, xrandr) {
        Ok(observed) if observed == target => Ok(observed),
        Ok(_) => {
            let all: Vec<&Move> = moves.iter().collect();
            rollback(&mut writer, &all)?;
            Err(ArrangementError::Changed.into())
        }
        Err(error) => {
            let all: Vec<&Move> = moves.iter().collect();
            let _ = rollback(&mut writer, &all);
            Err(error)
        }
    }
}

fn snapshot(xlib: &Xlib, xrandr: &Xrandr, dpy: *mut Display) -> Result<Snapshot, DisplayError> {
    let (resources, root) = screen_resources(xlib, xrandr, dpy)?;
    let result = read(xrandr, dpy, resources, root);
    // SAFETY: resources was allocated for this connection and is freed exactly once.
    unsafe {
        (xrandr.XRRFreeScreenResources)(resources);
    }
    result
}

fn screen_resources(
    xlib: &Xlib,
    xrandr: &Xrandr,
    dpy: *mut Display,
) -> Result<(*mut xrr::XRRScreenResources, xlib::Window), DisplayError> {
    // SAFETY: dpy is an open connection; the default root window always exists.
    let root = unsafe { (xlib.XDefaultRootWindow)(dpy) };
    // SAFETY: root belongs to this connection; a null result is rejected below.
    let resources = unsafe { (xrandr.XRRGetScreenResourcesCurrent)(dpy, root) };
    if resources.is_null() {
        return Err(DisplayError::Platform("X 服务器没有 XRandR 扩展".into()));
    }
    Ok((resources, root))
}

fn read(
    xrandr: &Xrandr,
    dpy: *mut Display,
    resources: *mut xrr::XRRScreenResources,
    root: xlib::Window,
) -> Result<Snapshot, DisplayError> {
    // SAFETY: resources is a live array for this connection; the primary output is a read-only query.
    let (noutput, primary) = unsafe { ((*resources).noutput, (xrandr.XRRGetOutputPrimary)(dpy, root)) };
    let mut names: BTreeMap<RROutput, String> = BTreeMap::new();
    for index in 0..noutput as usize {
        // SAFETY: index is below the array length reported by the server.
        let output = unsafe { *(*resources).outputs.add(index) };
        // SAFETY: output came from the live resources array; the info is freed below.
        let info = unsafe { (xrandr.XRRGetOutputInfo)(dpy, resources, output) };
        if info.is_null() {
            return Err(ArrangementError::Changed.into());
        }
        // SAFETY: info owns its name until freed; both calls run exactly once.
        let name = unsafe {
            let raw = (*info).name;
            let text = if raw.is_null() {
                String::new()
            } else {
                CStr::from_ptr(raw).to_string_lossy().into_owned()
            };
            (xrandr.XRRFreeOutputInfo)(info);
            text
        };
        names.insert(output, name);
    }
    // SAFETY: resources is a live structure for this connection; the count is read-only.
    let ncrtc = unsafe { (*resources).ncrtc };
    let mut groups = Vec::new();
    for index in 0..ncrtc as usize {
        // SAFETY: index is below the array length reported by the server.
        let crtc = unsafe { *(*resources).crtcs.add(index) };
        // SAFETY: crtc came from the live resources array; the info is freed below.
        let info = unsafe { (xrandr.XRRGetCrtcInfo)(dpy, resources, crtc) };
        if info.is_null() {
            continue;
        }
        // SAFETY: every index is below noutput and both pointers are valid until the free below.
        let (x, y, width, height, mode, rotation, outputs) = unsafe {
            let outputs = (0..(*info).noutput)
                .map(|i| *(*info).outputs.add(i as usize))
                .collect::<Vec<_>>();
            (
                (*info).x,
                (*info).y,
                (*info).width,
                (*info).height,
                (*info).mode,
                (*info).rotation,
                outputs,
            )
        };
        // SAFETY: info was allocated by the call above and is freed exactly once.
        unsafe {
            (xrandr.XRRFreeCrtcInfo)(info);
        }
        if mode == 0 || outputs.is_empty() {
            continue;
        }
        let mut entries = Vec::with_capacity(outputs.len());
        for output in &outputs {
            let name = names.get(output).ok_or(ArrangementError::Changed)?;
            if name.is_empty() {
                return Err(ArrangementError::Changed.into());
            }
            entries.push((format!("x11:{name}"), *output == primary));
        }
        let source_index = outputs.iter().position(|o| *o == primary).unwrap_or(0);
        let source = entries[source_index].0.clone();
        groups.push(Group {
            source,
            x,
            y,
            rect: Rect::new(f64::from(x), f64::from(y), f64::from(width), f64::from(height)),
            mode: mode_identity(resources, mode, rotation),
            handle: Handle {
                crtc,
                mode,
                rotation,
                outputs,
            },
            outputs: entries,
        });
    }

    let primary_key = groups
        .iter()
        .find(|g| g.outputs.iter().any(|(_, is_primary)| *is_primary))
        .map(|g| g.source.clone())
        .or_else(|| groups.iter().find(|g| g.x == 0 && g.y == 0).map(|g| g.source.clone()))
        .or_else(|| groups.iter().map(|g| g.source.clone()).min());
    let mut config = NativeConfiguration { displays: Vec::new() };
    let mut handles = BTreeMap::new();
    let mut keys = BTreeSet::new();
    for group in groups {
        for (key, _) in &group.outputs {
            if !keys.insert(key.clone()) {
                return Err(ArrangementError::Changed.into());
            }
            config.displays.push(NativeDisplay {
                key: key.clone(),
                bounds: group.rect,
                primary: primary_key.as_deref() == Some(key.as_str()),
                mirror_of: (key != &group.source).then(|| group.source.clone()),
                mode: group.mode.clone(),
            });
        }
        if handles.insert(group.source, group.handle).is_some() {
            return Err(ArrangementError::Changed.into());
        }
    }
    config.canonicalize();
    Ok(Snapshot { config, handles })
}

fn mode_identity(resources: *mut xrr::XRRScreenResources, mode: RRMode, rotation: Rotation) -> String {
    let (mut width, mut height, mut refresh) = (0u32, 0u32, 0u64);
    // SAFETY: resources is a live structure for this connection; the count is read-only.
    let nmode = unsafe { (*resources).nmode };
    for index in 0..nmode as usize {
        // SAFETY: index is below the mode array length reported by the server.
        let info = unsafe { &*(*resources).modes.add(index) };
        if info.id == mode {
            width = info.width;
            height = info.height;
            if info.hTotal > 0 && info.vTotal > 0 {
                let total = u128::from(info.hTotal) * u128::from(info.vTotal);
                refresh = ((u128::from(info.dotClock) * 1000) / total) as u64;
            }
            break;
        }
    }
    format!("{mode}:{width}x{height}@{refresh}:{rotation}")
}

#[derive(Debug, PartialEq)]
struct Move {
    key: String,
    from: (i32, i32),
    to: (i32, i32),
    handle: Handle,
}

fn plan(current: &Snapshot, target: &NativeConfiguration) -> Result<Vec<Move>, DisplayError> {
    let mut moves = Vec::new();
    for source in target.sources() {
        let before = current
            .config
            .displays
            .iter()
            .find(|d| d.key == source.key)
            .ok_or(ArrangementError::Changed)?;
        let from = (before.bounds.x as i32, before.bounds.y as i32);
        let to = (source.bounds.x as i32, source.bounds.y as i32);
        if from == to {
            continue;
        }
        let handle = current
            .handles
            .get(&source.key)
            .ok_or(ArrangementError::Changed)?
            .clone();
        moves.push(Move {
            key: source.key.clone(),
            from,
            to,
            handle,
        });
    }
    Ok(moves)
}

trait Writes {
    fn set_origin(&mut self, handle: &Handle, x: i32, y: i32) -> Result<(), DisplayError>;
    fn sync(&mut self);
}

struct XrandrWrites<'a> {
    xlib: &'a Xlib,
    xrandr: &'a Xrandr,
    dpy: *mut Display,
    resources: *mut xrr::XRRScreenResources,
}

impl Writes for XrandrWrites<'_> {
    fn set_origin(&mut self, handle: &Handle, x: i32, y: i32) -> Result<(), DisplayError> {
        // SAFETY: handle came from this connection's snapshot; its outputs Vec outlives the call.
        let status = unsafe {
            (self.xrandr.XRRSetCrtcConfig)(
                self.dpy,
                self.resources,
                handle.crtc,
                xlib::CurrentTime,
                x,
                y,
                handle.mode,
                handle.rotation,
                handle.outputs.as_ptr() as *mut RROutput,
                handle.outputs.len() as i32,
            )
        };
        if status == xrr::RRSetConfigSuccess {
            Ok(())
        } else {
            Err(DisplayError::Platform(format!("XRRSetCrtcConfig 返回 {status}")))
        }
    }

    fn sync(&mut self) {
        // SAFETY: dpy is the live connection used for the writes above.
        unsafe {
            (self.xlib.XSync)(self.dpy, 0);
        }
    }
}

fn commit(writer: &mut dyn Writes, moves: &[Move]) -> Result<(), DisplayError> {
    let mut applied: Vec<&Move> = Vec::new();
    for current in moves {
        if let Err(error) = writer.set_origin(&current.handle, current.to.0, current.to.1) {
            return match rollback(writer, &applied) {
                Ok(()) => Err(error),
                Err(rollback) => Err(rollback),
            };
        }
        applied.push(current);
    }
    writer.sync();
    Ok(())
}

fn rollback(writer: &mut dyn Writes, moves: &[&Move]) -> Result<(), DisplayError> {
    let mut failure = None;
    for current in moves.iter().rev() {
        if let Err(error) = writer.set_origin(&current.handle, current.from.0, current.from.1) {
            failure.get_or_insert(error);
        }
    }
    writer.sync();
    failure.map_or(Ok(()), Err)
}

#[cfg(test)]
#[path = "arrangement_tests.rs"]
mod tests;
