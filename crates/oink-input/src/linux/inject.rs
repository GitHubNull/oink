//! XTEST 注入。阶段 3 的语义原样保留；阶段 4 将补直接 FFI 的相对位移与按下台账。

use std::os::raw::c_int;

use oink_foundation::Point;
use oink_foundation::input::{InjectEvent, KeyCode, ScrollUnit};

use super::X11Input;
use super::keymap::{x_button, x_keycode};
use crate::InputError;

/// X11 只有整格滚轮；像素滚动按常见桌面约 100 px/格折算，余数累积。
pub(super) const PIXELS_PER_NOTCH: f64 = 100.0;

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

/// XTestRelativeMotionEvent 的绑定签名与本机 libXtst 不一致，改用
/// 「查询当前位置 + 绝对移动」合成相对位移（阶段 3 的远端位移量很小）。
fn synth_relative(input: &X11Input, dx: f64, dy: f64) -> Result<(), InputError> {
    let (dx, dy) = (round(dx), round(dy));
    if dx == 0 && dy == 0 {
        return Ok(());
    }
    let (x, y) = input.pointer()?;
    synth_motion_to(
        input,
        Point {
            x: f64::from(x.saturating_add(dx)),
            y: f64::from(y.saturating_add(dy)),
        },
    )
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
    let ok = input.with_dpy(|dpy| {
        // SAFETY: dpy 有效；keycode 是 X11 键码（evdev+8）。
        unsafe { (input.xtest.XTestFakeKeyEvent)(dpy, keycode, c_int::from(pressed), 0) }
    })?;
    input.flush()?;
    if ok == 0 {
        return Err(InputError::Platform(format!("XTest 注入按键 {} 失败", code.0)));
    }
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
    use std::sync::Mutex;
    use x11_dl::xlib::Xlib;
    use x11_dl::xtest::Xf86vmode as Xtst;

    #[test]
    fn fractional_notches_accumulate_into_whole_wheel_steps() {
        let mut accumulator = 0.0;
        assert_eq!(take_whole(&mut accumulator, 0.4), 0);
        assert_eq!(take_whole(&mut accumulator, 0.4), 0);
        assert_eq!(take_whole(&mut accumulator, 0.4), 1);
        assert!((accumulator - 0.2).abs() < 1e-9);
    }

    #[test]
    fn synthetic_platform_keys_are_ignored_and_volume_is_unsupported() {
        // 不需要 X 连接：这两条分支在访问连接之前返回。
        let xlib = match Xlib::open() {
            Ok(xlib) => xlib,
            Err(_) => return,
        };
        let xtest = match Xtst::open() {
            Ok(xtest) => xtest,
            Err(_) => return,
        };
        let backend = X11Input {
            xlib,
            xtest,
            conn: Mutex::new(Conn {
                dpy: std::ptr::null_mut(),
            }),
            mode: Mutex::new(oink_foundation::input::CaptureMode::Local),
            scroll: Mutex::new((0.0, 0.0)),
            capture: Mutex::new(None),
        };
        assert!(synth_key(&backend, KeyCode::APPLE_FN, true).is_ok());
        assert!(synth_key(&backend, KeyCode::MENU_MASK, true).is_ok());
        assert!(matches!(
            synth_key(&backend, KeyCode::new(KeyCode::PAGE_CONSUMER, 0xE9), true),
            Err(InputError::Unsupported)
        ));
        assert!(synth_key(&backend, KeyCode::keyboard(0x04), true).is_err());
    }
}
