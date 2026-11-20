//! XI2 raw 事件 → `CapturedEvent`。raw 事件只观察、不抢输入；XTEST 来源全部丢弃。

use std::sync::{Arc, Mutex};
use std::time::Instant;

use oink_foundation::Point;
use oink_foundation::input::{CaptureMode, CapturedEvent, InputEvent, ScrollPhase, ScrollUnit, SourceKind};
use values::{valuator, wheel_delta, wrap_delta, x_button};
use x11_dl::xinput2::{
    XI_KeyPress, XI_KeyRelease, XI_RawButtonPress, XI_RawButtonRelease, XI_RawKeyPress, XI_RawKeyRelease, XI_RawMotion,
    XIDeviceEvent, XIRawEvent,
};

use super::devices::Devices;
use super::keymap;

mod values;

/// 仅运行测试：Xephyr 里唯一可造的输入是 XTEST，允许它在端到端探针中穿过来源过滤。
/// 生产构建（非 cfg(test)）绝不含这条分支。
#[cfg(test)]
pub(super) static ACCEPT_XTEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 指针位置采样器（XQueryPointer）。测试用假采样器替代 X 连接。
pub(super) trait Sampler {
    fn cursor(&mut self) -> Option<Point>;
}

impl<F: FnMut() -> Option<Point>> Sampler for F {
    fn cursor(&mut self) -> Option<Point> {
        self()
    }
}

pub(super) struct Translator {
    devices: Devices,
    mode: Arc<Mutex<CaptureMode>>,
    sampler: Box<dyn Sampler>,
    epoch: Instant,
    cursor: Option<Point>,
    /// 绝对指针设备的 raw 位置基线；相对设备为 None。
    device_position: Option<Point>,
    pending: RawMotion,
    scroll: [Option<f64>; 2],
}

#[derive(Default)]
struct RawMotion {
    seen: bool,
    dx: f64,
    dy: f64,
}

impl Translator {
    pub(super) fn new(devices: Devices, mode: Arc<Mutex<CaptureMode>>, mut sampler: Box<dyn Sampler>) -> Self {
        let cursor = sampler.cursor();
        Self {
            devices,
            mode,
            sampler,
            epoch: Instant::now(),
            cursor,
            device_position: None,
            pending: RawMotion::default(),
            scroll: [None, None],
        }
    }

    pub(super) fn replace_devices(&mut self, devices: Devices) {
        self.devices = devices;
        self.device_position = None;
        self.scroll = [None, None];
    }

    /// 当前是否处于 Remote（光标停靠、事件不带本机位置）。
    fn remote(&self) -> bool {
        let mode = self.mode.lock().unwrap_or_else(|e| e.into_inner());
        matches!(*mode, CaptureMode::Remote { .. })
    }

    fn time_us(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.epoch).as_micros() as u64
    }

    pub(super) fn synthetic(&self, sourceid: i32) -> bool {
        #[cfg(test)]
        if ACCEPT_XTEST.load(std::sync::atomic::Ordering::Relaxed) {
            return sourceid <= 0;
        }
        sourceid <= 0 || self.devices.is_xtest(sourceid)
    }

    /// 键事件。`detail` 是 X11 键码（evdev + 8），translate 为 HID usage。
    #[allow(non_upper_case_globals)]
    pub(super) fn key(&mut self, now: Instant, raw: &XIRawEvent) -> Option<CapturedEvent> {
        if self.synthetic(raw.sourceid) {
            return None;
        }
        let pressed = match raw.evtype {
            XI_RawKeyPress => true,
            XI_RawKeyRelease => false,
            _ => return None,
        };
        let code = keymap::hid_usage(raw.detail as u32)?;
        Some(CapturedEvent {
            physical: true,
            event: InputEvent::Key { code, pressed },
            source: SourceKind::Keyboard,
            time_us: self.time_us(now),
            cursor: None,
        })
    }

    /// 按钮事件；滚轮按钮（4..=7）只在按下时折成一格滚动。
    #[allow(non_upper_case_globals)]
    pub(super) fn button(&mut self, now: Instant, raw: &XIRawEvent) -> Option<CapturedEvent> {
        if self.synthetic(raw.sourceid) {
            return None;
        }
        let pressed = match raw.evtype {
            XI_RawButtonPress => true,
            XI_RawButtonRelease => false,
            _ => return None,
        };
        let detail = raw.detail as u32;
        let cursor = self.cursor.filter(|_| !self.remote());
        if let Some((dx, dy)) = wheel_delta(detail) {
            if !pressed {
                return None;
            }
            return Some(CapturedEvent {
                physical: true,
                event: InputEvent::Scroll {
                    dx,
                    dy,
                    unit: ScrollUnit::Line,
                    phase: ScrollPhase::None,
                    wheel_steps: Some(Point::new(dx, dy)),
                },
                source: SourceKind::Mouse,
                time_us: self.time_us(now),
                cursor,
            });
        }
        let button = x_button(detail)?;
        Some(CapturedEvent {
            physical: true,
            event: InputEvent::Button { button, pressed },
            source: SourceKind::Mouse,
            time_us: self.time_us(now),
            cursor,
        })
    }

    /// 被动抓取送来的 cooked 键事件 → `CapturedEvent`。raw 事件已被抑制，
    /// 这条路径是该按键码唯一的本地路由来源。
    #[allow(non_upper_case_globals)]
    pub(super) fn grabbed_key(&self, now: Instant, event: &XIDeviceEvent) -> Option<CapturedEvent> {
        let pressed = match event.evtype {
            XI_KeyPress => true,
            XI_KeyRelease => false,
            _ => return None,
        };
        let code = keymap::hid_usage(event.detail as u32)?;
        Some(CapturedEvent {
            physical: true,
            event: InputEvent::Key { code, pressed },
            source: SourceKind::Keyboard,
            time_us: self.time_us(now),
            cursor: None,
        })
    }

    /// 停靠/恢复/停靠点跳跃之后重设位移基线，避免把 warp 或注入位移算进
    /// 下一次物理移动。
    pub(super) fn reset_cursor(&mut self, at: Option<Point>) {
        self.cursor = at;
        self.device_position = None;
        self.pending = RawMotion::default();
    }

    /// 用当前采样位置重建基线；采样失败时退回 `None`（下一次 flush 不产生位移）。
    pub(super) fn rebase_cursor(&mut self) {
        let sampled = self.sampler.cursor();
        self.reset_cursor(sampled);
        self.scroll = [None, None];
    }

    /// 暂停/锁屏期间丢弃尚未上报的 raw motion。
    pub(super) fn discard_motion(&mut self) {
        self.pending = RawMotion::default();
        self.device_position = None;
    }

    /// 记录 raw motion 并更新滚动状态；位置差与 raw 增量在 `flush_motion` 一次产生，
    /// 避免每个采样点都做一次 X 往返。滚动轴变化立即上报。
    pub(super) fn motion(&mut self, now: Instant, raw: &XIRawEvent) -> Option<CapturedEvent> {
        if self.synthetic(raw.sourceid) || raw.evtype != XI_RawMotion {
            return None;
        }
        let delta = if self.devices.absolute_pointer(raw.sourceid) {
            let position = Point::new(valuator(raw, 0).unwrap_or(0.0), valuator(raw, 1).unwrap_or(0.0));
            let delta = self.device_position.map_or(Point::new(0.0, 0.0), |previous| {
                Point::new(position.x - previous.x, position.y - previous.y)
            });
            self.device_position = Some(position);
            delta
        } else {
            Point::new(valuator(raw, 0).unwrap_or(0.0), valuator(raw, 1).unwrap_or(0.0))
        };
        self.pending.seen = true;
        self.pending.dx += delta.x;
        self.pending.dy += delta.y;
        self.motion_scroll(now, raw)
    }

    /// raw motion 里的滚动轴变化。libinput：2 = 水平、3 = 垂直，正值 = 向右/向下滚动。
    /// foundation 约定 dx>0 = 视图向左、dy>0 = 视图向上，因此取负。
    fn motion_scroll(&mut self, now: Instant, raw: &XIRawEvent) -> Option<CapturedEvent> {
        let mut steps = Point::new(0.0, 0.0);
        let mut changed = false;
        for (number, slot) in [(2_usize, 0_usize), (3_usize, 1_usize)] {
            let Some(profile) = self.devices.scroll_axis(raw.sourceid, slot) else {
                continue;
            };
            let Some(value) = valuator(raw, number) else {
                continue;
            };
            let Some(previous) = self.scroll[slot].replace(value) else {
                // 首个采样只建立基线，不把绝对累加值当成一次巨大滚动。
                continue;
            };
            let notches = wrap_delta(value, previous) / profile.increment;
            if notches.abs() <= 1e-6 {
                continue;
            }
            if slot == 0 {
                steps.x = notches;
            } else {
                steps.y = notches;
            }
            changed = true;
        }
        if !changed {
            return None;
        }
        let (dx, dy) = (-steps.x, -steps.y);
        Some(CapturedEvent {
            physical: true,
            event: InputEvent::Scroll {
                dx,
                dy,
                unit: ScrollUnit::Line,
                phase: ScrollPhase::None,
                wheel_steps: Some(Point::new(dx, dy)),
            },
            source: SourceKind::Mouse,
            time_us: self.time_us(now),
            cursor: self.cursor.filter(|_| !self.remote()),
        })
    }

    /// 把一批 raw motion 折成至多两个事件：加速后的本机位移与设备原始增量。
    /// Local 模式位移取本机光标位置差；Remote 模式光标已停靠，取设备原始增量。
    pub(super) fn flush_motion(&mut self, now: Instant) -> Vec<CapturedEvent> {
        if !self.pending.seen {
            return Vec::new();
        }
        let raw = std::mem::take(&mut self.pending);
        let remote = self.remote();
        let mut events = Vec::new();
        let cursor = self.sampler.cursor();
        let delta = match (remote, cursor, self.cursor) {
            (false, Some(now), Some(previous)) => Point::new(now.x - previous.x, now.y - previous.y),
            (false, _, _) => Point::new(0.0, 0.0),
            (true, _, _) => Point::new(raw.dx, raw.dy),
        };
        if let Some(now) = cursor {
            self.cursor = Some(now);
        }
        let time_us = self.time_us(now);
        if delta.x != 0.0 || delta.y != 0.0 {
            events.push(CapturedEvent {
                physical: true,
                event: InputEvent::PointerDelta {
                    dx: delta.x,
                    dy: delta.y,
                },
                source: SourceKind::Mouse,
                time_us,
                cursor: cursor.filter(|_| !remote),
            });
        }
        if raw.dx != 0.0 || raw.dy != 0.0 {
            events.push(CapturedEvent {
                physical: true,
                event: InputEvent::RawPointerDelta { dx: raw.dx, dy: raw.dy },
                source: SourceKind::Mouse,
                time_us,
                cursor: None,
            });
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::devices::ScrollAxis;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use x11_dl::xinput2::XIValuatorState;

    fn devices(xtest: Vec<i32>) -> Devices {
        Devices::for_tests(2, 3, xtest, [Some(ScrollAxis { increment: 120.0 }); 2], false)
    }

    fn raw(evtype: i32, sourceid: i32, detail: i32, values: &mut [f64], mask: &mut [u8]) -> XIRawEvent {
        XIRawEvent {
            _type: 35,
            serial: 0,
            send_event: 0,
            display: std::ptr::null_mut(),
            extension: 131,
            evtype,
            time: 0,
            deviceid: sourceid,
            sourceid,
            detail,
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
    fn xtest_and_synthetic_sources_are_dropped() {
        let mode = Arc::new(Mutex::new(CaptureMode::Local));
        let mut translator = Translator::new(devices(vec![4, 5]), mode, Box::new(|| None));
        let mut mask = [0_u8];
        let mut values = [0.0];
        let mut event = raw(XI_RawKeyPress, 5, 38, &mut values, &mut mask);
        assert!(translator.key(Instant::now(), &event).is_none());
        event.sourceid = 6;
        assert!(translator.key(Instant::now(), &event).is_some());
    }

    #[test]
    fn remote_motion_uses_device_deltas_and_hides_cursor() {
        let mode = Arc::new(Mutex::new(CaptureMode::Remote {
            park: Point::new(10.0, 20.0),
        }));
        let sampled = Arc::new(AtomicUsize::new(0));
        let samples = sampled.clone();
        let sampler = move || {
            samples.fetch_add(1, Ordering::Relaxed);
            Some(Point::new(100.0, 200.0))
        };
        let mut translator = Translator::new(devices(vec![]), mode, Box::new(sampler));
        let mut mask = [0b0000_0011_u8];
        let mut values = [7.0, -3.0];
        let event = raw(XI_RawMotion, 6, 0, &mut values, &mut mask);
        assert!(translator.motion(Instant::now(), &event).is_none());
        assert!(translator.motion(Instant::now(), &event).is_none());
        let events = translator.flush_motion(Instant::now());
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event, InputEvent::PointerDelta { dx: 14.0, dy: -6.0 });
        assert_eq!(events[0].cursor, None);
        assert_eq!(events[1].event, InputEvent::RawPointerDelta { dx: 14.0, dy: -6.0 });
        assert_eq!(sampled.load(Ordering::Relaxed), 2, "new + flush");
    }

    #[test]
    fn local_motion_prefers_cursor_position_delta() {
        let mode = Arc::new(Mutex::new(CaptureMode::Local));
        let position = Arc::new(Mutex::new(Point::new(50.0, 50.0)));
        let sample = position.clone();
        let sampler = move || Some(*sample.lock().unwrap());
        let mut translator = Translator::new(devices(vec![]), mode, Box::new(sampler));
        *position.lock().unwrap() = Point::new(54.0, 49.0);
        let mut mask = [0b0000_0011_u8];
        let mut values = [100.0, 100.0];
        let event = raw(XI_RawMotion, 6, 0, &mut values, &mut mask);
        assert!(translator.motion(Instant::now(), &event).is_none());
        let events = translator.flush_motion(Instant::now());
        assert_eq!(events[0].event, InputEvent::PointerDelta { dx: 4.0, dy: -1.0 });
        assert_eq!(events[0].cursor, Some(Point::new(54.0, 49.0)));
    }

    #[test]
    fn scroll_valuator_uses_increment_and_foundation_sign() {
        let mode = Arc::new(Mutex::new(CaptureMode::Local));
        let mut translator = Translator::new(devices(vec![]), mode, Box::new(|| None));
        let mut mask = [0b0000_1100_u8];
        let mut first = [0.0, 0.0];
        let event = raw(XI_RawMotion, 6, 0, &mut first, &mut mask);
        assert!(
            translator.motion(Instant::now(), &event).is_none(),
            "first sample is baseline"
        );
        let mut second = [0.0, 120.0];
        let event = raw(XI_RawMotion, 6, 0, &mut second, &mut mask);
        let scroll = translator.motion(Instant::now(), &event).expect("one notch");
        assert_eq!(
            scroll.event,
            InputEvent::Scroll {
                dx: 0.0,
                dy: -1.0,
                unit: ScrollUnit::Line,
                phase: ScrollPhase::None,
                wheel_steps: Some(Point::new(0.0, -1.0)),
            }
        );
    }
}
