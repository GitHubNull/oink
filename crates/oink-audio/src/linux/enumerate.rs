//! One-shot enumeration over a short-lived connection. Enumeration is explicit
//! (never periodic) and never probes stream formats; latency is reported only
//! by an opened playback stream.
use std::sync::{Arc, Mutex, mpsc};

use libpulse_binding as pulse;
use pulse::callbacks::ListResult;
use pulse::context::introspect::SinkInfo;

use crate::OutputDevice;
use crate::linux::{is_private, output_id, sys};

#[derive(Clone, Debug)]
pub(crate) struct SinkSummary {
    pub(crate) name: String,
    pub(crate) description: String,
}

fn sink_summary(info: &SinkInfo<'_>) -> Option<SinkSummary> {
    let name = info.name.as_deref()?;
    if is_private(name) {
        return None;
    }
    Some(SinkSummary {
        name: name.to_string(),
        description: info.description.as_deref().unwrap_or(name).to_string(),
    })
}

/// The server may answer with the symbolic `@DEFAULT_SINK@`/`@DEFAULT_SOURCE@`
/// placeholder (observed on a minimal pipewire-pulse 1.0.5 without session
/// manager policy). That is "no concrete default reported", never a guess:
/// enumeration marks nothing default and callers see an explicit error.
fn concrete(name: &Option<String>) -> Option<&str> {
    name.as_deref().filter(|name| !name.starts_with('@'))
}

/// Enumerate playback outputs. Private `oink_*` sinks (the virtual microphone
/// render node) are excluded; the default flag follows the server's
/// `default_sink`, not a cached index.
pub fn outputs() -> Result<Vec<OutputDevice>, String> {
    let conn = sys::connect("oink-enumerate")?;
    let defaults = query_server_defaults(&conn)?;
    let sinks = sink_summaries(&conn)?;
    conn.close();
    let default_sink = concrete(&defaults.0);
    Ok(sinks
        .into_iter()
        .map(|sink| OutputDevice {
            is_default: default_sink == Some(sink.name.as_str()),
            id: output_id(&sink.name),
            name: sink.description,
        })
        .collect())
}

/// Concrete default sink name, if the server reports one (symbolic
/// placeholders are not a name and return None).
pub(crate) fn default_sink_name() -> Option<String> {
    let conn = sys::connect("oink-enumerate").ok()?;
    let defaults = query_server_defaults(&conn).ok()?;
    let name = concrete(&defaults.0).map(str::to_string);
    conn.close();
    name
}

/// The default output's device sample rate, without opening any stream.
/// The default sink may be one of our private nodes (e.g. the virtual
/// microphone render sink), so this lookup does not apply the public
/// enumeration filter.
pub(crate) fn default_sink_sample_rate() -> Result<u32, String> {
    let conn = sys::connect("oink-enumerate")?;
    let defaults = query_server_defaults(&conn)?;
    let name = concrete(&defaults.0).ok_or("pulse server reports no concrete default sink")?;
    let rate = sink_rate_by_name(&conn, name);
    conn.close();
    rate
}

fn sink_rate_by_name(conn: &sys::Connection, name: &str) -> Result<u32, String> {
    let (tx, rx) = mpsc::channel();
    let _op = {
        let name = name.to_string();
        conn.context
            .introspect()
            .get_sink_info_by_name(&name, move |result| match result {
                ListResult::Item(info) => {
                    let _ = tx.send(info.sample_spec.rate);
                }
                ListResult::End | ListResult::Error => {}
            })
    };
    sys::wait_op(&rx, "default sink info")
}

fn query_server_defaults(conn: &sys::Connection) -> Result<(Option<String>, Option<String>), String> {
    let (tx, rx) = mpsc::channel();
    let _op = {
        conn.context.introspect().get_server_info(move |info| {
            let _ = tx.send((
                info.default_sink_name.as_deref().map(str::to_string),
                info.default_source_name.as_deref().map(str::to_string),
            ));
        })
    };
    sys::wait_op(&rx, "server info")
}

fn sink_summaries(conn: &sys::Connection) -> Result<Vec<SinkSummary>, String> {
    let (tx, rx) = mpsc::channel::<()>();
    let items: Arc<Mutex<Vec<SinkSummary>>> = Arc::new(Mutex::new(Vec::new()));
    let _op = {
        let items = items.clone();
        conn.context
            .introspect()
            .get_sink_info_list(move |result| match result {
                ListResult::Item(info) => {
                    if let Some(summary) = sink_summary(info) {
                        items.lock().unwrap_or_else(|e| e.into_inner()).push(summary);
                    }
                }
                ListResult::End | ListResult::Error => {
                    let _ = tx.send(());
                }
            })
    };
    sys::wait_op(&rx, "sink list")?;
    let items = items.lock().unwrap_or_else(|e| e.into_inner());
    Ok(items.clone())
}

/// Whether a source with this exact name currently exists. `None` means the
/// lookup itself failed; a definitive answer is never guessed.
pub(crate) fn monitor_exists(name: &str) -> Option<bool> {
    let conn = sys::connect("oink-enumerate").ok()?;
    let (tx, rx) = mpsc::channel();
    let _op = {
        let name = name.to_string();
        conn.context
            .introspect()
            .get_source_info_by_name(&name, move |result| match result {
                ListResult::Item(_) => {
                    let _ = tx.send(true);
                }
                ListResult::Error => {
                    let _ = tx.send(false);
                }
                ListResult::End => {}
            })
    };
    let found = sys::wait_op(&rx, "source info").ok()?;
    conn.close();
    Some(found)
}
