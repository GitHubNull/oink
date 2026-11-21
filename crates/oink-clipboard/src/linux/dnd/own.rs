//! Our own drag for remote content: the press, target tracking, the release
//! handling and cancel. Runs entirely on the drag thread.
use oink_foundation::Point;
use oink_foundation::input::{InjectEvent, MouseButton};

use super::run::Run;
use super::source::{self, Own};
use super::window::{self, Xdnd};
use crate::ClipboardError;
use crate::Sensitivity;
use crate::dnd::{DragOperation, DragSpec, PlatformDndEvent};
use crate::linux::x11::Conn;
use crate::linux::{binding, publish};
impl Run {
    pub(super) fn cancel_own(&mut self) -> bool {
        let plan = {
            let Some(own) = &mut self.own else {
                return true;
            };
            if let Some(current) = &own.target {
                own.send_leave(&self.conn, &self.xdnd, current.window);
            }
            (own.operation, own.pressed)
        };
        let (operation, pressed) = plan;
        if pressed {
            let _ = self.shared.inject(&InjectEvent::Button {
                button: MouseButton::Left,
                pressed: false,
            });
        }
        self.end_own();
        self.emit(PlatformDndEvent::Ended {
            operation,
            dropped: false,
        });
        true
    }

    pub(super) fn end_own(&mut self) {
        if let Some(own) = &self.own {
            publish::release_selection(&self.conn, self.xdnd.selection, own.source);
        }
        self.own = None;
    }

    pub(super) fn begin(&mut self, spec: DragSpec) -> Result<(), ClipboardError> {
        if self.own.is_some() {
            return Err(ClipboardError::Platform("a drag is already active".into()));
        }
        let binding = match &spec.native {
            Some(native) => binding::from_spec(native)?,
            None => binding::from_items(&spec.items, Sensitivity::Normal)?,
        };
        publish::install_binding(
            &self.conn,
            &self.atoms,
            self.xdnd.selection,
            &mut self.serve,
            binding,
            self.source_window,
        )?;
        let ownership = self
            .serve
            .ownership
            .as_ref()
            .ok_or_else(|| ClipboardError::Platform("drag ownership failed".into()))?;
        let types = ownership.installed.atoms.clone();
        let operation = spec.operation;
        let at = spec.at;
        let mut own = Own::new(operation, self.source_window, types);
        own.publish_type_list(&self.conn, &self.xdnd, self.atoms.atom_type);
        // The source window is a protocol endpoint only; it never maps.
        if let Err(error) = self.press_at(&mut own, at) {
            let _ = self.shared.inject(&InjectEvent::Button {
                button: MouseButton::Left,
                pressed: false,
            });
            publish::release_selection(&self.conn, self.xdnd.selection, self.source_window);
            return Err(error);
        }
        self.own = Some(own);
        Ok(())
    }

    /// Place the press atomically with the input owner when it offers the
    /// guarded start; otherwise inject a move and the button directly.
    fn press_at(&self, own: &mut Own, at: Point) -> Result<(), ClipboardError> {
        let shared = self.shared.clone();
        let operation = own.operation;
        let mut outcome: Result<(), String> = Ok(());
        {
            let mut critical = || {
                let move_result = shared.inject(&InjectEvent::MoveTo { pos: at });
                let press_result = match shared.press.get() {
                    Some(press) => press(operation, at),
                    None => shared.inject(&InjectEvent::Button {
                        button: MouseButton::Left,
                        pressed: true,
                    }),
                };
                outcome = match (move_result, press_result) {
                    (Ok(()), Ok(())) => Ok(()),
                    (Err(error), _) | (_, Err(error)) => Err(error),
                };
            };
            let guard = match self.shared.start.get() {
                Some(start) => start(&mut critical),
                None => {
                    critical();
                    Ok(0)
                }
            };
            if let Err(error) = guard {
                return Err(ClipboardError::Platform(format!("drag press: {error}")));
            }
        }
        outcome.map_err(|error| ClipboardError::Platform(format!("drag press: {error}")))
    }

    /// Drive one poll cycle of our own drag: pointer tracking, target
    /// discovery, Enter/Position submission and the release handling.
    pub(super) fn poll_own(&mut self) {
        enum Step {
            None,
            Released,
            Timeout,
            Track(i32, i32),
        }
        let step = {
            let Some(own) = &mut self.own else {
                return;
            };
            if own.awaiting_finished.is_some() {
                if own.finish_timed_out() {
                    Step::Timeout
                } else {
                    Step::None
                }
            } else if let Some((x, y, mask)) = self.conn.query_pointer_full() {
                // Core protocol mask: Button1Mask is 1 << 8.
                if !own.pressed && mask & (1 << 8) != 0 {
                    own.pressed = true;
                }
                if own.pressed && mask & (1 << 8) == 0 {
                    Step::Released
                } else {
                    Step::Track(x, y)
                }
            } else {
                Step::None
            }
        };
        match step {
            Step::None => {}
            Step::Released => self.own_release(),
            Step::Timeout => {
                let (operation, accepted) = match &self.own {
                    Some(own) => (own.operation, own.accepted),
                    None => return,
                };
                self.end_own();
                self.emit(PlatformDndEvent::Ended { operation, dropped: accepted });
            }
            Step::Track(x, y) => {
                let skip = [self.source_window, self.catcher];
                if let Some(own) = &mut self.own {
                    track_target(&self.conn, &self.xdnd, &skip, own, x, y);
                }
            }
        }
    }

    fn own_release(&mut self) {
        enum Plan {
            Drop(u64),
            End(DragOperation),
        }
        let plan = {
            let Some(own) = &mut self.own else {
                return;
            };
            match &own.target {
                Some(current) if own.accepted => Plan::Drop(current.window),
                Some(current) => {
                    own.send_leave(&self.conn, &self.xdnd, current.window);
                    Plan::End(own.operation)
                }
                None => Plan::End(own.operation),
            }
        };
        match plan {
            Plan::Drop(dest) => {
                if let Some(own) = &mut self.own {
                    own.send_drop(&self.conn, &self.xdnd, dest, 0);
                }
            }
            Plan::End(operation) => {
                self.end_own();
                self.emit(PlatformDndEvent::Ended {
                    operation,
                    dropped: false,
                });
            }
        }
    }

    pub(super) fn own_message(&mut self, kind: u64, data: [u32; 5]) {
        let finished = {
            let Some(own) = &mut self.own else {
                return;
            };
            if kind == self.xdnd.status {
                own.accepted = data[1] & 1 != 0;
                own.wants_position = data[1] & 2 != 0;
                None
            } else if kind == self.xdnd.finished && own.awaiting_finished.is_some() {
                Some((own.operation, data[1] & 1 != 0))
            } else {
                None
            }
        };
        if let Some((operation, dropped)) = finished {
            self.end_own();
            self.emit(PlatformDndEvent::Ended { operation, dropped });
        }
    }
}

/// Track the window under our drag and keep the XDND conversation going.
fn track_target(conn: &Conn, xdnd: &Xdnd, skip: &[u64], own: &mut Own, x: i32, y: i32) {
    let found = window::find_dest(conn, xdnd, skip);
    let current = own.target.as_ref();
    match (found, current) {
        (Some((dest, version)), Some(now)) if now.window != dest => {
            own.send_leave(conn, xdnd, now.window);
            own.send_enter(conn, xdnd, dest, version);
            own.send_position(conn, xdnd, dest, x, y);
            own.target = Some(source::Current { window: dest });
        }
        (Some((dest, version)), None) => {
            own.send_enter(conn, xdnd, dest, version);
            own.send_position(conn, xdnd, dest, x, y);
            own.target = Some(source::Current { window: dest });
        }
        (Some((dest, _)), Some(_)) => {
            if own.position_due(x, y) {
                own.send_position(conn, xdnd, dest, x, y);
            }
        }
        (None, Some(now)) => {
            own.send_leave(conn, xdnd, now.window);
            own.target = None;
            own.accepted = false;
        }
        (None, None) => {}
    }
}
