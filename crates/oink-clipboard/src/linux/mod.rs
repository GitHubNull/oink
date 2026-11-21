//! X11 CLIPBOARD backend: ownership, XFixes observation, bounded transfers.
//! Only CLIPBOARD is ever touched; PRIMARY is never registered or captured.
mod atoms;
mod binding;
mod capture;
mod publish;
mod selection;
mod serve;
mod sys;
mod uris;
mod virtual_files;
mod watch;
mod x11;

use crate::content;
use crate::{
    CapturedClipboard, ChangeHandler, ClipData, ClipboardBackend, ClipboardError, ClipSnapshot, NativeSpec, Sensitivity,
};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) owner: u64,
    pub(crate) timestamp: u32,
}

impl Identity {
    /// A missing TIMESTAMP target is common and returns 0; the owner still
    /// distinguishes ownership until a trustworthy timestamp appears.
    fn same(self, other: Self) -> bool {
        self.owner == other.owner
            && (self.timestamp == other.timestamp || self.timestamp == 0 || other.timestamp == 0)
    }
}

fn same(left: Option<Identity>, right: Option<Identity>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.same(right),
        _ => false,
    }
}

#[derive(Default)]
struct State {
    /// The identity last observed by polling or by an XFixes event.
    last: Option<Option<Identity>>,
    /// XFixes SetSelectionOwner events are deduplicated against polls.
    last_event: Option<(u64, u32)>,
    self_identity: Option<Identity>,
}

pub(crate) struct Engine {
    count: AtomicU64,
    state: Mutex<State>,
    handler: Mutex<Option<ChangeHandler>>,
    paused: AtomicBool,
    boot: Vec<u8>,
    display: String,
}

impl Engine {
    fn new() -> Result<Self, ClipboardError> {
        let display = x11::Conn::open()
            .map_err(|error| ClipboardError::Platform(format!("X11: {error}")))?
            .display_name();
        let boot = std::fs::read("/proc/sys/kernel/random/boot_id").unwrap_or_default();
        Ok(Self {
            count: AtomicU64::new(0),
            state: Mutex::new(State::default()),
            handler: Mutex::new(None),
            paused: AtomicBool::new(false),
            boot,
            display,
        })
    }

    pub(crate) fn count(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }

    fn set_handler(&self, handler: ChangeHandler) {
        *self.handler.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handler);
    }

    fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    /// The clipboard identity is metadata only; payload bytes are never read.
    fn clipboard_key(&self) -> std::io::Result<[u8; 32]> {
        let identity = selection::identity()?;
        let mut hash = Sha256::new();
        hash.update(&self.boot);
        hash.update(self.display.as_bytes());
        let (owner, timestamp) = identity.map(|id| (id.owner, id.timestamp)).unwrap_or((0, 0));
        hash.update(owner.to_le_bytes());
        hash.update(timestamp.to_le_bytes());
        Ok(hash.finalize().into())
    }

    fn note_initial(&self, identity: Option<Identity>) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.last.is_none() {
            state.last = Some(identity);
        }
    }

    /// Capture saw this identity; never regress a newer observation back to it.
    fn note_captured(&self, identity: Identity) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match &mut state.last {
            None => state.last = Some(Some(identity)),
            Some(Some(previous)) if previous.same(identity) && previous.timestamp == 0 => {
                *previous = identity;
            }
            _ => {}
        }
    }

    /// Record an observation. `event` carries the XFixes ownership timestamp
    /// when the observation came from an event; polls compare permissively
    /// because lazy owners refuse TIMESTAMP.
    fn observe(&self, identity: Option<Identity>, event: Option<(u64, u32)>) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed = if let Some(event) = event {
            if state.last_event == Some(event) {
                return;
            }
            state.last_event = Some(event);
            match (&state.last, identity) {
                (Some(None), None) => false,
                (Some(Some(previous)), Some(current)) => {
                    previous.owner != current.owner || previous.timestamp != current.timestamp
                }
                _ => true,
            }
        } else {
            !same(state.last.flatten(), identity)
        };
        if !changed {
            // Prefer a real timestamp over a lazy owner's zero for deduplication.
            if let Some(current) = identity
                && let Some(Some(previous)) = &mut state.last
                && previous.same(current)
                && previous.timestamp == 0
                && current.timestamp != 0
            {
                *previous = current;
            }
            return;
        }
        state.last = Some(identity);
        if self.paused.load(Ordering::SeqCst) {
            // Observation is paused like an unregistered listener: record the
            // identity so resuming does not replay an old change.
            return;
        }
        if let Some(current) = identity
            && state.self_identity.is_some_and(|me| me.same(current))
        {
            // Our own publication is reported by write()'s return count.
            return;
        }
        self.count.fetch_add(1, Ordering::AcqRel);
        let handler = self
            .handler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        drop(state);
        if let Some(handler) = handler {
            handler();
        }
    }

    /// Record our identity before ownership changes so the observer can
    /// recognize the echo even if its event arrives first.
    fn expect_self(&self, identity: Identity) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.self_identity = Some(identity);
    }

    /// Account for a completed publication and return its post-change count.
    /// An observer echo of this identity was already suppressed, so the write
    /// is the only place that counts it.
    fn note_write(&self, identity: Identity) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.self_identity = Some(identity);
        state.last = Some(Some(identity));
        self.count.fetch_add(1, Ordering::AcqRel) + 1
    }
}

pub(crate) struct Backend {
    engine: Arc<Engine>,
    owner: publish::Owner,
    observer: watch::Observer,
}

impl Backend {
    fn new() -> Result<Self, ClipboardError> {
        let engine = Arc::new(Engine::new()?);
        let observer = watch::Observer::spawn(engine.clone())?;
        let owner = publish::Owner::spawn(engine.clone())?;
        Ok(Self {
            engine,
            owner,
            observer,
        })
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // Stop observing before teardown so our own release is not a change.
        self.observer.stop();
        self.owner.stop();
    }
}

impl ClipboardBackend for Backend {
    fn change_count(&self) -> u64 {
        self.engine.count()
    }

    fn clipboard_key(&self) -> std::io::Result<[u8; 32]> {
        self.engine.clipboard_key()
    }

    fn read(&self) -> Result<ClipSnapshot, ClipboardError> {
        capture::capture(&self.engine).map(|captured| captured.snapshot)
    }

    fn capture(&self, _data: &std::path::Path) -> Result<CapturedClipboard, ClipboardError> {
        capture::capture(&self.engine)
    }

    fn write_native(&self, spec: NativeSpec) -> Result<u64, ClipboardError> {
        let binding = binding::from_spec(&spec)?;
        self.owner.publish(binding)
    }

    fn write_native_guarded(
        &self,
        spec: NativeSpec,
        guard: content::ReaderGuard,
    ) -> Result<u64, ClipboardError> {
        let binding = binding::from_spec(&spec)?;
        guard().map_err(|_| ClipboardError::Busy)?;
        self.owner.publish(binding)
    }

    fn write_native_cancellable(
        &self,
        spec: NativeSpec,
        guard: content::ReaderGuard,
        current: content::ReaderGuard,
    ) -> Result<u64, ClipboardError> {
        current().map_err(|_| ClipboardError::Busy)?;
        self.write_native_guarded(spec, guard)
    }

    fn write(&self, items: &[ClipData], sensitivity: Sensitivity) -> Result<u64, ClipboardError> {
        let binding = binding::from_items(items, sensitivity)?;
        self.owner.publish(binding)
    }

    fn write_virtual_files(
        &self,
        entries: &[crate::FileEntry],
        others: &[ClipData],
        _contents: std::sync::Arc<dyn crate::FileContents>,
        sensitivity: Sensitivity,
    ) -> Result<u64, ClipboardError> {
        // X11 transfers are path based; content is read by the pasting app.
        // Entries served from elsewhere carry no local path until the FUSE
        // bridge, so a list without local paths cannot be published.
        let mut binding = if others.is_empty() && sensitivity == Sensitivity::Normal {
            binding::Binding::default()
        } else {
            // A non-normal sensitivity still contributes its marker targets.
            binding::from_items(others, sensitivity)?
        };
        virtual_files::add(&mut binding, &virtual_files::top_paths(entries));
        if binding.targets.is_empty() {
            return Err(ClipboardError::Unsupported);
        }
        self.owner.publish(binding)
    }

    fn watch(&self, on_change: ChangeHandler) -> Result<(), ClipboardError> {
        self.engine.set_handler(on_change);
        Ok(())
    }

    fn set_watch_paused(&self, paused: bool) -> Result<(), ClipboardError> {
        self.engine.set_paused(paused);
        Ok(())
    }
}

pub(crate) fn backend() -> Result<Box<dyn ClipboardBackend>, ClipboardError> {
    Ok(Box::new(Backend::new()?))
}

fn platform(error: std::io::Error) -> ClipboardError {
    ClipboardError::Platform(format!("X11: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(owner: u64, timestamp: u32) -> Identity {
        Identity { owner, timestamp }
    }

    fn engine() -> Engine {
        Engine {
            count: AtomicU64::new(0),
            state: Mutex::new(State::default()),
            handler: Mutex::new(None),
            paused: AtomicBool::new(false),
            boot: Vec::new(),
            display: String::new(),
        }
    }

    #[test]
    fn self_publications_count_once_without_callbacks() {
        let engine = engine();
        engine.note_initial(Some(identity(1, 10)));
        let published = identity(9, 99);
        engine.expect_self(published);
        // The XFixes event may be observed before the writer returns; it is an
        // echo of our own publication and must not count or fire the handler.
        engine.observe(Some(published), Some((9, 99)));
        assert_eq!(engine.count(), 0);
        assert_eq!(engine.note_write(published), 1);
        engine.observe(Some(published), None);
        assert_eq!(engine.count(), 1);
    }

    #[test]
    fn foreign_events_and_polls_never_double_count() {
        let engine = engine();
        engine.note_initial(Some(identity(1, 10)));
        let change = identity(2, 20);
        engine.observe(Some(change), Some((2, 20)));
        assert_eq!(engine.count(), 1);
        engine.observe(Some(change), None);
        engine.observe(Some(change), Some((2, 20)));
        assert_eq!(engine.count(), 1);
        // A lazy owner reports 0 for TIMESTAMP; its event timestamp still counts.
        let next = identity(2, 30);
        engine.observe(Some(identity(2, 0)), None);
        engine.observe(Some(next), Some((2, 30)));
        assert_eq!(engine.count(), 2);
    }

    #[test]
    fn pause_records_but_does_not_count_or_replay() {
        let engine = engine();
        engine.note_initial(None);
        engine.set_paused(true);
        engine.observe(Some(identity(3, 30)), Some((3, 30)));
        assert_eq!(engine.count(), 0);
        engine.set_paused(false);
        engine.observe(Some(identity(3, 30)), None);
        assert_eq!(engine.count(), 0);
    }

    #[test]
    fn capture_never_regresses_the_observed_identity() {
        let engine = engine();
        engine.note_initial(None);
        engine.observe(Some(identity(4, 40)), Some((4, 40)));
        engine.note_captured(identity(4, 0));
        let state = engine.state.lock().unwrap();
        assert_eq!(state.last, Some(Some(identity(4, 40))));
        assert_eq!(engine.count(), 1);
    }
}
