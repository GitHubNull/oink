//! 截获线程上的模式状态机：Remote 主动抓取/光标遮罩、被动热键抓取与原子恢复。
//!
//! 所有 X 请求都在截获线程自己的连接上执行；其他线程只改共享愿望值并写唤醒
//! 管道，绝不在回调里同步等待。

use std::sync::Arc;
use std::time::{Duration, Instant};

use oink_foundation::Point;
use x11_dl::xinput2::{XIDeviceEvent, XInput2};
use x11_dl::xlib::{Display, Xlib};

use crate::InputError;
use crate::capture_loop::{KeyChord, ResumeHotkey};
use crate::linux::{cursor, devices, hotkeys};

use super::super::event::Translator;

const REMOTE_RETRY: Duration = Duration::from_secs(1);
const VERIFY_TOLERANCE: f64 = 1.0;

pub(super) struct Context {
    /// 可路由本机输入（未暂停、未锁定、owner 仍请求截获）。
    pub(super) active: bool,
    /// 已暂停；仅保留恢复组合。
    pub(super) paused: bool,
    /// 锁屏状态已知且解锁；未知一律为 false。
    pub(super) resume_allowed: bool,
    /// owner 当前希望 Remote（主动抓取）。
    pub(super) desired_remote: bool,
    pub(super) park: Point,
    pub(super) resume: Option<ResumeHotkey>,
    pub(super) hotkeys: Vec<KeyChord>,
    pub(super) guard: bool,
}

pub(super) struct Modes {
    xlib: Arc<Xlib>,
    xi: Arc<XInput2>,
    dpy: *mut Display,
    devices: devices::Devices,
    cursor: cursor::CursorMask,
    active: hotkeys::ActiveGrab,
    passive: hotkeys::Grabs,
    desired: hotkeys::Desired,
    remote: bool,
    retry_at: Option<Instant>,
}

impl Modes {
    pub(super) fn new(
        xlib: Arc<Xlib>,
        xi: Arc<XInput2>,
        dpy: *mut Display,
        devices: devices::Devices,
        modmap: devices::ModifierMap,
    ) -> Result<Self, InputError> {
        let cursor = cursor::CursorMask::new(Arc::clone(&xlib), dpy)?;
        let passive = hotkeys::Grabs::new(Arc::clone(&xlib), Arc::clone(&xi), dpy, devices.clone(), modmap);
        Ok(Self {
            xlib,
            xi,
            dpy,
            devices,
            cursor,
            active: hotkeys::ActiveGrab::new(),
            passive,
            desired: hotkeys::Desired::default(),
            remote: false,
            retry_at: None,
        })
    }

    pub(super) fn remote(&self) -> bool {
        self.remote
    }

    /// 抓取过的键码，其 raw 事件不得再路由。
    pub(super) fn suppresses(&self, keycode: i32) -> bool {
        self.passive.suppresses(keycode)
    }

    pub(super) fn query_devices(&self) -> Result<devices::Devices, InputError> {
        devices::query(&self.xi, self.dpy)
    }

    pub(super) fn replace_devices(
        &mut self,
        devices: devices::Devices,
        modmap: devices::ModifierMap,
        translator: &mut Translator,
    ) {
        let previous = std::mem::replace(&mut self.devices, devices);
        self.active.release(&self.xlib, &self.xi, self.dpy, &previous);
        if self.remote {
            self.cursor.show();
            translator.rebase_cursor();
            self.remote = false;
        }
        self.passive.replace_devices(self.devices.clone(), modmap);
        self.retry_at = None;
    }

    pub(super) fn apply(&mut self, context: &Context, translator: &mut Translator, now: Instant) {
        if self.remote {
            if !context.active || !context.desired_remote {
                self.disengage(translator);
            }
        } else if context.active && context.desired_remote && self.retry_at.is_none_or(|retry| now >= retry) {
            if let Err(error) = self.engage(context.park, translator) {
                tracing::warn!(%error, "进入 Remote 主动抓取失败，保持 Local");
                self.retry_at = Some(now + REMOTE_RETRY);
            } else {
                self.retry_at = None;
            }
        }
        self.desired = hotkeys::Desired {
            enabled: if context.paused {
                context.resume_allowed
            } else {
                context.active
            },
            paused: context.paused,
            resume: context.resume,
            guard: context.guard,
        };
        self.passive.apply(&self.desired, &context.hotkeys);
    }

    fn engage(&mut self, park: Point, translator: &mut Translator) -> Result<(), InputError> {
        self.cursor.hide()?;
        if let Err(error) = self.active.engage(&self.xlib, &self.xi, self.dpy, &self.devices) {
            self.cursor.show();
            translator.rebase_cursor();
            return Err(error);
        }
        self.passive.withdraw();
        self.cursor.park(park);
        translator.reset_cursor(Some(park));
        self.remote = true;
        Ok(())
    }

    fn disengage(&mut self, translator: &mut Translator) {
        self.active.release(&self.xlib, &self.xi, self.dpy, &self.devices);
        self.passive.withdraw();
        self.cursor.show();
        translator.rebase_cursor();
        self.remote = false;
    }

    /// 处理被动抓取送来的 cooked 键事件。返回 true 表示已同步吞掉。
    pub(super) fn handle_key(&mut self, now: Instant, event: &XIDeviceEvent, translator: &mut Translator) -> bool {
        if self.remote || !self.desired.enabled {
            // 状态刚切换时可能仍有上一轮抓取的冻结事件：必须显式回放，不能留在服务端。
            self.passive.replay(event);
            return false;
        }
        self.passive.handle(now, event, translator, &self.desired)
    }

    /// 隐藏时停靠 → 切到 Local 基线 → 撤遮罩/显示 → XSync → 重读核验。
    pub(super) fn force_local(&mut self, at: Point, translator: &mut Translator) -> bool {
        self.active.release(&self.xlib, &self.xi, self.dpy, &self.devices);
        self.passive.withdraw();
        if self.cursor.hidden() {
            self.cursor.park(at);
        }
        self.cursor.warp(at);
        translator.reset_cursor(Some(at));
        self.cursor.show();
        // SAFETY: dpy 有效且由截获线程独占。
        unsafe { (self.xlib.XSync)(self.dpy, 0) };
        let observed = self.cursor.position();
        let placed = observed.is_some_and(|point| {
            (point.x - at.x).abs() <= VERIFY_TOLERANCE && (point.y - at.y).abs() <= VERIFY_TOLERANCE
        });
        let restored = placed && !self.cursor.hidden() && !self.active.engaged();
        if !restored {
            tracing::warn!(
                observed = ?observed,
                hidden = self.cursor.hidden(),
                grabbed = self.active.engaged(),
                "恢复本地光标未通过核验"
            );
        }
        self.remote = false;
        restored
    }

    /// 停止/崩溃清理：只撤本应用的抓取与遮罩，幂等。
    pub(super) fn shutdown(&mut self) {
        self.active.release(&self.xlib, &self.xi, self.dpy, &self.devices);
        self.passive.withdraw();
        self.cursor.show();
        self.remote = false;
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(super) fn registration_count(&self) -> usize {
        self.passive.count()
    }
}
