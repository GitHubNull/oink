//! XI2 事件分派：cooked 事件来自抓取（主连接），raw 事件来自独立观察连接。

use std::time::Instant;

use x11_dl::xinput2::{
    XI_HierarchyChanged, XI_KeyPress, XI_KeyRelease, XI_RawButtonPress, XI_RawButtonRelease, XI_RawKeyPress,
    XI_RawKeyRelease, XI_RawMotion, XIDeviceEvent, XIHierarchyEvent, XIRawEvent,
};
use x11_dl::xlib::{Display, GenericEvent, XEvent};

use crate::capture_loop;
use crate::linux::devices;

use super::LoopState;

impl LoopState {
    #[allow(non_upper_case_globals)]
    pub(super) fn dispatch(&mut self, dpy: *mut Display) {
        // SAFETY: XEvent 是可零初始化的 C union 存储，XNextEvent 会写满。
        let mut event: XEvent = unsafe { std::mem::zeroed() };
        // SAFETY: dpy 有效；event 是足够大的 XEvent 存储；只有 XPending>0 才调用。
        unsafe { (self.xlib.XNextEvent)(dpy, &mut event) };
        // SAFETY: type_ 是 union 的公共首字段。
        if unsafe { event.type_ } != GenericEvent {
            return;
        }
        // SAFETY: 上面已确认 type_ == GenericEvent，cookie 字段有效。
        let cookie = unsafe { &mut event.generic_event_cookie };
        if cookie.extension != self.opcode {
            return;
        }
        // SAFETY: cookie 来自 XNextEvent；成功后 data 指向扩展事件结构。
        if unsafe { (self.xlib.XGetEventData)(dpy, cookie) } == 0 {
            return;
        }
        let now = Instant::now();
        match cookie.evtype {
            XI_KeyPress | XI_KeyRelease => {
                // SAFETY: 抓取送来的 cooked 事件的 data 指向 XIDeviceEvent。
                let key = unsafe { &*(cookie.data as *const XIDeviceEvent) };
                // Remote 或抓取撤销期间也走这里：handle_key 会回放被冻结的事件。
                self.modes.handle_key(now, key, &mut self.translator);
            }
            XI_RawKeyPress | XI_RawKeyRelease => {
                // SAFETY: raw 事件的 data 指向 XIRawEvent。
                let raw = unsafe { &*(cookie.data as *const XIRawEvent) };
                if self.active
                    && !self.modes.suppresses(raw.detail)
                    && let Some(event) = self.translator.key(now, raw)
                {
                    let _ = capture_loop::event(&event);
                }
            }
            XI_RawButtonPress | XI_RawButtonRelease => {
                // SAFETY: 同上。
                let raw = unsafe { &*(cookie.data as *const XIRawEvent) };
                if self.active
                    && let Some(event) = self.translator.button(now, raw)
                {
                    let _ = capture_loop::event(&event);
                }
            }
            XI_RawMotion => {
                // SAFETY: 同上。
                let raw = unsafe { &*(cookie.data as *const XIRawEvent) };
                if self.active
                    && let Some(event) = self.translator.motion(now, raw)
                {
                    let _ = capture_loop::event(&event);
                }
            }
            XI_HierarchyChanged => {
                // SAFETY: hierarchy 事件的 data 指向 XIHierarchyEvent。
                let change = unsafe { &*(cookie.data as *const XIHierarchyEvent) };
                tracing::info!(flags = change.flags, "XI2 设备层级变化，重新枚举");
                if let Ok(devices) = self.modes.query_devices() {
                    self.translator.replace_devices(devices.clone());
                    if let Ok(modmap) = devices::ModifierMap::query(&self.xlib, self.dpy) {
                        self.modes.replace_devices(devices, modmap, &mut self.translator);
                    }
                }
                self.next_sync = Instant::now();
            }
            _ => {}
        }
        // SAFETY: 与 XGetEventData 成对释放同一 cookie。
        unsafe { (self.xlib.XFreeEventData)(dpy, cookie) };
    }
}
