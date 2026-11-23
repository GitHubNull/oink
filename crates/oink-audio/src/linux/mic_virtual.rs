//! Virtual recording source lifecycle: a private null-sink (`oink_mic`) that
//! remote microphone PCM renders into, remapped to a recording source
//! (`oink_input`) consumers can select. Creation is idempotent — a leftover
//! pair from an earlier run is reused, and foreign same-named nodes make the
//! state Ambiguous and are never unloaded by us.
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use libpulse_binding as pulse;
use pulse::callbacks::ListResult;

use crate::linux::sys;
use crate::microphone::{InputDevice, VirtualOutput};

pub(crate) const VIRTUAL_SINK: &str = "oink_mic";
pub(crate) const VIRTUAL_SOURCE: &str = "oink_input";
const OINK_UID: &str = "org.oink.virtual-microphone.v1";
const DESCRIPTION: &str = "Oink Microphone";
/// Module loading is asynchronous on the server; bound the confirmation poll.
const CREATE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct SourceMatch {
    device: InputDevice,
}

fn source_matches(conn: &sys::Connection) -> Result<Vec<SourceMatch>, String> {
    let (tx, rx) = mpsc::channel::<()>();
    let matches: Arc<Mutex<Vec<SourceMatch>>> = Arc::new(Mutex::new(Vec::new()));
    let _op = {
        let matches = matches.clone();
        conn.context
            .introspect()
            .get_source_info_by_name(VIRTUAL_SOURCE, move |result| match result {
                ListResult::Item(info) => {
                    let name = info.name.as_deref().unwrap_or(VIRTUAL_SOURCE);
                    matches.lock().unwrap_or_else(|e| e.into_inner()).push(SourceMatch {
                        device: InputDevice {
                            id: format!("{}{}", crate::linux::INPUT_PREFIX, name),
                            name: info.description.as_deref().unwrap_or(DESCRIPTION).to_string(),
                        },
                    });
                }
                ListResult::End | ListResult::Error => {
                    let _ = tx.send(());
                }
            })
    };
    sys::wait_op(&rx, "virtual source lookup")?;
    let matches = matches.lock().unwrap_or_else(|e| e.into_inner());
    Ok(matches.clone())
}

fn load_module(conn: &sys::Connection, name: &str, argument: &str, what: &str) -> Result<u32, String> {
    let (tx, rx) = mpsc::channel();
    let _op = {
        conn.context.introspect().load_module(name, argument, move |index| {
            let _ = tx.send(index);
        })
    };
    sys::wait_op(&rx, what)
}

fn null_sink_args() -> String {
    format!("sink_name={VIRTUAL_SINK} sink_properties=\"device.description='{DESCRIPTION} Feed' oink.uid={OINK_UID}\"")
}

fn remap_source_args() -> String {
    format!(
        "source_name={VIRTUAL_SOURCE} master={VIRTUAL_SINK}.monitor source_properties=\"device.description='{DESCRIPTION}' oink.uid={OINK_UID}\""
    )
}

/// Idempotent virtual recording source. Returns the render sink the remote
/// microphone feed plays into (`pulse:sink:oink_mic`). A pre-existing foreign
/// `oink_input` is reused, never replaced; two or more make the state
/// Ambiguous so the caller resolves by explicit ID instead of first-match.
pub fn ensure() -> Result<VirtualOutput, String> {
    let conn = sys::connect("oink-mic-virtual")?;
    let existing = source_matches(&conn)?;
    let output = match existing.len() {
        0 => {
            let sink_module = load_module(&conn, "module-null-sink", &null_sink_args(), "null sink module")?;
            let source_module = load_module(
                &conn,
                "module-remap-source",
                &remap_source_args(),
                "remap source module",
            )?;
            // The source appears asynchronously; poll instead of assuming the
            // module ACK implies node creation. On timeout, unload only the
            // modules we just created and fail closed.
            let deadline = Instant::now() + CREATE_TIMEOUT;
            let confirmed = loop {
                match source_matches(&conn) {
                    Ok(found) if !found.is_empty() => break Some(found),
                    Ok(_) if Instant::now() >= deadline => break None,
                    Ok(_) => std::thread::sleep(Duration::from_millis(50)),
                    Err(error) => return Err(error),
                }
            };
            let Some(_found) = confirmed else {
                for index in [sink_module, source_module] {
                    let (tx, rx) = mpsc::channel();
                    let _op = conn.context.introspect().unload_module(index, move |ok| {
                        let _ = tx.send(ok);
                    });
                    let _ = sys::wait_op(&rx, "virtual module unload");
                }
                conn.close();
                return Err("virtual recording source did not appear after module load".into());
            };
            VirtualOutput::Ready {
                render_id: crate::linux::output_id(VIRTUAL_SINK),
                recording_name: DESCRIPTION.into(),
            }
        }
        1 => VirtualOutput::Ready {
            render_id: crate::linux::output_id(VIRTUAL_SINK),
            recording_name: DESCRIPTION.into(),
        },
        _ => VirtualOutput::Ambiguous(existing.into_iter().map(|entry| entry.device).collect()),
    };
    conn.close();
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_arguments_name_the_private_nodes_and_carry_the_uid() {
        let sink = null_sink_args();
        assert!(sink.contains("sink_name=oink_mic"));
        assert!(sink.contains("oink.uid=org.oink.virtual-microphone.v1"));
        let source = remap_source_args();
        assert!(source.contains("source_name=oink_input"));
        assert!(source.contains("master=oink_mic.monitor"));
    }
}
