//! valuator 读取、滚轮回绕与按钮编号翻译。

use oink_foundation::input::MouseButton;
use x11_dl::xinput2::XIRawEvent;

/// 滚动 valuator 是双精度累加器；跨过半个回绕周期视为回绕，不当作真实跳变。
const WRAP: f64 = 4_294_967_296.0;
const HALF_WRAP: f64 = 2_147_483_648.0;

pub(super) fn x_button(detail: u32) -> Option<MouseButton> {
    Some(match detail {
        1 => MouseButton::Left,
        2 => MouseButton::Middle,
        3 => MouseButton::Right,
        8 => MouseButton::Back,
        9 => MouseButton::Forward,
        _ => MouseButton::Other(u8::try_from(detail.saturating_sub(1)).ok()?),
    })
}

/// 滚轮按钮 → foundation 滚动方向（4 = 上，5 = 下，6 = 左，7 = 右）。
pub(super) fn wheel_delta(detail: u32) -> Option<(f64, f64)> {
    Some(match detail {
        4 => (0.0, 1.0),
        5 => (0.0, -1.0),
        6 => (1.0, 0.0),
        7 => (-1.0, 0.0),
        _ => return None,
    })
}

pub(super) fn wrap_delta(value: f64, previous: f64) -> f64 {
    let delta = value - previous;
    if delta > HALF_WRAP {
        delta - WRAP
    } else if delta < -HALF_WRAP {
        delta + WRAP
    } else {
        delta
    }
}

/// 按 valuator 编号读取 raw 事件值。mask 的每一位对应一个 valuator 编号，
/// 值按编号升序排列。
pub(super) fn valuator(raw: &XIRawEvent, number: usize) -> Option<f64> {
    let state = &raw.valuators;
    if state.mask_len <= 0 || state.mask.is_null() || state.values.is_null() {
        return None;
    }
    let mut index = 0;
    for current in 0..state.mask_len as usize * 8 {
        // SAFETY: mask_len 字节由服务器提供；values 至少有同等数量的已设定位。
        let set = unsafe { *state.mask.add(current / 8) } & (1 << (current % 8)) != 0;
        if !set {
            continue;
        }
        if current == number {
            // SAFETY: index 只在已设定位上递增，不会超出 values 的长度。
            return Some(unsafe { *state.values.add(index) });
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use x11_dl::xinput2::XIValuatorState;

    fn raw(values: &mut [f64], mask: &mut [u8]) -> XIRawEvent {
        XIRawEvent {
            _type: 35,
            serial: 0,
            send_event: 0,
            display: std::ptr::null_mut(),
            extension: 131,
            evtype: 17,
            time: 0,
            deviceid: 6,
            sourceid: 6,
            detail: 0,
            flags: 0,
            valuators: XIValuatorState {
                mask_len: mask.len() as i32,
                mask: mask.as_mut_ptr(),
                values: values.as_mut_ptr(),
            },
            raw_values: std::ptr::null_mut(),
        }
    }

    #[test]
    fn valuator_reads_by_number_through_the_mask() {
        let mut mask = [0b0000_1101_u8];
        let mut values = [10.0, 240.0, 30.0];
        let event = raw(&mut values, &mut mask);
        assert_eq!(valuator(&event, 0), Some(10.0));
        assert_eq!(valuator(&event, 2), Some(240.0));
        assert_eq!(valuator(&event, 3), Some(30.0));
        assert_eq!(valuator(&event, 1), None);
    }

    #[test]
    fn wheel_buttons_map_to_foundation_directions() {
        assert_eq!(wheel_delta(4), Some((0.0, 1.0)));
        assert_eq!(wheel_delta(5), Some((0.0, -1.0)));
        assert_eq!(wheel_delta(6), Some((1.0, 0.0)));
        assert_eq!(wheel_delta(7), Some((-1.0, 0.0)));
        assert_eq!(wheel_delta(1), None);
    }

    #[test]
    fn scroll_wrap_is_not_a_huge_jump() {
        assert_eq!(wrap_delta(10.0, 5.0), 5.0);
        assert_eq!(wrap_delta(-WRAP / 2.0 + 60.0, WRAP / 2.0 - 60.0), 120.0);
        assert_eq!(wrap_delta(WRAP / 2.0 - 60.0, -WRAP / 2.0 + 60.0), -120.0);
    }
}
