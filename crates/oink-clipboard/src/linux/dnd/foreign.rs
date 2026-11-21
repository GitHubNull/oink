//! Foreign drag sessions on the catcher: Enter/Position/Leave/Drop replies,
//! the eager-capture consumer that reports Detected, and the finish watchdog.
use std::time::Duration;

use super::run::Run;
use super::target::{self, Foreign, ForeignResult};
use super::window::{self};
use crate::dnd::{DragOperation, PlatformDndEvent};
use crate::linux::atoms;

/// A held foreign source that never drops after finish(accept) is declared lost.
const FINISH_WATCHDOG: Duration = Duration::from_secs(2);
const MAX_TYPE_LIST: usize = 256;

impl Run {
    pub(super) fn foreign_message(&mut self, kind: u64, data: [u32; 5]) {
        enum Kind {
            Enter,
            Position,
            Leave,
            Drop,
            Other,
        }
        let kind = if kind == self.xdnd.enter {
            Kind::Enter
        } else if kind == self.xdnd.position {
            Kind::Position
        } else if kind == self.xdnd.leave {
            Kind::Leave
        } else if kind == self.xdnd.drop {
            Kind::Drop
        } else {
            Kind::Other
        };
        if self.migration.is_some() || self.migration_left.is_some() {
            // A live message reached the replacement catcher, so the source's
            // hit test has moved; the former window's only pending message is
            // the absorbed Leave. Destroy it to keep the root clean.
            self.migration_left = None;
            if let Some(old) = self.migration.take() {
                self.conn.destroy_window(old);
            }
        }
        match kind {
            Kind::Enter => self.foreign_enter(data),
            Kind::Position => {
                if let Some(foreign) = &self.foreign {
                    foreign.reply_status(&self.conn, &self.xdnd, self.catcher, true);
                }
            }
            Kind::Leave => self.foreign_leave(u64::from(data[0])),
            Kind::Drop => self.foreign_drop(u64::from(data[0]), self.catcher),
            Kind::Other => {}
        }
    }

    fn foreign_enter(&mut self, data: [u32; 5]) {
        let source = u64::from(data[0]);
        let version = data[1] >> 24;
        if !(3..=window::XDND_VERSION).contains(&version) {
            return;
        }
        let kinds = if data[1] & 1 != 0 {
            self.conn
                .read_values32_keep(source, self.xdnd.type_list, MAX_TYPE_LIST)
                .unwrap_or_default()
        } else {
            data[2..].iter().copied().filter(|kind| *kind != 0).collect()
        };
        let names: Vec<String> = kinds
            .iter()
            .filter_map(|kind| self.conn.atom_name(u64::from(*kind)))
            .filter(|name| !atoms::is_control(name))
            .collect();
        if names.is_empty() {
            return;
        }
        if self.foreign.as_ref().is_some_and(|foreign| foreign.source == source) {
            return; // Duplicate Enter for the same session.
        }
        let foreign = Foreign::new(DragOperation::new(), source, names);
        foreign.reply_status(&self.conn, &self.xdnd, self.catcher, true);
        self.foreign = Some(foreign);
    }

    fn foreign_leave(&mut self, source: u64) {
        let matches = self
            .foreign
            .as_ref()
            .is_some_and(|foreign| foreign.source == source);
        if !matches {
            return;
        }
        if let Some(foreign) = &self.foreign
            && self.held == Some(foreign.operation)
        {
            let operation = foreign.operation;
            self.retire_foreign();
            self.emit(PlatformDndEvent::SourceCatcherLost { operation });
        } else {
            self.foreign = None; // An ordinary probe may re-enter later.
        }
    }

    fn foreign_drop(&mut self, source: u64, arrived_on: u64) {
        let matches = self
            .foreign
            .as_ref()
            .is_some_and(|foreign| foreign.source == source);
        if !matches {
            return;
        }
        let (operation, accepted) = match &self.foreign {
            Some(foreign) => (
                foreign.operation,
                self.held == Some(foreign.operation) || foreign.finishing,
            ),
            None => return,
        };
        if let Some(foreign) = &mut self.foreign {
            foreign.accepted = accepted;
            // GTK matches Finished against the window the Drop was sent to
            // (its drop_xid), which can be the replaced catcher.
            foreign.reply_finished(&self.conn, &self.xdnd, arrived_on);
        }
        self.retire_foreign();
        if accepted {
            self.emit(PlatformDndEvent::SourceEnded {
                operation,
                accepted: true,
            });
        }
        // Unheld probe drops end silently.
    }

    /// Consume the capture outcome and report the drag once it is ready.
    pub(super) fn poll_foreign_capture(&mut self) {
        let outcome = {
            let Some(foreign) = &mut self.foreign else {
                return;
            };
            if foreign.reported() {
                return;
            }
            match foreign.take_capture() {
                Some(ForeignResult::Done(capture)) => Some(Ok(capture)),
                Some(ForeignResult::Failed) => Some(Err(())),
                None => None,
            }
        };
        match outcome {
            Some(Ok(capture)) => {
                if let Some(foreign) = &mut self.foreign {
                    foreign.mark_reported(true);
                }
                let operation = capture.operation;
                *self.shared.source_data.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((operation, capture.provider.clone()));
                self.emit(target::detected_event(operation, capture.items.clone()));
            }
            Some(Err(())) => {
                if let Some(foreign) = &mut self.foreign {
                    foreign.mark_reported(false);
                }
            }
            None => {}
        }
    }

    pub(super) fn poll_foreign_watchdog(&mut self) {
        let lost = self.foreign.as_ref().is_some_and(|foreign| {
            foreign.finishing && foreign.finish_age().is_some_and(|age| age > FINISH_WATCHDOG)
        });
        if lost
            && let Some(foreign) = &self.foreign
        {
            let operation = foreign.operation;
            self.retire_foreign();
            self.emit(PlatformDndEvent::SourceCatcherLost { operation });
        }
    }

    /// A message arrived on the replaced catcher window. GTK leaves the
    /// former dest on the first motion after a replacement and then enters
    /// the new catcher; that churn is absorbed here. A drop can also land
    /// here when no motion separated the replacement from the release,
    /// because the drop follows GTK's cached dest id.
    pub(super) fn migration_message(&mut self, kind: u64, data: [u32; 5]) {
        if let Some(foreign) = &self.foreign
            && kind == self.xdnd.position
        {
            // GTK's dest is still the former window; keep answering.
            foreign.reply_status(&self.conn, &self.xdnd, self.migration.unwrap_or(self.catcher), true);
        }
        if kind == self.xdnd.drop {
            if let Some(arrived_on) = self.migration {
                self.foreign_drop(u64::from(data[0]), arrived_on);
            }
            return;
        }
        if kind != self.xdnd.leave {
            return;
        }
        let Some(foreign) = &self.foreign else {
            return;
        };
        if u64::from(data[0]) != foreign.source {
            return;
        }
        // Replacement churn is followed by an Enter on the new catcher; a
        // leave with no follow-up within the settle window is a real loss.
        self.migration_left = Some((foreign.operation, std::time::Instant::now()));
    }

    /// Settle a session that saw a leave on the replaced catcher: if no live
    /// message followed, the drag really left.
    pub(super) fn poll_migration(&mut self) {
        let Some((operation, at)) = self.migration_left else {
            return;
        };
        if at.elapsed() <= super::run::MIGRATION_SETTLE {
            return;
        }
        self.migration_left = None;
        if self.held == Some(operation) {
            self.retire_foreign();
            self.emit(PlatformDndEvent::SourceCatcherLost { operation });
        } else {
            // An ordinary probe session ends quietly.
            if let Some(old) = self.migration.take() {
                self.conn.destroy_window(old);
            }
            self.foreign = None;
        }
    }
}
