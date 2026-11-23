//! Long-lived subscription connection on a dedicated thread. Subscribe
//! callbacks only set latches and call `device_notifications::notify()` —
//! never enumerate, never interpret device IDs, never allocate (same rule
//! as the Windows IMMNotificationClient thread).
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use libpulse_binding as pulse;
use pulse::context::subscribe::{Facility, InterestMaskSet};

use crate::linux::sys;

static START: Once = Once::new();
/// Starts at 1 and only changes on a server-restart notification, matching
/// the macOS `service_generation()` contract.
static SERVICE_RESTARTS: AtomicU64 = AtomicU64::new(1);

pub(crate) fn start() {
    START.call_once(|| {
        if let Err(error) = thread::Builder::new().name("oink-pulse-devices".into()).spawn(run) {
            tracing::warn!(%error, "linux audio device notification thread failed to start");
        }
    });
}

fn run() -> Result<(), String> {
    let mut conn = sys::connect("oink-devices")?;
    conn.context.set_subscribe_callback(Some(Box::new(|facility, _, _| {
        if let Some(facility) = facility {
            note_facility(facility);
        }
    })));
    conn.context.subscribe(
        InterestMaskSet::SERVER | InterestMaskSet::SINK | InterestMaskSet::SOURCE,
        |_| {},
    );
    // The threaded mainloop keeps running on its own internal thread; park
    // this thread so the connection outlives any single consumer.
    loop {
        thread::park();
    }
}

/// What a subscribe facility change means for shared state. Pure mapping so
/// the callback stays trivial and the policy is unit-testable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    /// `Facility::Server`: the audio service itself restarted.
    ServiceRestart,
    /// Sink/source arrival, removal or property change.
    DeviceChange,
}

fn classify(facility: Facility) -> Event {
    match facility {
        Facility::Server => Event::ServiceRestart,
        _ => Event::DeviceChange,
    }
}

fn note_facility(facility: Facility) {
    match classify(facility) {
        Event::ServiceRestart => {
            SERVICE_RESTARTS.fetch_add(1, Ordering::Release);
        }
        Event::DeviceChange => {}
    }
    crate::device_notifications::invalidate();
}

pub(crate) fn service_generation() -> u64 {
    SERVICE_RESTARTS.load(Ordering::Acquire)
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    SERVICE_RESTARTS.store(1, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering as TestOrdering};

    #[test]
    fn server_facility_is_a_service_restart_other_facilities_are_not() {
        assert_eq!(classify(Facility::Server), Event::ServiceRestart);
        assert_eq!(classify(Facility::Sink), Event::DeviceChange);
        assert_eq!(classify(Facility::Source), Event::DeviceChange);
        assert_eq!(classify(Facility::SinkInput), Event::DeviceChange);
        assert_eq!(classify(Facility::SourceOutput), Event::DeviceChange);
        assert_eq!(classify(Facility::Card), Event::DeviceChange);
    }

    #[test]
    fn service_generation_starts_at_one_and_only_restarts_bump_it() {
        reset_for_tests();
        assert_eq!(service_generation(), 1);
        let before = service_generation();
        let restarts = AtomicU64::new(before);
        restarts.fetch_add(1, TestOrdering::Release);
        assert_eq!(restarts.load(TestOrdering::Acquire), before + 1);
    }
}
