//! The drag thread: connection setup, the command pump and the event loop.
//! `Run` owns every Xlib object; sibling modules add the own-drag and
//! foreign-session behaviour as `impl Run` blocks.
use std::sync::{Arc, mpsc};
use std::time::Duration;

use oink_foundation::Point;
use oink_foundation::dnd::DragAnchor;

use super::window::Xdnd;
use super::{Command, Shared, platform, source, target, window};
use crate::ClipboardError;
use crate::dnd::{DragOperation, PlatformDndEvent};
use crate::linux::atoms::Atoms;
use crate::linux::serve::{self, Context as ServeContext};
use crate::linux::sys::Wakeup;
use crate::linux::x11::{self, Conn};

/// Idle poll when no drag and no selection transfer is pending.
const IDLE_TICK: Duration = Duration::from_millis(60);

/// Delay between a leave on the replaced catcher and declaring the drag lost;
/// a replacement leave is followed immediately by an Enter on the new catcher.
pub(super) const MIGRATION_SETTLE: Duration = Duration::from_millis(300);

/// Everything the drag thread owns. The X connection lives and dies here.
pub(super) struct Run {
    pub(super) shared: Arc<Shared>,
    pub(super) conn: Conn,
    pub(super) atoms: Atoms,
    pub(super) xdnd: Xdnd,
    pub(super) catcher: u64,
    /// Former catcher window kept mapped until the source's hit test has
    /// demonstrably moved to [`Run::catcher`]; absorbs the XdndLeave GTK
    /// sends the old dest on the first motion after a replacement.
    pub(super) migration: Option<u64>,
    /// Held session that saw a leave on the replaced catcher and is waiting
    /// out the settle window to tell replacement churn from a real loss.
    pub(super) migration_left: Option<(DragOperation, std::time::Instant)>,
    pub(super) source_window: u64,
    pub(super) serve: serve::State,
    pub(super) probing: bool,
    pub(super) probe_at: Point,
    pub(super) foreign: Option<target::Foreign>,
    pub(super) held: Option<DragOperation>,
    pub(super) own: Option<source::Own>,
}

impl Run {
    pub(super) fn emit(&self, event: PlatformDndEvent) {
        self.shared.emit(event);
    }

    pub(super) fn retire_foreign(&mut self) {
        self.migration_left = None;
        if let Some(old) = self.migration.take() {
            self.conn.destroy_window(old);
        }
        self.foreign = None;
        self.held = None;
        if self.probing {
            // Probe means "watch this point until unprobe"; park the catcher
            // back on the probe point for the next drag.
            self.place_catcher(self.probe_at);
        } else {
            self.conn.unmap_window(self.catcher);
        }
    }

    fn place_catcher(&mut self, at: Point) {
        let (x, y, w, h) = target::catcher_geometry(at, self.conn.root_size());
        self.conn.move_resize_window(self.catcher, x, y, w, h);
        self.conn.map_raised(self.catcher);
    }

    fn new_catcher_window(&self) -> Option<u64> {
        let window = self.conn.create_override_window(0).ok()?;
        self.conn.change_property32(
            window,
            self.xdnd.aware,
            self.atoms.atom_type,
            &[window::XDND_VERSION],
        );
        Some(window)
    }

    /// The window the source currently talks to: until the source's hit
    /// test migrates, its dest is still the replaced catcher window.
    pub(super) fn reply_window(&self) -> u64 {
        self.migration.unwrap_or(self.catcher)
    }

    /// Swap the catcher for a fresh window at new geometry mid-drag.
    ///
    /// GTK3 hit-tests drags against a cache of root children that is fed by
    /// X events; a ConfigureNotify from resizing an override-redirect window
    /// mid-drag desynchronizes that cache and the drag silently leaves us
    /// (XdndLeave on the next motion, observed against gtk-3-24 on Xephyr).
    /// CreateNotify/MapNotify is the event path that populates the cache
    /// correctly, so geometry changes build a new window instead.
    ///
    /// The replaced window stays mapped as [`Run::migration`] until the
    /// source's next client message lands on the new catcher: GTK sends an
    /// XdndLeave to the former dest on the first motion after a dest change,
    /// and that Leave must not retire the held session.
    fn replace_catcher(&mut self, x: i32, y: i32, w: u32, h: u32) {
        let Some(new_window) = self.new_catcher_window() else {
            return; // Keep the current catcher rather than dropping the drag.
        };
        self.conn.move_resize_window(new_window, x, y, w, h);
        self.conn.map_raised(new_window);
        if let Some(older) = self.migration.replace(self.catcher) {
            self.conn.destroy_window(older);
        }
        self.catcher = new_window;
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Probe(at) => {
                if self.held.is_some() {
                    return; // A queued follow must not shrink the parking surface.
                }
                self.probing = true;
                self.probe_at = at;
                self.place_catcher(at);
            }
            Command::Unprobe => {
                self.probing = false;
                if self.held.is_none() {
                    self.conn.unmap_window(self.catcher);
                }
            }
            Command::UnprobeSource(operation) => {
                if self.foreign.as_ref().is_some_and(|foreign| foreign.operation == operation) {
                    // Hide immediately; the source's Leave retires the session.
                    self.held = None;
                    self.probing = false;
                    self.conn.unmap_window(self.catcher);
                }
            }
            Command::HoldSource { operation, reply } => {
                let result = self.hold(operation);
                let _ = reply.send(result);
            }
            Command::ResumeSource { operation, at, reply } => {
                let ok = self.resume(operation, at);
                let _ = reply.send(ok);
            }
            Command::FinishSource { operation, accept, reply } => {
                let ok = self.finish(operation, accept);
                let _ = reply.send(ok);
            }
            Command::Begin { spec, reply } => {
                let result = self.begin(*spec);
                let _ = reply.send(result);
            }
            Command::Cancel { reply } => {
                let ok = self.cancel_own();
                let _ = reply.send(ok);
            }
            Command::Stop => {}
        }
    }

    fn hold(&mut self, operation: DragOperation) -> Result<DragAnchor, ClipboardError> {
        let Some(foreign) = &self.foreign else {
            return Err(ClipboardError::Platform("no drag on the catcher".into()));
        };
        if foreign.operation != operation || self.held.is_some() || !self.probing {
            return Err(ClipboardError::Platform("catcher state does not match the hold".into()));
        }
        if !foreign.usable() {
            return Err(ClipboardError::Platform("drag content is not usable".into()));
        }
        self.held = Some(operation);
        // Span the whole root so the pointer can never leave the anchor
        // surface while the drag is held. Done as a window replacement:
        // resizing mid-drag desynchronizes GTK3's root-child cache.
        let (width, height) = self.conn.root_size();
        self.replace_catcher(0, 0, width, height);
        let at = self
            .conn
            .query_pointer()
            .map(|(x, y)| Point::new(f64::from(x), f64::from(y)))
            .unwrap_or(self.probe_at);
        Ok(DragAnchor {
            generation: operation.sequence,
            at,
            surface: self.catcher as usize,
        })
    }

    fn resume(&mut self, operation: DragOperation, at: Option<Point>) -> bool {
        if self.held != Some(operation) {
            return false;
        }
        self.held = None;
        let (x, y, w, h) = target::catcher_geometry(at.unwrap_or(self.probe_at), self.conn.root_size());
        self.replace_catcher(x, y, w, h);
        true
    }

    fn finish(&mut self, operation: DragOperation, accept: bool) -> bool {
        // The final commit runs after the router resumed the source, so the
        // session is live on the catcher but no longer held.
        if self
            .foreign
            .as_ref()
            .is_none_or(|foreign| foreign.operation != operation || foreign.finishing)
            || !self.probing
        {
            return false;
        }
        if accept {
            // The release closure submitted by the agent raises the source's
            // button, which sends XdndDrop here; foreign_drop reports the
            // accepted end for a finishing session even without a hold.
            if let Some(foreign) = &mut self.foreign {
                foreign.finishing = true;
                foreign.start_finish();
            }
            true
        } else {
            let from = self.reply_window();
            if let Some(foreign) = &mut self.foreign {
                foreign.reply_reject(&self.conn, &self.xdnd, from);
            }
            self.emit(PlatformDndEvent::SourceEnded {
                operation,
                accepted: false,
            });
            self.retire_foreign();
            true
        }
    }

    fn dispatch_event(&mut self, event: &x11::XEvent, serve_ctx: &ServeContext) {
        let had_ownership = self.serve.ownership.is_some();
        serve::handle_event(&self.conn, &self.atoms, serve_ctx, &mut self.serve, event);
        if had_ownership && self.serve.ownership.is_none() && self.own.is_some() {
            // The XdndSelection was taken from us mid-drag.
            let operation = self.own.as_ref().map(|own| own.operation);
            self.end_own();
            if let Some(operation) = operation {
                self.emit(PlatformDndEvent::Ended {
                    operation,
                    dropped: false,
                });
            }
            return;
        }
        let Some((window, kind, data)) = self.conn.client_message_words(event) else {
            return;
        };
        enum Side {
            Foreign(u64, [u32; 5]),
            Own(u64, [u32; 5]),
            /// The replaced catcher only exists to absorb GTK's Leave to the
            /// former dest; every message on it is replacement churn.
            Migration,
            Ignore,
        }
        let side = if window == self.catcher {
            Side::Foreign(kind, data)
        } else if window == self.source_window {
            Side::Own(kind, data)
        } else if self.migration == Some(window) {
            Side::Migration
        } else {
            Side::Ignore
        };
        match side {
            Side::Foreign(kind, data) => self.foreign_message(kind, data),
            Side::Own(kind, data) => self.own_message(kind, data),
            Side::Migration => self.migration_message(kind, data),
            Side::Ignore => {}
        }
    }
}

pub(super) fn run(shared: Arc<Shared>, commands: mpsc::Receiver<Command>, wake: Arc<Wakeup>) {
    let setup = || -> Result<(Conn, Atoms, Xdnd, u64, u64), ClipboardError> {
        let conn = Conn::open().map_err(platform)?;
        let atoms = Atoms::load(&conn).map_err(platform)?;
        let xdnd = Xdnd::load(&conn).map_err(platform)?;
        let catcher = conn.create_override_window(0).map_err(platform)?;
        conn.change_property32(catcher, xdnd.aware, atoms.atom_type, &[window::XDND_VERSION]);
        // The drag source endpoint stays unmapped; only protocol peers see
        // it. PropertyChangeMask lets ownership installs round-trip a server
        // timestamp through this window.
        let source_window =
            conn.create_override_window(x11::PROPERTY_CHANGE_MASK).map_err(platform)?;
        Ok((conn, atoms, xdnd, catcher, source_window))
    };
    let Ok((conn, atoms, xdnd, catcher, source_window)) = setup() else {
        return;
    };
    let (jobs_tx, jobs_rx) = mpsc::channel();
    let serve_ctx = ServeContext {
        window: source_window,
        selection: xdnd.selection,
        wake: wake.clone(),
        jobs: jobs_tx,
    };
    let mut run = Run {
        shared: shared.clone(),
        conn,
        atoms,
        xdnd,
        catcher,
        migration: None,
        migration_left: None,
        source_window,
        serve: serve::State::default(),
        probing: false,
        probe_at: Point::default(),
        foreign: None,
        held: None,
        own: None,
    };
    let mut stopping = false;
    loop {
        while let Ok(command) = commands.try_recv() {
            if matches!(command, Command::Stop) {
                stopping = true;
            } else {
                run.command(command);
            }
        }
        let mut events = std::mem::take(&mut run.serve.stash);
        while run.conn.pending() {
            events.push(run.conn.next_event());
        }
        for event in events {
            run.dispatch_event(&event, &serve_ctx);
        }
        while let Ok(done) = jobs_rx.try_recv() {
            serve::finish_job(&run.conn, &run.atoms, run.xdnd.selection, &mut run.serve, done);
        }
        run.poll_foreign_capture();
        run.poll_foreign_watchdog();
        run.poll_migration();
        run.poll_own();
        serve::expire(&run.conn, &mut run.serve);
        run.conn.flush();
        if stopping {
            break;
        }
        let timeout = if run.own.is_some() {
            source::POLL_INTERVAL
        } else {
            serve::next_tick(&run.serve, IDLE_TICK)
        };
        let _ = crate::linux::sys::wait_readable2(run.conn.fd(), wake.fd(), timeout);
        wake.drain();
    }
}
