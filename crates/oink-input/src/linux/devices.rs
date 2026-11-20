//! XI2 设备枚举：master/XTEST 设备、滚动轴增量与修饰键映射。
//!
//! 滚动轴编号沿用 Xorg libinput 驱动的约定：valuator 2 = 水平、3 = 垂直，
//! 增量（每格滚动的 valuator 距离）通常为 120。X11 没有触摸板/鼠标的可靠区分，
//! 指针设备一律按鼠标上报（触摸支持仍为 BLOCKED）。

use std::collections::HashMap;
use std::ffi::CStr;

use x11_dl::xinput2::{
    XIAllDevices, XIAnyClassInfo, XIDeviceInfo, XIMasterKeyboard, XIMasterPointer, XIModeAbsolute, XIScrollClass,
    XIScrollClassInfo, XIScrollTypeHorizontal, XIScrollTypeVertical, XIValuatorClass, XIValuatorClassInfo, XInput2,
};
use x11_dl::xlib::{Display, Xlib};

use crate::InputError;

pub(super) const SCROLL_HORIZONTAL: usize = 2;
pub(super) const SCROLL_VERTICAL: usize = 3;
/// libinput 与 evdev 驱动的常规每格距离；仅在没有 scroll class 时兜底。
pub(super) const DEFAULT_SCROLL_INCREMENT: f64 = 120.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ScrollAxis {
    pub(super) increment: f64,
}

#[derive(Clone, Debug)]
pub(super) struct Devices {
    pub(super) master_pointer: i32,
    pub(super) master_keyboard: i32,
    xtest: Vec<i32>,
    scroll: HashMap<i32, [Option<ScrollAxis>; 2]>,
    absolute_pointer: HashMap<i32, bool>,
}

impl Devices {
    pub(super) fn is_xtest(&self, source: i32) -> bool {
        self.xtest.contains(&source)
    }

    /// `axis` 为 0 = 水平、1 = 垂直；设备没有该轴时返回 `None`。
    pub(super) fn scroll_axis(&self, source: i32, axis: usize) -> Option<ScrollAxis> {
        self.scroll.get(&source).and_then(|axes| axes[axis])
    }

    /// 指针 valuator 0 是绝对坐标（Xephyr 虚拟鼠标、绝对设备）还是原始增量（libinput 相对设备）。
    pub(super) fn absolute_pointer(&self, source: i32) -> bool {
        self.absolute_pointer.get(&source).copied().unwrap_or(false)
    }

    #[cfg(test)]
    pub(super) fn for_tests(
        master_pointer: i32,
        master_keyboard: i32,
        xtest: Vec<i32>,
        scroll: [Option<ScrollAxis>; 2],
        absolute: bool,
    ) -> Self {
        Self {
            master_pointer,
            master_keyboard,
            xtest,
            scroll: HashMap::from([(6, scroll)]),
            absolute_pointer: HashMap::from([(6, absolute)]),
        }
    }
}

pub(super) fn query(xi: &XInput2, dpy: *mut Display) -> Result<Devices, InputError> {
    let mut count = 0;
    // SAFETY: dpy 是有效的 XI2 连接；count 是有效 out 指针。
    let info = unsafe { (xi.XIQueryDevice)(dpy, XIAllDevices, &mut count) };
    if info.is_null() {
        return Err(InputError::Platform("XIQueryDevice 失败".into()));
    }
    let mut devices = Devices {
        master_pointer: -1,
        master_keyboard: -1,
        xtest: Vec::new(),
        scroll: HashMap::new(),
        absolute_pointer: HashMap::new(),
    };
    // SAFETY: info 指向 count 个 XIDeviceInfo，遍历结束后一次性释放。
    unsafe {
        for index in 0..count as usize {
            let device = &*info.add(index);
            if device.name.is_null() {
                continue;
            }
            let name = CStr::from_ptr(device.name).to_string_lossy();
            if device._use == XIMasterPointer && devices.master_pointer < 0 {
                devices.master_pointer = device.deviceid;
            }
            if device._use == XIMasterKeyboard && devices.master_keyboard < 0 {
                devices.master_keyboard = device.deviceid;
            }
            if name == "Virtual core XTEST pointer" || name == "Virtual core XTEST keyboard" {
                devices.xtest.push(device.deviceid);
            }
            if let Some(axes) = scroll_axes(device) {
                devices.scroll.insert(device.deviceid, axes);
            }
            if let Some(absolute) = pointer_mode(device) {
                devices.absolute_pointer.insert(device.deviceid, absolute);
            }
        }
        (xi.XIFreeDeviceInfo)(info);
    }
    if devices.master_pointer < 0 || devices.master_keyboard < 0 {
        return Err(InputError::Platform("缺少 XI2 master 设备".into()));
    }
    Ok(devices)
}

/// 调用者保证 `device.classes` 指向 `num_classes` 个有效 class 指针。
#[allow(non_upper_case_globals)]
fn scroll_axes(device: &XIDeviceInfo) -> Option<[Option<ScrollAxis>; 2]> {
    let mut axes = [None, None];
    let mut found = false;
    for index in 0..device.num_classes as usize {
        // SAFETY: 调用者保证 classes 数组长度为 num_classes。
        let class: *mut XIAnyClassInfo = unsafe { *device.classes.add(index) };
        if class.is_null() {
            continue;
        }
        // SAFETY: class 指针由服务器提供且非空。
        let scroll = unsafe { &*(class as *const XIScrollClassInfo) };
        if scroll._type != XIScrollClass {
            continue;
        }
        let increment = if scroll.increment > 0.0 {
            scroll.increment
        } else {
            DEFAULT_SCROLL_INCREMENT
        };
        let slot = match (scroll.scroll_type, scroll.number) {
            (XIScrollTypeHorizontal, _) => Some(0),
            (XIScrollTypeVertical, _) => Some(1),
            (_, x) if x == SCROLL_HORIZONTAL as i32 => Some(0),
            (_, x) if x == SCROLL_VERTICAL as i32 => Some(1),
            _ => None,
        };
        if let Some(slot) = slot {
            axes[slot] = Some(ScrollAxis { increment });
            found = true;
        }
    }
    found.then_some(axes)
}

/// 调用者保证 `device.classes` 指向 `num_classes` 个有效 class 指针。
fn pointer_mode(device: &XIDeviceInfo) -> Option<bool> {
    for index in 0..device.num_classes as usize {
        // SAFETY: 调用者保证 classes 数组长度为 num_classes。
        let class: *mut XIAnyClassInfo = unsafe { *device.classes.add(index) };
        if class.is_null() {
            continue;
        }
        // SAFETY: class 指针由服务器提供且非空。
        let valuator = unsafe { &*(class as *const XIValuatorClassInfo) };
        if valuator._type != XIValuatorClass {
            continue;
        }
        if valuator.number == 0 {
            return Some(valuator.mode == XIModeAbsolute);
        }
    }
    None
}

/// 核心修饰键索引 → X 键码。索引沿用 X 的约定（Shift=0, Lock=1, Control=2, Mod1..Mod5=3..7）。
/// 供 W3 的同步被动抓取展开 Lock/Mod2 组合。
#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub(super) struct ModifierMap {
    index_by_keycode: HashMap<u8, u8>,
}

#[allow(dead_code)]
impl ModifierMap {
    pub(super) fn query(xlib: &Xlib, dpy: *mut Display) -> Result<Self, InputError> {
        // SAFETY: dpy 有效；返回的映射由 XFreeModifiermap 释放。
        let map = unsafe { (xlib.XGetModifierMapping)(dpy) };
        if map.is_null() {
            return Err(InputError::Platform("XGetModifierMapping 失败".into()));
        }
        // SAFETY: map 非空且由 XGetModifierMapping 分配。
        let per_modifier = unsafe { (*map).max_keypermod }.max(0) as usize;
        // SAFETY: 同上；指针在 XFreeModifiermap 之前有效。
        let keycodes = unsafe { (*map).modifiermap };
        let mut pairs = Vec::new();
        if per_modifier > 0 && !keycodes.is_null() {
            for slot in 0..per_modifier * 8 {
                // SAFETY: XModifierKeymap 保证 max_keypermod * 8 个键码。
                let keycode = unsafe { *keycodes.add(slot) };
                if keycode != 0 {
                    pairs.push((keycode, (slot / per_modifier) as u8));
                }
            }
        }
        // SAFETY: map 由 XGetModifierMapping 返回，只能释放一次。
        unsafe { (xlib.XFreeModifiermap)(map) };
        Ok(Self::from_pairs(pairs))
    }

    fn from_pairs(pairs: Vec<(u8, u8)>) -> Self {
        Self {
            index_by_keycode: pairs.into_iter().collect(),
        }
    }

    pub(super) fn index(&self, keycode: u32) -> Option<u8> {
        u8::try_from(keycode)
            .ok()
            .and_then(|keycode| self.index_by_keycode.get(&keycode).copied())
    }

    /// 把和弦里的修饰键展开为 X 核心修饰位；未知键码忽略。
    pub(super) fn chord_mask(&self, chord: &crate::capture_loop::KeyChord) -> u32 {
        let mut mask = 0;
        let groups = [
            (chord.control, [crate::KeyCode::LEFT_CTRL, crate::KeyCode::RIGHT_CTRL]),
            (chord.alt, [crate::KeyCode::LEFT_ALT, crate::KeyCode::RIGHT_ALT]),
            (chord.shift, [crate::KeyCode::LEFT_SHIFT, crate::KeyCode::RIGHT_SHIFT]),
            (chord.meta, [crate::KeyCode::LEFT_GUI, crate::KeyCode::RIGHT_GUI]),
        ];
        for (enabled, keys) in groups {
            if !enabled {
                continue;
            }
            for key in keys {
                let Some(keycode) = super::keymap::x_keycode(key) else {
                    continue;
                };
                if let Some(index) = self.index(keycode) {
                    mask |= 1_u32 << index;
                }
            }
        }
        mask
    }
}

/// XI2 连接协商。必须在任何 XI2 请求之前调用。
pub(super) fn negotiate(xi: &XInput2, dpy: *mut Display) -> Result<(i32, i32), InputError> {
    let (mut major, mut minor) = (2, 2);
    // SAFETY: dpy 有效；major/minor 是有效 in/out 指针。
    let status = unsafe { (xi.XIQueryVersion)(dpy, &mut major, &mut minor) };
    if status != x11_dl::xlib::Success as i32 {
        return Err(InputError::Platform(format!("XI2 版本协商失败（状态 {status}）")));
    }
    Ok((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_axes_follow_libinput_numbering() {
        let mut devices = Devices {
            master_pointer: 2,
            master_keyboard: 3,
            xtest: vec![4, 5],
            scroll: HashMap::new(),
            absolute_pointer: HashMap::new(),
        };
        devices.scroll.insert(
            6,
            [
                Some(ScrollAxis { increment: 120.0 }),
                Some(ScrollAxis { increment: 120.0 }),
            ],
        );
        assert_eq!(devices.scroll_axis(6, 0).map(|axis| axis.increment), Some(120.0));
        assert_eq!(devices.scroll_axis(6, 1).map(|axis| axis.increment), Some(120.0));
        assert_eq!(devices.scroll_axis(7, 0), None);
        assert!(devices.is_xtest(5));
        assert!(!devices.is_xtest(7));
    }

    #[test]
    fn chord_mask_expands_only_enabled_modifiers() {
        let map = ModifierMap::from_pairs(vec![
            (37, 2),  // Control_L
            (105, 2), // Control_R
            (64, 3),  // Alt_L = Mod1
            (50, 0),  // Shift_L
            (133, 6), // Super_L = Mod4
        ]);
        let chord = crate::capture_loop::KeyChord {
            key: crate::KeyCode::keyboard(0x3A),
            control: true,
            alt: false,
            shift: true,
            meta: false,
        };
        assert_eq!(map.chord_mask(&chord), (1 << 2) | (1 << 0));
        let all = crate::capture_loop::KeyChord {
            key: crate::KeyCode::keyboard(0x3A),
            control: true,
            alt: true,
            shift: false,
            meta: true,
        };
        assert_eq!(map.chord_mask(&all), (1 << 2) | (1 << 3) | (1 << 6));
    }

    #[test]
    fn unknown_modifier_keycodes_are_ignored() {
        let map = ModifierMap::default();
        let chord = crate::capture_loop::KeyChord {
            key: crate::KeyCode::keyboard(0x3A),
            control: true,
            alt: true,
            shift: true,
            meta: true,
        };
        assert_eq!(map.chord_mask(&chord), 0);
    }
}
