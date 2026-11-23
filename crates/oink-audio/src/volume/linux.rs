//! Pulse sink volume/mute endpoint. The endpoint owns a dedicated
//! subscription connection: the subscribe callback only latches `changed`
//! and calls the owner wake — never enumerates, never interprets IDs. Reads
//! and writes introspect the exact sink by name and report measured values,
//! so a rounded write is always followed by an actual readback.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use libpulse_binding as pulse;
use pulse::callbacks::ListResult;
use pulse::context::subscribe::{Facility, InterestMaskSet};
use pulse::volume::{ChannelVolumes, Volume};

use super::{Endpoint, ExitVolumeStatus, Fault, Reading};
use crate::linux::{OUTPUT_PREFIX, sys};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SinkReading {
    channels: u8,
    percent: u8,
    muted: bool,
}

fn volume_to_percent(volume: Volume) -> u8 {
    let scaled = (u64::from(volume.0) * 100 + u64::from(Volume::NORMAL.0) / 2) / u64::from(Volume::NORMAL.0);
    scaled.min(u64::from(u8::MAX)) as u8
}

fn percent_to_volume(percent: u8) -> Volume {
    Volume((u64::from(percent) * u64::from(Volume::NORMAL.0) / 100) as u32)
}

enum QueryResult {
    Found(SinkReading),
    NoSuchSink,
}

fn query(conn: &sys::Connection, name: &str) -> Result<QueryResult, Fault> {
    let (tx, rx) = mpsc::channel();
    let _op = {
        let name = name.to_string();
        conn.context
            .introspect()
            .get_sink_info_by_name(&name, move |result| match result {
                ListResult::Item(info) => {
                    let channels = info.volume.get().len() as u8;
                    if channels == 0 {
                        return;
                    }
                    let _ = tx.send(QueryResult::Found(SinkReading {
                        channels,
                        percent: volume_to_percent(info.volume.avg()),
                        muted: info.mute,
                    }));
                }
                ListResult::Error => {
                    let _ = tx.send(QueryResult::NoSuchSink);
                }
                ListResult::End => {}
            })
    };
    sys::wait_op(&rx, "sink volume info").map_err(|_| Fault {
        status: ExitVolumeStatus::Unavailable,
        native_code: None,
    })
}

fn unavailable() -> Fault {
    Fault {
        status: ExitVolumeStatus::Unavailable,
        native_code: None,
    }
}

/// `true` when the context is connected; every read/write fails closed
/// while the server is unreachable (service restart included).
fn ready(conn: &mut sys::Connection) -> bool {
    conn.mainloop.lock();
    let state = conn.context.get_state();
    conn.mainloop.unlock();
    matches!(state, pulse::context::State::Ready)
}

struct Native {
    conn: sys::Connection,
    name: String,
    changed: Arc<AtomicBool>,
}

pub(super) fn open(output: &str, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Box<dyn Endpoint>, Fault> {
    let name = output
        .strip_prefix(OUTPUT_PREFIX)
        .filter(|name| !name.is_empty() && !name.contains('\0') && name.len() <= 512)
        .ok_or_else(Fault::unavailable)?;
    let mut conn = sys::connect("oink-volume").map_err(|_| unavailable())?;
    let changed = Arc::new(AtomicBool::new(false));
    conn.mainloop.lock();
    conn.context.set_subscribe_callback(Some(Box::new({
        let changed = changed.clone();
        move |facility, _, _| {
            if matches!(facility, Some(Facility::Sink) | Some(Facility::Server)) {
                changed.store(true, Ordering::Release);
                wake();
            }
        }
    })));
    conn.context
        .subscribe(InterestMaskSet::SINK | InterestMaskSet::SERVER, |_| {});
    conn.mainloop.unlock();
    Ok(Box::new(Native {
        conn,
        name: name.to_string(),
        changed,
    }))
}

impl Endpoint for Native {
    fn changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }

    fn read(&mut self) -> Result<Reading, Fault> {
        if !ready(&mut self.conn) {
            return Err(unavailable());
        }
        match query(&self.conn, &self.name)? {
            QueryResult::Found(sink) => Ok(Reading {
                percent: sink.percent,
                muted: sink.muted,
            }),
            QueryResult::NoSuchSink => Err(unavailable()),
        }
    }

    fn write(&mut self, reading: Reading) -> Result<(), Fault> {
        if !ready(&mut self.conn) {
            return Err(unavailable());
        }
        // Re-query the channel count: cvolumes must match the sink layout.
        let channels = match query(&self.conn, &self.name)? {
            QueryResult::Found(sink) => sink.channels,
            QueryResult::NoSuchSink => return Err(unavailable()),
        };
        let mut volumes = ChannelVolumes::default();
        volumes.set(channels, percent_to_volume(reading.percent));
        let (volume_tx, volume_rx) = mpsc::channel();
        let (mute_tx, mute_rx) = mpsc::channel();
        {
            self.conn.mainloop.lock();
            let name = self.name.clone();
            let mut introspect = self.conn.context.introspect();
            let _volume_op = introspect.set_sink_volume_by_name(
                &name,
                &volumes,
                Some(Box::new(move |success| {
                    let _ = volume_tx.send(success);
                })),
            );
            let _mute_op = introspect.set_sink_mute_by_name(
                &name,
                reading.muted,
                Some(Box::new(move |success| {
                    let _ = mute_tx.send(success);
                })),
            );
            self.conn.mainloop.unlock();
        }
        let volume_ok = volume_rx.recv_timeout(sys::CONNECT_TIMEOUT).unwrap_or(false);
        let mute_ok = mute_rx.recv_timeout(sys::CONNECT_TIMEOUT).unwrap_or(false);
        if volume_ok && mute_ok {
            Ok(())
        } else {
            Err(Fault {
                status: ExitVolumeStatus::BackendFailed,
                native_code: None,
            })
        }
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        // Same orderly teardown as `sys::Connection::drop`, which cannot run
        // because Drop only gets `&mut self`.
        self.conn.context.disconnect();
        self.conn.mainloop.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_volume_round_trips_through_the_normal_reference() {
        for percent in [0, 5, 20, 40, 65, 100] {
            let volume = percent_to_volume(percent);
            assert_eq!(volume_to_percent(volume), percent);
        }
        assert_eq!(percent_to_volume(100), Volume::NORMAL);
        assert_eq!(percent_to_volume(0), Volume::MUTED);
        // Over-100% pulse gains stay representable.
        let boosted = Volume(Volume::NORMAL.0 + Volume::NORMAL.0 / 2);
        assert_eq!(volume_to_percent(boosted), 150);
    }
}
