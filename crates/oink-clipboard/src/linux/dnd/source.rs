//! Source side of XDND: our own drag, driven by an injected left press. The
//! thread polls the pointer (button mask included), discovers the XdndAware
//! window under it, sends Enter/Position and finishes with Drop/Finished.
use super::window::{self, Xdnd};
use crate::dnd::DragOperation;
use crate::linux::x11::Conn;
use std::time::{Duration, Instant};

pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(40);
pub(super) const POSITION_INTERVAL: Duration = Duration::from_millis(120);
/// A target that never answers Finished cannot hold the session hostage.
pub(super) const FINISH_TIMEOUT: Duration = Duration::from_secs(2);

/// The target our drag is currently talking to.
pub(super) struct Current {
    pub(super) window: u64,
}

/// One live drag that this machine started for remote content.
pub(super) struct Own {
    pub(super) operation: DragOperation,
    pub(super) source: u64,
    types: Vec<u64>,
    more: bool,
    pub(super) target: Option<Current>,
    pub(super) pressed: bool,
    pub(super) accepted: bool,
    pub(super) wants_position: bool,
    pub(super) last_position: Instant,
    pub(super) last_point: (i32, i32),
    /// Set once the button release submits the drop; waits for Finished.
    pub(super) awaiting_finished: Option<Instant>,
}

impl Own {
    pub(super) fn new(operation: DragOperation, source: u64, types: Vec<u64>) -> Self {
        let more = types.len() > window::MAX_INLINE_TYPES;
        Self {
            operation,
            source,
            types,
            more,
            target: None,
            pressed: false,
            accepted: false,
            wants_position: false,
            last_position: Instant::now(),
            last_point: (i32::MIN, i32::MIN),
            awaiting_finished: None,
        }
    }

    /// Publish the full type list as XdndTypeList on the source window.
    pub(super) fn publish_type_list(&self, conn: &Conn, xdnd: &Xdnd, atom_type: u64) {
        let values: Vec<u32> = self.types.iter().map(|kind| *kind as u32).collect();
        conn.change_property32(self.source, xdnd.type_list, atom_type, &values);
    }

    pub(super) fn send_enter(&self, conn: &Conn, xdnd: &Xdnd, dest: u64, version: u32) {
        let data = window::enter(self.source, version, &self.types, self.more);
        conn.send_client_message(dest, xdnd.enter, data);
    }

    pub(super) fn send_position(&mut self, conn: &Conn, xdnd: &Xdnd, dest: u64, x: i32, y: i32) {
        let data = window::position(self.source, x, y, 0, xdnd.action_copy);
        conn.send_client_message(dest, xdnd.position, data);
        self.last_position = Instant::now();
        self.last_point = (x, y);
    }

    /// Position is re-sent when the pointer moved or the target asked for
    /// updates inside its (zero-sized) rectangle.
    pub(super) fn position_due(&self, x: i32, y: i32) -> bool {
        self.last_position.elapsed() >= POSITION_INTERVAL
            && (self.wants_position || self.last_point != (x, y))
    }

    pub(super) fn send_leave(&self, conn: &Conn, xdnd: &Xdnd, dest: u64) {
        conn.send_client_message(dest, xdnd.leave, window::leave(self.source));
    }

    pub(super) fn send_drop(&mut self, conn: &Conn, xdnd: &Xdnd, dest: u64, timestamp: u32) {
        conn.send_client_message(dest, xdnd.drop, window::drop(self.source, timestamp));
        self.awaiting_finished = Some(Instant::now());
    }

    pub(super) fn finish_timed_out(&self) -> bool {
        self.awaiting_finished.is_some_and(|since| since.elapsed() > FINISH_TIMEOUT)
    }
}
