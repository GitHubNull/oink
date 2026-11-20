//! XTEST 注入。所有注入只作用于本进程连接的 X server；按下台账记录本应用
//! 已按下但尚未释放的键/按钮，停止或结束注入会话时只释放台账里的项。

use std::collections::BTreeSet;
use std::os::raw::{c_int, c_ulong};

use oink_foundation::Point;
use oink_foundation::input::{InjectEvent, KeyCode, ScrollUnit};
use x11_dl::xlib::Display;

use super::X11Input;
use super::keymap::{x_button, x_keycode};
use crate::InputError;

/// X11 只有整格滚轮；像素滚动按常见桌面约 100 px/格折算，余数累积。
pub(super) const PIXELS_PER_NOTCH: f64 = 100.0;

/// 本应用经 XTEST 按下、尚未确认释放的键码与按钮号。
#[derive(Default)]
pub(crate) struct Ledger {
    keys: BTreeSet<u32>,
    buttons: BTreeSet<u32>,
}

impl Ledger {
    fn clear(&mut self) {
        self.keys.clear();
        self.buttons.clear();
    }

    fn key(&mut self, keycode: u32, pressed: bool) {
        if pressed {
            self.keys.insert(keycode);
        } else {
            self.keys.remove(&keycode);
        }
    }

    fn button(&mut self, button: u32, pressed: bool) {
        if pressed {
            self.buttons.insert(button);
        } else {
            self.buttons.remove(&button);
        }
    }
}

pub(super) fn inject(input: &X11Input, event: &InjectEvent) -> Result<(), InputError> {
    match event {
        InjectEvent::MoveTo { pos } | InjectEvent::MoveToWithDelta { pos, .. } => synth_motion_to(input, *pos),
        InjectEvent::MoveRelative { dx, dy } => synth_relative(input, *dx, *dy),
        InjectEvent::Button { button, pressed } => {
            let button = x_button(*button).ok_or(InputError::Unsupported)?;
            synth_button(input, button, *pressed)
        }
        InjectEvent::Scroll { dx, dy, unit, .. } => synth_scroll(input, *dx, *dy, *unit),
        InjectEvent::Key { code, pressed } | InjectEvent::KeyWithoutLock { code, pressed } => {
            synth_key(input, *code, *pressed)
        }
        InjectEvent::TouchpadScroll { wheel, .. } => synth_scroll(input, wheel.dx, wheel.dy, wheel.unit),
        InjectEvent::Gesture(_) | InjectEvent::TouchpadContacts(_) | InjectEvent::Volume { .. } => {
            Err(InputError::Unsupported)
        }
    }
}

/// 新的注入会话从空台账开始；上一会话的残留由 `stop_injection_session` 释放。
pub(super) fn start_session(input: &X11Input) {
    if let Ok(mut ledger) = input.ledger.lock() {
        ledger.clear();
    }
}

/// 只释放本应用台账中仍按下的键/按钮，然后清空台账。
pub(super) fn release_all(input: &X11Input) {
    let (keys, buttons) = match input.ledger.lock() {
        Ok(mut ledger) => (std::mem::take(&mut ledger.keys), std::mem::take(&mut ledger.buttons)),
        Err(_) => return,
    };
    for keycode in keys {
        let _ = synth_keycode(input, keycode, false);
    }
    for button in buttons {
        let _ = synth_button(input, button, false);
    }
}

fn record_key(input: &X11Input, keycode: u32, pressed: bool) {
    if let Ok(mut ledger) = input.ledger.lock() {
        ledger.key(keycode, pressed);
    }
}

fn record_button(input: &X11Input, button: u32, pressed: bool) {
    if let Ok(mut ledger) = input.ledger.lock() {
        ledger.button(button, pressed);
    }
}

fn synth_motion_to(input: &X11Input, pos: Point) -> Result<(), InputError> {
    let ok = input.with_dpy(|dpy| {
        // SAFETY: dpy 有效；screen=-1 表示默认屏，坐标为整数物理像素。
        unsafe { (input.xtest.XTestFakeMotionEvent)(dpy, -1, round(pos.x), round(pos.y), 0) }
    })?;
    input.flush()?;
    if ok == 0 {
        return Err(InputError::Platform("XTest 注入指针移动失败".into()));
    }
    Ok(())
}

type RelativeMotion = unsafe extern "C" fn(*mut Display, c_int, c_int, c_ulong) -> c_int;

/// x11-dl 把 libXtst 的 4 参数 `XTestFakeRelativeMotionEvent` 声明成了 5 参数；
/// 重新解释函数指针后按真实 ABI 调用（多出的参数是 delay=0）。
fn relative_motion(input: &X11Input) -> RelativeMotion {
    // SAFETY: 函数指针只做 ABI 签名修正；两者都是 extern "C" 且延迟参数为 0。
    unsafe {
        std::mem::transmute::<unsafe extern "C" fn(*mut Display, c_int, c_int, c_int, c_ulong) -> c_int, RelativeMotion>(
            input.xtest.XTestFakeRelativeMotionEvent,
        )
    }
}

fn synth_relative(input: &X11Input, dx: f64, dy: f64) -> Result<(), InputError> {
    let (dx, dy) = (round(dx), round(dy));
    if dx == 0 && dy == 0 {
        return Ok(());
    }
    let relative = relative_motion(input);
    let ok = input.with_dpy(|dpy| {
        // SAFETY: dpy 有效；dx/dy 为设备像素增量，delay=0 同步注入。
        unsafe { relative(dpy, dx, dy, 0) }
    })?;
    input.flush()?;
    if ok == 0 {
        return Err(InputError::Platform("XTest 注入相对位移失败".into()));
    }
    Ok(())
}

fn synth_button(input: &X11Input, button: u32, pressed: bool) -> Result<(), InputError> {
    let ok = input.with_dpy(|dpy| {
        // SAFETY: dpy 有效；XTEST 按钮号是 X11 核心按钮编号。
        unsafe { (input.xtest.XTestFakeButtonEvent)(dpy, button, c_int::from(pressed), 0) }
    })?;
    input.flush()?;
    if ok == 0 {
        return Err(InputError::Platform(format!("XTest 注入鼠标按钮 {button} 失败")));
    }
    record_button(input, button, pressed);
    Ok(())
}

fn synth_scroll(input: &X11Input, dx: f64, dy: f64, unit: ScrollUnit) -> Result<(), InputError> {
    let per_notch = match unit {
        ScrollUnit::Line => 1.0,
        ScrollUnit::Pixel => 1.0 / PIXELS_PER_NOTCH,
    };
    let (nx, ny) = {
        let mut acc = input
            .scroll
            .lock()
            .map_err(|_| InputError::Platform("滚动累积锁已损坏".into()))?;
        (
            take_whole(&mut acc.0, dx * per_notch),
            take_whole(&mut acc.1, dy * per_notch),
        )
    };
    // dy>0 表示视图向上 = 滚轮上（4）；dx>0 表示视图向左 = 滚轮左（6）。
    for (button, steps) in [(4, ny), (5, -ny), (6, nx), (7, -nx)] {
        for _ in 0..steps.max(0) {
            synth_button(input, button, true)?;
            synth_button(input, button, false)?;
        }
    }
    Ok(())
}

fn synth_key(input: &X11Input, code: KeyCode, pressed: bool) -> Result<(), InputError> {
    if matches!(code.page(), 0xff | 0xff_ac) {
        // 平台合成键（AppKit Fn、Windows 菜单遮罩）不是真实按键：其他目标忽略。
        return Ok(());
    }
    let Some(keycode) = x_keycode(code) else {
        return Err(InputError::Unsupported);
    };
    synth_keycode(input, keycode, pressed)
}

fn synth_keycode(input: &X11Input, keycode: u32, pressed: bool) -> Result<(), InputError> {
    let ok = input.with_dpy(|dpy| {
        // SAFETY: dpy 有效；keycode 是 X11 键码（evdev+8）。
        unsafe { (input.xtest.XTestFakeKeyEvent)(dpy, keycode, c_int::from(pressed), 0) }
    })?;
    input.flush()?;
    if ok == 0 {
        return Err(InputError::Platform(format!("XTest 注入键码 {keycode} 失败")));
    }
    record_key(input, keycode, pressed);
    Ok(())
}

fn take_whole(accumulator: &mut f64, value: f64) -> i32 {
    *accumulator += value;
    let whole = accumulator.trunc();
    *accumulator -= whole;
    whole as i32
}

fn round(value: f64) -> i32 {
    value.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::{Conn, X11Input};
    use std::sync::{Arc, Mutex};
    use x11_dl::xlib::Xlib;
    use x11_dl::xtest::Xf86vmode as Xtst;

    fn backend() -> Option<X11Input> {
        Some(X11Input {
            xlib: Xlib::open().ok()?,
            xtest: Xtst::open().ok()?,
            conn: Mutex::new(Conn {
                dpy: std::ptr::null_mut(),
            }),
            mode: Arc::new(Mutex::new(oink_foundation::input::CaptureMode::Local)),
            scroll: Mutex::new((0.0, 0.0)),
            ledger: Mutex::new(Ledger::default()),
            capture: Mutex::new(None),
            safety: Arc::new(crate::session_gate::SessionGate::default()),
            locked: Arc::new(std::sync::atomic::AtomicI8::new(0)),
        })
    }

    #[test]
    fn fractional_notches_accumulate_into_whole_wheel_steps() {
        let mut accumulator = 0.0;
        assert_eq!(take_whole(&mut accumulator, 0.4), 0);
        assert_eq!(take_whole(&mut accumulator, 0.4), 0);
        assert_eq!(take_whole(&mut accumulator, 0.4), 1);
        assert!((accumulator - 0.2).abs() < 1e-9);
    }

    #[test]
    fn ledger_tracks_presses_and_releases() {
        let mut ledger = Ledger::default();
        ledger.key(38, true);
        ledger.button(1, true);
        assert!(ledger.keys.contains(&38));
        assert!(ledger.buttons.contains(&1));
        ledger.key(38, false);
        ledger.button(1, false);
        assert!(ledger.keys.is_empty());
        assert!(ledger.buttons.is_empty());
        ledger.key(38, true);
        ledger.clear();
        assert!(ledger.keys.is_empty());
    }

    #[test]
    fn synthetic_platform_keys_are_ignored_and_volume_is_unsupported() {
        // 不需要 X 连接：这两条分支在访问连接之前返回。
        let Some(input) = backend() else {
            return;
        };
        assert!(synth_key(&input, KeyCode::APPLE_FN, true).is_ok());
        assert!(synth_key(&input, KeyCode::MENU_MASK, true).is_ok());
        assert!(matches!(
            synth_key(&input, KeyCode::new(KeyCode::PAGE_CONSUMER, 0xE9), true),
            Err(InputError::Unsupported)
        ));
        assert!(synth_key(&input, KeyCode::keyboard(0x04), true).is_err());
    }

    #[test]
    fn relative_motion_reinterprets_the_loaded_symbol() {
        let Some(input) = backend() else {
            return;
        };
        let _ = relative_motion(&input);
    }
}
