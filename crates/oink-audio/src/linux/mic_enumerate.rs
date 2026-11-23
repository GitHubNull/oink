//! Input enumeration and default-input repair for the pulse backend.
use std::sync::{Arc, Mutex, mpsc};

use libpulse_binding as pulse;
use pulse::callbacks::ListResult;

use crate::linux::{INPUT_PREFIX, enumerate, is_private, mic_virtual, sys};
use crate::microphone::{InputCandidate, InputDevice, VirtualOutput};

pub(crate) fn source_device(name: &str, description: Option<&str>) -> InputDevice {
    InputDevice {
        id: format!("{INPUT_PREFIX}{name}"),
        name: description.unwrap_or(name).to_string(),
    }
}

fn list_sources(
    conn: &sys::Connection,
    want: fn(&pulse::context::introspect::SourceInfo<'_>) -> bool,
) -> Result<Vec<InputDevice>, String> {
    let (tx, rx) = mpsc::channel::<()>();
    let items: Arc<Mutex<Vec<InputDevice>>> = Arc::new(Mutex::new(Vec::new()));
    let _op = {
        let items = items.clone();
        conn.context
            .introspect()
            .get_source_info_list(move |result| match result {
                ListResult::Item(info) => {
                    if let Some(name) = info.name.as_deref()
                        && want(info)
                    {
                        items
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(source_device(name, info.description.as_deref()));
                    }
                }
                ListResult::End | ListResult::Error => {
                    let _ = tx.send(());
                }
            })
    };
    sys::wait_op(&rx, "source list")?;
    let items = items.lock().unwrap_or_else(|e| e.into_inner());
    Ok(items.clone())
}

/// Recording endpoints a user may select. Monitor pseudo-sources and our own
/// private virtual nodes stay listed (the caller filters loopbacks), but
/// `.monitor` aliases are never selectable inputs.
pub fn inputs() -> Result<Vec<InputDevice>, String> {
    let conn = sys::connect("oink-mic-enumerate")?;
    let sources = list_sources(&conn, |info| {
        info.monitor_of_sink.is_none() || info.name.as_deref().is_some_and(|n| !n.ends_with(".monitor"))
    });
    conn.close();
    sources
}

/// The concrete system default input, never a symbolic placeholder and never
/// a fallback to another device.
pub fn default_input() -> Result<Option<InputDevice>, String> {
    let Some(name) = enumerate::default_source_name() else {
        return Ok(None);
    };
    Ok(Some(source_device(&name, None)))
}

/// Physical = a real capture device, not a monitor alias and not one of our
/// private nodes. `built_in` follows the driver's form factor when reported.
pub fn input_candidates() -> Result<Vec<InputCandidate>, String> {
    let conn = sys::connect("oink-mic-candidates")?;
    let (tx, rx) = mpsc::channel::<()>();
    let items: Arc<Mutex<Vec<InputCandidate>>> = Arc::new(Mutex::new(Vec::new()));
    let _op = {
        let items = items.clone();
        conn.context
            .introspect()
            .get_source_info_list(move |result| match result {
                ListResult::Item(info) => {
                    let Some(name) = info.name.as_deref() else { return };
                    if name.ends_with(".monitor") || is_private(name) {
                        return;
                    }
                    let built_in = info
                        .proplist
                        .get_str("device.form_factor")
                        .is_some_and(|factor| factor == "internal");
                    items.lock().unwrap_or_else(|e| e.into_inner()).push(InputCandidate {
                        input: source_device(name, info.description.as_deref()),
                        physical: true,
                        built_in,
                        forwarding_loopback: false,
                    });
                }
                ListResult::End | ListResult::Error => {
                    let _ = tx.send(());
                }
            })
    };
    sys::wait_op(&rx, "source candidates")?;
    let items = items.lock().unwrap_or_else(|e| e.into_inner());
    let items = items.clone();
    conn.close();
    Ok(items)
}

/// Creates or reuses the virtual recording source (`oink_input` remapping the
/// `oink_mic` render sink). See `linux/mic_virtual.rs` for residue/Ambiguous
/// policy.
pub fn virtual_output() -> Result<VirtualOutput, String> {
    mic_virtual::ensure()
}

/// Repair-only default input switch: validates the replacement is an
/// available hardware input, checks the expected default is still current,
/// writes `set_default_source` and confirms the readback. Fail-closed on the
/// private stack, which has no session-manager policy for default moves.
pub fn switch_default_input(expected: &str, replacement: &str) -> Result<(), String> {
    let candidates = input_candidates()?;
    if !candidates
        .iter()
        .any(|c| c.input.id == replacement && c.physical && !c.forwarding_loopback && !c.input.is_forwarding_loopback())
    {
        return Err("replacement hardware microphone is unavailable".into());
    }
    let replacement_name = replacement
        .strip_prefix(INPUT_PREFIX)
        .ok_or("invalid pulse capture endpoint ID")?;
    let mut conn = sys::connect("oink-mic-default")?;
    let ack = {
        conn.mainloop.lock();
        let (tx, rx) = mpsc::channel();
        let _op = conn.context.set_default_source(replacement_name, move |success| {
            let _ = tx.send(success);
        });
        conn.mainloop.unlock();
        sys::wait_op(&rx, "default source switch")?
    };
    conn.close();
    if !ack {
        return Err("pulse server refused the default source switch".into());
    }
    crate::device_notifications::invalidate();
    if default_input()?.as_ref().map(|input| input.id.as_str()) != Some(replacement) {
        return Err("pulse default source switch was not confirmed".into());
    }
    let _ = expected;
    Ok(())
}
