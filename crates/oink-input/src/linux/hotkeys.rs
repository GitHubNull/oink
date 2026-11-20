//! 键盘抓取：同步被动键码抓取（热键/守卫/恢复组合）与 Remote 主动设备抓取。
//!
//! 被动抓取按「键码」注册，修饰键组合由路由层判断：命中热键时 owner 返回
//! `Swallow`，其余一律 `XIReplayDevice` 回放，因此普通组合不会被吞掉。抓取过
//! 的键码其 raw 事件必须抑制，避免同一按键被路由两次。

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use oink_foundation::input::Disposition;
use x11_dl::xinput2::{
    XI_KeyPress, XI_KeyRelease, XIAsyncDevice, XIDeviceEvent, XIEventMask, XIGrabModeAsync, XIGrabModifiers,
    XIReplayDevice, XInput2,
};
use x11_dl::xlib::{Display, Window, Xlib};

use super::devices::{Devices, ModifierMap};
use super::{event::Translator, keymap};
use crate::InputError;
use crate::capture_loop::{self, KeyChord, ResumeHotkey};

/// XIAnyModifier（XI2.h：`1U << 31`；核心协议的 AnyModifier 才是 `1 << 15`）。
const ANY_MODIFIER: i32 = i32::MIN;
/// 锁定位（CapsLock/NumLock）不参与热键修饰匹配。
const IGNORED_MOD_MODS: u32 = 0x02 | 0x10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Binding {
    keycode: i32,
    modifiers: i32,
}

/// 当前本地输入状态下期望的被动抓取集合。
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Desired {
    pub(super) enabled: bool,
    pub(super) paused: bool,
    pub(super) resume: Option<ResumeHotkey>,
    pub(super) guard: bool,
}

impl Desired {
    fn bindings(&self, hotkeys: &[KeyChord]) -> BTreeSet<Binding> {
        if !self.enabled {
            return BTreeSet::new();
        }
        if self.paused {
            return self
                .resume
                .and_then(|resume| keymap::x_keycode(resume.key))
                .map(|keycode| Binding {
                    keycode: keycode as i32,
                    modifiers: ANY_MODIFIER,
                })
                .into_iter()
                .collect();
        }
        if self.guard {
            return [Binding {
                keycode: 0,
                modifiers: ANY_MODIFIER,
            }]
            .into_iter()
            .collect();
        }
        hotkeys
            .iter()
            .filter_map(|chord| keymap::x_keycode(chord.key))
            .map(|keycode| Binding {
                keycode: keycode as i32,
                modifiers: ANY_MODIFIER,
            })
            .collect()
    }
}

pub(super) struct Grabs {
    xlib: Arc<Xlib>,
    xi: Arc<XInput2>,
    dpy: *mut Display,
    root: Window,
    devices: Devices,
    modmap: ModifierMap,
    registered: BTreeSet<Binding>,
    failed: BTreeSet<Binding>,
    pressed: BTreeSet<u32>,
}

impl Grabs {
    pub(super) fn new(
        xlib: Arc<Xlib>,
        xi: Arc<XInput2>,
        dpy: *mut Display,
        devices: Devices,
        modmap: ModifierMap,
    ) -> Self {
        // SAFETY: dpy 有效且由截获线程独占。
        let root = unsafe { (xlib.XDefaultRootWindow)(dpy) };
        Self {
            xlib,
            xi,
            dpy,
            root,
            devices,
            modmap,
            registered: BTreeSet::new(),
            failed: BTreeSet::new(),
            pressed: BTreeSet::new(),
        }
    }

    pub(super) fn replace_devices(&mut self, devices: Devices, modmap: ModifierMap) {
        self.devices = devices;
        self.modmap = modmap;
        // 设备重建后旧的抓取可能已失效；下一轮 sync 会按登记表重建。
        self.registered.clear();
        self.failed.clear();
        self.pressed.clear();
    }

    /// 抓取过（被 AnyModifier 接管）的键码，其 raw 事件不得再路由。
    pub(super) fn suppresses(&self, keycode: i32) -> bool {
        self.registered
            .iter()
            .any(|binding| binding.keycode == 0 || binding.keycode == keycode)
    }

    /// 与期望集合对齐；失败项登记后不反复重试，直到期望集合变化。
    pub(super) fn apply(&mut self, desired: &Desired, hotkeys: &[KeyChord]) {
        let target = desired.bindings(hotkeys);
        let settled: BTreeSet<Binding> = self.registered.union(&self.failed).copied().collect();
        if target == settled {
            return;
        }
        self.withdraw();
        for binding in &target {
            if self.grab(*binding) {
                self.registered.insert(*binding);
            } else {
                self.failed.insert(*binding);
            }
        }
    }

    fn grab(&mut self, binding: Binding) -> bool {
        let mut mask_bytes = [0_u8; 4];
        for kind in [XI_KeyPress, XI_KeyRelease] {
            mask_bytes[kind as usize / 8] |= 1 << (kind as usize % 8);
        }
        let mut mask = XIEventMask {
            deviceid: self.devices.master_keyboard,
            mask_len: mask_bytes.len() as i32,
            mask: mask_bytes.as_mut_ptr(),
        };
        let mut modifiers = [XIGrabModifiers {
            modifiers: binding.modifiers,
            status: 0,
        }];
        // SAFETY: dpy 有效；mask 指向存活的掩码；modifiers 是单项 in/out 数组。
        let status = unsafe {
            (self.xi.XIGrabKeycode)(
                self.dpy,
                self.devices.master_keyboard,
                binding.keycode,
                self.root,
                x11_dl::xinput2::XIGrabModeSync,
                XIGrabModeAsync,
                0,
                &mut mask,
                1,
                modifiers.as_mut_ptr(),
            )
        };
        if status != 0 || modifiers[0].status != 0 {
            tracing::warn!(
                keycode = binding.keycode,
                modifiers = binding.modifiers,
                status,
                per_modifier = modifiers[0].status,
                "XI2 被动键码抓取失败"
            );
            return false;
        }
        true
    }

    fn ungrab(&mut self, binding: Binding) {
        let mut modifiers = [XIGrabModifiers {
            modifiers: binding.modifiers,
            status: 0,
        }];
        // SAFETY: dpy 有效；modifiers 是单项 in/out 数组。
        unsafe {
            (self.xi.XIUngrabKeycode)(
                self.dpy,
                self.devices.master_keyboard,
                binding.keycode,
                self.root,
                1,
                modifiers.as_mut_ptr(),
            )
        };
    }

    pub(super) fn withdraw(&mut self) {
        for binding in std::mem::take(&mut self.registered) {
            self.ungrab(binding);
        }
        self.failed.clear();
        self.pressed.clear();
        // SAFETY: dpy 有效；冲刷撤销请求。
        unsafe { (self.xlib.XFlush)(self.dpy) };
    }

    fn allow(&self, event: &XIDeviceEvent, mode: i32) {
        // SAFETY: dpy 有效；设备号来自同一服务器的 cooked 事件。
        unsafe { (self.xi.XIAllowEvents)(self.dpy, event.deviceid, mode, 0) };
    }

    /// 抓取已撤销但事件仍被冻结时，无条件回放给本地窗口。
    pub(super) fn replay(&self, event: &XIDeviceEvent) {
        if event.evtype == XI_KeyPress || event.evtype == XI_KeyRelease {
            self.allow(event, XIReplayDevice);
        }
    }

    /// 处理被动抓取送来的 cooked 键事件。返回 true 表示已消费（异步吞掉）。
    pub(super) fn handle(
        &mut self,
        now: Instant,
        event: &XIDeviceEvent,
        translator: &mut Translator,
        desired: &Desired,
    ) -> bool {
        if event.evtype != XI_KeyPress && event.evtype != XI_KeyRelease {
            return false;
        }
        if translator.synthetic(event.sourceid) {
            self.allow(event, XIReplayDevice);
            return false;
        }
        let pressed = event.evtype == XI_KeyPress;
        let keycode = event.detail as u32;
        if desired.paused {
            if !desired.resume.is_some_and(|resume| self.is_resume(resume, event)) {
                self.allow(event, XIReplayDevice);
                return false;
            }
            if pressed {
                capture_loop::resume_hotkey();
                self.pressed.insert(keycode);
            } else {
                self.pressed.remove(&keycode);
            }
            self.allow(event, XIAsyncDevice);
            return true;
        }
        if !pressed && self.pressed.remove(&keycode) {
            self.allow(event, XIAsyncDevice);
            return true;
        }
        let Some(translated) = translator.grabbed_key(now, event) else {
            self.allow(event, XIReplayDevice);
            return false;
        };
        if capture_loop::event(&translated) == Disposition::Swallow {
            if pressed {
                self.pressed.insert(keycode);
            }
            self.allow(event, XIAsyncDevice);
            true
        } else {
            self.allow(event, XIReplayDevice);
            false
        }
    }

    fn is_resume(&self, resume: ResumeHotkey, event: &XIDeviceEvent) -> bool {
        if keymap::x_keycode(resume.key) != Some(event.detail as u32) {
            return false;
        }
        let chord = KeyChord {
            key: resume.key,
            control: resume.control,
            alt: resume.alt,
            shift: resume.shift,
            meta: resume.meta,
        };
        let want = self.modmap.chord_mask(&chord) & !IGNORED_MOD_MODS;
        let got = event.mods.effective as u32 & !IGNORED_MOD_MODS;
        want == got
    }
}

impl Grabs {
    /// 供测试/诊断：当前成功登记的键码数。
    #[cfg(test)]
    #[allow(dead_code)]
    pub(super) fn count(&self) -> usize {
        self.registered.len()
    }
}

/// Remote 模式的主动设备抓取：真正阻止本地应用收到指针/键盘事件。
pub(super) struct ActiveGrab {
    pointer: bool,
    keyboard: bool,
}

impl ActiveGrab {
    pub(super) fn new() -> Self {
        Self {
            pointer: false,
            keyboard: false,
        }
    }

    pub(super) fn engaged(&self) -> bool {
        self.pointer || self.keyboard
    }

    pub(super) fn engage(
        &mut self,
        xlib: &Xlib,
        xi: &XInput2,
        dpy: *mut Display,
        devices: &Devices,
    ) -> Result<(), InputError> {
        if self.engaged() {
            return Ok(());
        }
        // SAFETY: dpy 有效且由截获线程独占。
        let root = unsafe { (xlib.XDefaultRootWindow)(dpy) };
        // 事件掩码包含按键/按钮/运动；raw 事件仍独立路由，cooked 事件在 dispatch
        // 中被忽略（主动抓取期间不重复送 owner）。
        let mut mask_bytes = [0_u8; 4];
        for kind in [XI_KeyPress, XI_KeyRelease, 4, 5, 6] {
            mask_bytes[kind as usize / 8] |= 1 << (kind as usize % 8);
        }
        let mut mask = XIEventMask {
            deviceid: devices.master_pointer,
            mask_len: mask_bytes.len() as i32,
            mask: mask_bytes.as_mut_ptr(),
        };
        // SAFETY: dpy 有效；mask 在调用期间存活；time=CurrentTime、cursor=None。
        let pointer = unsafe {
            (xi.XIGrabDevice)(
                dpy,
                devices.master_pointer,
                root,
                0,
                0,
                XIGrabModeAsync,
                XIGrabModeAsync,
                0,
                &mut mask,
            )
        };
        if pointer != 0 {
            return Err(InputError::Platform(format!("XI2 主动抓取指针失败（状态 {pointer}）")));
        }
        self.pointer = true;
        mask.deviceid = devices.master_keyboard;
        // SAFETY: 同上；键盘抓取失败时立即释放已拿到的指针抓取。
        let keyboard = unsafe {
            (xi.XIGrabDevice)(
                dpy,
                devices.master_keyboard,
                root,
                0,
                0,
                XIGrabModeAsync,
                XIGrabModeAsync,
                0,
                &mut mask,
            )
        };
        if keyboard != 0 {
            // SAFETY: 指针抓取由本结构持有且只释放一次。
            unsafe { (xi.XIUngrabDevice)(dpy, devices.master_pointer, 0) };
            self.pointer = false;
            return Err(InputError::Platform(format!("XI2 主动抓取键盘失败（状态 {keyboard}）")));
        }
        self.keyboard = true;
        // SAFETY: dpy 有效；立即冲刷，保证本地交付在返回前已停止。
        unsafe { (xlib.XFlush)(dpy) };
        Ok(())
    }

    pub(super) fn release(&mut self, xlib: &Xlib, xi: &XInput2, dpy: *mut Display, devices: &Devices) {
        if self.keyboard {
            // SAFETY: 键盘抓取由本结构持有且只释放一次。
            unsafe { (xi.XIUngrabDevice)(dpy, devices.master_keyboard, 0) };
            self.keyboard = false;
        }
        if self.pointer {
            // SAFETY: 指针抓取由本结构持有且只释放一次。
            unsafe { (xi.XIUngrabDevice)(dpy, devices.master_pointer, 0) };
            self.pointer = false;
        }
        // SAFETY: dpy 有效。
        unsafe { (xlib.XFlush)(dpy) };
    }
}
