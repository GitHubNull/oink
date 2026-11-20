//! HID 用法与 X11 键码/按钮编号的映射。

use oink_foundation::input::{KeyCode, MouseButton};

pub(super) fn x_button(button: MouseButton) -> Option<u32> {
    Some(match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 3,
        MouseButton::Back => 8,
        MouseButton::Forward => 9,
        MouseButton::Other(_) => return None,
    })
}

/// HID 用法 → X11 键码。现代 X server 的 evdev 键码表固定为「evdev code + 8」，
/// 与键盘布局无关；布局只影响键符解释，不影响扫描码。
pub(super) fn x_keycode(code: KeyCode) -> Option<u32> {
    if code.page() != KeyCode::PAGE_KEYBOARD {
        return None;
    }
    Some(u32::from(evdev_code(code.usage())?) + 8)
}

fn evdev_code(usage: u16) -> Option<u16> {
    // HID 0x04..=0x1D 按字母顺序 a..z；evdev 顺序不连续，用查表。
    const LETTERS: [u16; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,
    ];
    match usage {
        0x04..=0x1D => Some(LETTERS[usize::from(usage - 0x04)]),
        0x1E..=0x27 => Some(2 + usage - 0x1E),
        0x28 => Some(28),
        0x29 => Some(1),
        0x2A => Some(14),
        0x2B => Some(15),
        0x2C => Some(57),
        0x2D => Some(12),
        0x2E => Some(13),
        0x2F => Some(26),
        0x30 => Some(27),
        0x31 => Some(43),
        0x32 => Some(43),
        0x33 => Some(39),
        0x34 => Some(40),
        0x35 => Some(41),
        0x36 => Some(51),
        0x37 => Some(52),
        0x38 => Some(53),
        0x39 => Some(58),
        0x3A..=0x43 => Some(59 + usage - 0x3A),
        0x44 => Some(87),
        0x45 => Some(88),
        0x46..=0x52 => Some(match usage {
            0x46 => 99,
            0x47 => 70,
            0x48 => 119,
            0x49 => 110,
            0x4A => 102,
            0x4B => 104,
            0x4C => 111,
            0x4D => 107,
            0x4E => 109,
            0x4F => 106,
            0x50 => 105,
            0x51 => 108,
            0x52 => 103,
            _ => return None,
        }),
        0x53..=0x63 => Some(match usage {
            0x53 => 69,
            0x54 => 98,
            0x55 => 55,
            0x56 => 74,
            0x57 => 78,
            0x58 => 96,
            0x59 => 79,
            0x5A => 80,
            0x5B => 81,
            0x5C => 75,
            0x5D => 76,
            0x5E => 77,
            0x5F => 71,
            0x60 => 72,
            0x61 => 73,
            0x62 => 82,
            0x63 => 83,
            _ => return None,
        }),
        0x64 => Some(86),
        0x67 => Some(117),
        0x68..=0x73 => Some(183 + usage - 0x68),
        0xE0 => Some(29),
        0xE1 => Some(42),
        0xE2 => Some(56),
        0xE3 => Some(125),
        0xE4 => Some(97),
        0xE5 => Some(54),
        0xE6 => Some(100),
        0xE7 => Some(126),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_keyboard_usages_map_to_evdev_x_keycodes() {
        assert_eq!(x_keycode(KeyCode::keyboard(0x04)), Some(38)); // a = evdev 30
        assert_eq!(x_keycode(KeyCode::keyboard(0x1E)), Some(10)); // 1 = evdev 2
        assert_eq!(x_keycode(KeyCode::keyboard(0x28)), Some(36)); // Enter = evdev 28
        assert_eq!(x_keycode(KeyCode::keyboard(0x44)), Some(95)); // F11 = evdev 87
        assert_eq!(x_keycode(KeyCode::LEFT_CTRL), Some(37)); // evdev 29
        assert_eq!(x_keycode(KeyCode::keyboard(0x99)), None);
        assert_eq!(x_keycode(KeyCode::new(KeyCode::PAGE_CONSUMER, 0xE9)), None);
    }
}
