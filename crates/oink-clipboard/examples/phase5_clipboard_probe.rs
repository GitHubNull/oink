//! Phase 5 X11 clipboard probe. Emits one JSON object per line; payload bytes
//! are reported only as lengths and SHA-256 digests, never as text.
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("phase5_clipboard_probe is Linux/X11 only");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn main() {
    imp::run();
}

#[cfg(target_os = "linux")]
mod imp {
    use oink_clipboard::content;
    use oink_clipboard::model::{NativeFormat, StandardFormat};
    use oink_clipboard::{ClipData, Sensitivity, platform_backend};
    use oink_foundation::clipboard::ClipPlatform;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::io::{self, Read, Write};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    const TEXT: &str = "oink-phase5-text";
    const HTML: &str = "<p>oink-phase5-html</p>";
    const RTF: &[u8] = b"{\\rtf1 oink-phase5}";
    const PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R', 0, 0, 0, 1, 0, 0, 0, 1,
        8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0a, b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x63, 0, 0, 0, 0, 2,
        0, 1, 0x0d, 0x0a, 0x2d, 0xb4, 0, 0, 0, 0, 0x49, b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
    ];

    pub(crate) fn run() {
        let mut command = String::new();
        let mut opts = HashMap::new();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let Some(name) = arg.strip_prefix("--") else {
                if command.is_empty() {
                    command = arg;
                    continue;
                }
                emit(json!({"event": "error", "reason": "unexpected argument"}));
                std::process::exit(2);
            };
            let value = match name {
                "watch" | "read" | "translate" => "1".to_owned(),
                "size" | "hold-ms" | "seconds" | "write-size" | "delay-ms" => {
                    let Some(value) = args.next() else {
                        emit(json!({"event": "error", "reason": "missing option value", "option": name}));
                        std::process::exit(2);
                    };
                    value
                }
                _ => {
                    emit(json!({"event": "error", "reason": "unknown option", "option": name}));
                    std::process::exit(2);
                }
            };
            opts.insert(name.to_owned(), value);
        }
        if let Err(error) = dispatch(&command, &opts) {
            emit(json!({
                "event": "error",
                "command": command,
                "kind": format!("{:?}", error.kind()),
                "reason": error.to_string(),
            }));
            std::process::exit(1);
        }
    }

    fn dispatch(command: &str, opts: &HashMap<String, String>) -> io::Result<()> {
        match command {
            "key" => key(),
            "write-text" | "serve" => write_text(opts),
            "write-multi" => write_multi(opts),
            "watch" => watch(opts),
            "capture" => capture(opts),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown command {other}"),
            )),
        }
    }

    fn number(opts: &HashMap<String, String>, name: &str, default: u64) -> io::Result<u64> {
        match opts.get(name) {
            None => Ok(default),
            Some(text) => text
                .parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, name.to_string())),
        }
    }

    fn emit(value: serde_json::Value) {
        println!("{value}");
        io::stdout().flush().ok();
    }

    fn digest(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn pattern(len: usize) -> String {
        (0..len).map(|i| char::from(b'a' + (i % 26) as u8)).collect()
    }

    fn hold(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn key() -> io::Result<()> {
        let backend = backend()?;
        let key = backend.clipboard_key()?;
        emit(json!({"event": "key", "key": digest(&key)}));
        Ok(())
    }

    fn backend() -> io::Result<Box<dyn oink_clipboard::ClipboardBackend>> {
        platform_backend().map_err(|error| io::Error::other(format!("{error:?}")))
    }

    fn write_text(opts: &HashMap<String, String>) -> io::Result<()> {
        let text = match opts.get("size") {
            Some(_) => pattern(number(opts, "size", TEXT.len() as u64)? as usize),
            None => TEXT.to_owned(),
        };
        let backend = backend()?;
        let count = backend
            .write(&[ClipData::Text(text.clone())], Sensitivity::Normal)
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        emit(json!({
            "event": "write",
            "count": count,
            "length": text.len(),
            "sha256": digest(text.as_bytes()),
            "change_count": backend.change_count(),
        }));
        hold(number(opts, "hold-ms", 0)?);
        emit(json!({"event": "exit", "count": backend.change_count()}));
        Ok(())
    }

    fn write_multi(opts: &HashMap<String, String>) -> io::Result<()> {
        let items = vec![
            ClipData::Text(TEXT.to_owned()),
            ClipData::Html(HTML.to_owned()),
            ClipData::Rtf(RTF.to_vec()),
            ClipData::Png(PNG.to_vec()),
        ];
        let backend = backend()?;
        let count = backend
            .write(&items, Sensitivity::Normal)
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        emit(json!({
            "event": "write",
            "count": count,
            "change_count": backend.change_count(),
            "text": {"length": TEXT.len(), "sha256": digest(TEXT.as_bytes())},
            "html": {"length": HTML.len(), "sha256": digest(HTML.as_bytes())},
            "rtf": {"length": RTF.len(), "sha256": digest(RTF)},
            "png": {"length": PNG.len(), "sha256": digest(PNG)},
        }));
        hold(number(opts, "hold-ms", 0)?);
        emit(json!({"event": "exit", "count": backend.change_count()}));
        Ok(())
    }

    fn watch(opts: &HashMap<String, String>) -> io::Result<()> {
        let backend = backend()?;
        let events = Arc::new(AtomicU64::new(0));
        let counter = events.clone();
        backend
            .watch(Arc::new(move || {
                let seq = counter.fetch_add(1, Ordering::SeqCst) + 1;
                emit(json!({"event": "watch-change", "seq": seq}));
            }))
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        emit(json!({"event": "watch-ready"}));
        if let Some(size) = opts.get("write-size") {
            let len: usize = size
                .parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "write-size"))?;
            let count = backend
                .write(&[ClipData::Text(pattern(len))], Sensitivity::Normal)
                .map_err(|error| io::Error::other(format!("{error:?}")))?;
            emit(json!({"event": "write", "count": count, "length": len}));
        }
        std::thread::sleep(Duration::from_millis(number(opts, "seconds", 3)? * 1000));
        emit(json!({
            "event": "watch-done",
            "events": events.load(Ordering::SeqCst),
            "count": backend.change_count(),
        }));
        Ok(())
    }

    fn standard_label(standard: Option<StandardFormat>) -> &'static str {
        match standard {
            Some(StandardFormat::Text) => "text",
            Some(StandardFormat::Html) => "html",
            Some(StandardFormat::Rtf) => "rtf",
            Some(StandardFormat::Png) => "png",
            Some(StandardFormat::Privacy) => "privacy",
            Some(StandardFormat::Url) => "url",
            None => "none",
        }
    }

    fn format_label(format: &NativeFormat) -> String {
        match format {
            NativeFormat::LinuxAtom(name) => format!("linux:{name}"),
            NativeFormat::Mac(name) => format!("mac:{name}"),
            NativeFormat::WindowsRegistered(name) => format!("win:{name}"),
            NativeFormat::WindowsPredefined(id) => format!("win-cf:{id}"),
            NativeFormat::Unknown(name) => format!("unknown:{name}"),
        }
    }

    fn kind_label(data: &oink_clipboard::model::RepresentationData) -> &'static str {
        use oink_clipboard::model::RepresentationData;
        match data {
            RepresentationData::Reader { .. } => "reader",
            RepresentationData::Inline(_) => "inline",
            RepresentationData::File(_) => "file",
            RepresentationData::Unavailable(_) => "unavailable",
            RepresentationData::Promise(..) => "promise",
        }
    }

    fn capture(opts: &HashMap<String, String>) -> io::Result<()> {
        let backend = backend()?;
        let captured = backend
            .capture(Path::new(""))
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        let snapshot = &captured.snapshot;
        let inventory = captured.content.as_ref().map(|provider| provider.inventory());
        let mut items = Vec::new();
        if let Some(inventory) = &inventory {
            for (item_index, item) in inventory.items.iter().enumerate() {
                let reps: Vec<_> = item
                    .representations
                    .iter()
                    .map(|rep| {
                        json!({
                            "id": rep.id.0,
                            "format": format_label(&rep.format),
                            "standard": standard_label(rep.standard),
                            "data": kind_label(&rep.data),
                        })
                    })
                    .collect();
                items.push(json!({"index": item_index, "reps": reps}));
            }
        }
        emit(json!({
            "event": "capture",
            "items": inventory.as_ref().map(|i| i.items.len()).unwrap_or(0),
            "platform": format!("{:?}", inventory.as_ref().map(|i| i.platform).unwrap_or(ClipPlatform::Unknown)),
            "sensitivity": format!("{:?}", snapshot.sensitivity),
            "snapshot_items": snapshot.items.len(),
            "change_count": snapshot.change_count,
            "representations": items,
        }));
        hold(number(opts, "delay-ms", 0)?);
        if opts.contains_key("read") || opts.contains_key("translate") {
            read_representations(&captured, opts.contains_key("read"), opts.contains_key("translate"));
        }
        emit(json!({"event": "exit"}));
        Ok(())
    }

    fn read_representations(captured: &oink_clipboard::CapturedClipboard, raw: bool, translate: bool) {
        let Some(provider) = captured.content.as_ref() else {
            emit(json!({"event": "read", "ok": false, "reason": "missing-provider"}));
            return;
        };
        let inventory = provider.inventory();
        for item in &inventory.items {
            for rep in &item.representations {
                if raw {
                    emit(read_line(rep.id.0, "raw", |id| provider.open(id)));
                }
                if translate {
                    emit(read_line(rep.id.0, "portable", |id| {
                        content::open_for(provider.as_ref(), id, ClipPlatform::Windows)
                    }));
                }
            }
        }
    }

    fn read_line(
        id: u32,
        channel: &str,
        open: impl FnOnce(oink_clipboard::model::RepresentationId) -> io::Result<oink_clipboard::FileReader>,
    ) -> serde_json::Value {
        let result = open(oink_clipboard::model::RepresentationId(id)).and_then(|mut reader| {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        match result {
            Ok(bytes) => json!({
                "event": "read",
                "id": id,
                "channel": channel,
                "ok": true,
                "length": bytes.len(),
                "sha256": digest(&bytes),
            }),
            Err(error) => json!({
                "event": "read",
                "id": id,
                "channel": channel,
                "ok": false,
                "kind": format!("{:?}", error.kind()),
            }),
        }
    }
}
