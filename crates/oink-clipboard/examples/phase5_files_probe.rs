//! Phase 5 file/privacy/delayed-delivery probe. JSON lines only; payloads are
//! reported as lengths and SHA-256 digests, never as text.
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("phase5_files_probe is Linux/X11 only");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn main() {
    imp::run();
}

#[cfg(target_os = "linux")]
mod imp {
    use oink_clipboard::content::{self, Provider};
    use oink_clipboard::model::{
        ClipId, ContentItem, ContentSize, Inventory, ItemId, Medium, NativeFormat, NodeKind, OfferId, Representation,
        RepresentationData, RepresentationId, StandardFormat,
    };
    use oink_clipboard::{ClipData, FileContents, FileEntry, NativeSpec, Sensitivity, platform_backend};
    use oink_foundation::DeviceId;
    use oink_foundation::clipboard::ClipPlatform;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::io::{self, Read, Write};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    const TEXT: &str = "oink-phase5-text";

    pub(crate) fn run() {
        let mut command = String::new();
        let mut opts = HashMap::new();
        let mut paths = Vec::new();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let Some(name) = arg.strip_prefix("--") else {
                if command.is_empty() {
                    command = arg;
                    continue;
                }
                fail("unexpected argument");
            };
            let value = if name == "path" {
                let Some(value) = args.next() else {
                    fail("missing option value");
                };
                paths.push(PathBuf::from(value));
                continue;
            } else {
                match name {
                    "save" | "hold-ms" | "delay-ms" | "level" | "mode" => {
                        let Some(value) = args.next() else {
                            fail("missing option value");
                        };
                        value
                    }
                    _ => fail("unknown option"),
                }
            };
            opts.insert(name.to_owned(), value);
        }
        if let Err(error) = dispatch(&command, &opts, &paths) {
            emit(json!({
                "event": "error",
                "command": command,
                "kind": format!("{:?}", error.kind()),
                "reason": error.to_string(),
            }));
            std::process::exit(1);
        }
    }

    fn fail(reason: &str) -> ! {
        emit(json!({"event": "error", "reason": reason}));
        std::process::exit(2);
    }

    fn dispatch(command: &str, opts: &HashMap<String, String>, paths: &[PathBuf]) -> io::Result<()> {
        match command {
            "capture-files" => capture_files(opts),
            "write-files" => write_files(opts, paths),
            "write-sensitive" => write_sensitive(opts),
            "write-guarded" => write_guarded(opts),
            "write-virtual" => write_virtual(opts, paths),
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

    fn hold(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn backend() -> io::Result<Box<dyn oink_clipboard::ClipboardBackend>> {
        platform_backend().map_err(|error| io::Error::other(format!("{error:?}")))
    }

    fn kind_label(kind: &NodeKind) -> &'static str {
        match kind {
            NodeKind::File => "file",
            NodeKind::Directory => "dir",
            NodeKind::Symlink(_) => "symlink",
        }
    }

    fn size_label(size: ContentSize) -> String {
        match size {
            ContentSize::Known(n) => format!("{n}"),
            ContentSize::Unknown => "unknown".into(),
        }
    }

    fn capture_files(opts: &HashMap<String, String>) -> io::Result<()> {
        let backend = backend()?;
        let captured = backend
            .capture(Path::new(""))
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        let provider = captured
            .content
            .as_ref()
            .ok_or_else(|| io::Error::other("missing provider"))?;
        let inventory = provider.inventory();
        let roots: Vec<_> = inventory
            .roots
            .iter()
            .map(|node| {
                json!({
                    "kind": kind_label(&node.kind),
                    "size": size_label(node.size),
                    "name_sha256": digest(node.name.as_bytes()),
                })
            })
            .collect();
        let file_reps: Vec<_> = inventory
            .items
            .iter()
            .filter(|item| item.file.is_some())
            .flat_map(|item| &item.representations)
            .map(|rep| match &rep.format {
                NativeFormat::LinuxAtom(name) => format!("linux:{name}"),
                _ => "other".into(),
            })
            .collect();
        emit(json!({
            "event": "capture",
            "items": inventory.items.len(),
            "snapshot_items": captured.snapshot.items.len(),
            "sensitivity": format!("{:?}", captured.snapshot.sensitivity),
            "roots": roots,
            "file_items": inventory.items.iter().filter(|item| item.file.is_some()).count(),
            "file_reps": file_reps,
        }));
        // Delayed delivery: no namespace or payload read happens before this point.
        hold(number(opts, "delay-ms", 0)?);
        if let Some(namespace) = provider.namespace() {
            for node in namespace.roots() {
                if matches!(node.kind, NodeKind::Directory) {
                    let page = namespace.list(node.id, None)?;
                    let files: Vec<_> = page
                        .entries
                        .iter()
                        .filter(|entry| matches!(entry.kind, NodeKind::File))
                        .map(|entry| {
                            json!({
                                "kind": "file",
                                "size": size_label(entry.size),
                                "name_sha256": digest(entry.name.as_bytes()),
                            })
                        })
                        .collect();
                    emit(json!({
                        "event": "namespace-list",
                        "entries": page.entries.len(),
                        "version": page.version,
                        "children": files,
                    }));
                    for entry in &page.entries {
                        if matches!(entry.kind, NodeKind::File) {
                            read_node(namespace.as_ref(), entry.id, "child")?;
                        }
                    }
                } else if matches!(node.kind, NodeKind::File) {
                    read_node(namespace.as_ref(), node.id, "root")?;
                }
            }
        }
        if let Some(directory) = opts.get("save") {
            let saved =
                oink_clipboard::paste_file::save(provider.as_ref(), Path::new(directory), "zh", "phase5", false)?;
            let mut digests = Vec::new();
            for path in &saved {
                walk(path, &mut digests)?;
            }
            digests.sort();
            emit(json!({
                "event": "save",
                "roots": saved.len(),
                "files": digests.len(),
                "digests": digests,
            }));
        }
        emit(json!({"event": "exit"}));
        Ok(())
    }

    fn read_node(
        namespace: &dyn content::namespace::Namespace,
        id: oink_clipboard::model::NodeId,
        role: &str,
    ) -> io::Result<()> {
        let node = namespace.select(id)?;
        let mut reader = namespace.open(node.id, node.version, 0)?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        emit(json!({
            "event": "namespace-read",
            "role": role,
            "ok": true,
            "length": bytes.len(),
            "sha256": digest(&bytes),
        }));
        Ok(())
    }

    fn walk(path: &Path, digests: &mut Vec<String>) -> io::Result<()> {
        if path.is_dir() {
            for entry in std::fs::read_dir(path)? {
                walk(&entry?.path(), digests)?;
            }
        } else {
            digests.push(digest(&std::fs::read(path)?));
        }
        Ok(())
    }

    fn entry(path: &Path) -> io::Result<FileEntry> {
        let metadata = std::fs::metadata(path)?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "file name"))?
            .to_owned();
        Ok(FileEntry {
            path: name,
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            modified_ms: None,
            local_path: Some(path.to_path_buf()),
        })
    }

    fn write_files(opts: &HashMap<String, String>, paths: &[PathBuf]) -> io::Result<()> {
        let entries: Vec<_> = paths.iter().map(|path| entry(path)).collect::<io::Result<_>>()?;
        let backend = backend()?;
        let count = backend
            .write(&[ClipData::Files(entries)], Sensitivity::Normal)
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        emit(json!({
            "event": "write",
            "count": count,
            "change_count": backend.change_count(),
            "entries": paths.len(),
        }));
        hold(number(opts, "hold-ms", 0)?);
        emit(json!({"event": "exit", "count": backend.change_count()}));
        Ok(())
    }

    fn write_sensitive(opts: &HashMap<String, String>) -> io::Result<()> {
        let sensitivity = match opts.get("level").map(String::as_str) {
            Some("transient") => Sensitivity::Transient,
            None | Some("concealed") => Sensitivity::Concealed,
            Some(other) => return Err(io::Error::new(io::ErrorKind::InvalidInput, other)),
        };
        let backend = backend()?;
        let count = backend
            .write(&[ClipData::Text(TEXT.to_owned())], sensitivity)
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        emit(json!({
            "event": "write",
            "count": count,
            "sensitivity": format!("{sensitivity:?}"),
            "length": TEXT.len(),
        }));
        hold(number(opts, "hold-ms", 0)?);
        emit(json!({"event": "exit"}));
        Ok(())
    }

    fn spec(bytes: Vec<u8>) -> NativeSpec {
        let rep = Representation {
            id: RepresentationId(1),
            format: NativeFormat::LinuxAtom("UTF8_STRING".into()),
            medium: Medium::Bytes,
            aspect: 1,
            index: -1,
            standard: Some(StandardFormat::Text),
            data: RepresentationData::Reader {
                size: ContentSize::Unknown,
            },
        };
        let inventory = Inventory {
            id: OfferId {
                clip: ClipId {
                    origin: DeviceId([0; 32]),
                    seq: 1,
                },
                generation: 1,
            },
            platform: ClipPlatform::Linux,
            items: vec![ContentItem {
                id: ItemId(0),
                virtual_files: false,
                file: None,
                representations: vec![rep],
            }],
            roots: Vec::new(),
        };
        NativeSpec {
            inventory: inventory.clone(),
            contents: Arc::new(Local { inventory, bytes }),
            files: Vec::new(),
            file_contents: None,
            sensitivity: Sensitivity::Normal,
        }
    }

    struct Local {
        inventory: Inventory,
        bytes: Vec<u8>,
    }

    impl Provider for Local {
        fn inventory(&self) -> Inventory {
            self.inventory.clone()
        }
        fn open(&self, _: RepresentationId) -> io::Result<oink_clipboard::FileReader> {
            Ok(Box::new(io::Cursor::new(self.bytes.clone())))
        }
        fn translator(&self) -> Arc<dyn content::Translator> {
            Arc::new(PassThrough)
        }
    }

    struct PassThrough;

    impl content::Translator for PassThrough {
        fn standard(&self, _: &Representation, _: &[u8]) -> io::Result<Vec<ClipData>> {
            Err(content::unavailable(oink_clipboard::model::Unavailable::Unsupported))
        }
    }

    fn write_guarded(opts: &HashMap<String, String>) -> io::Result<()> {
        let backend = backend()?;
        let refused: content::ReaderGuard = Arc::new(|| Err(io::Error::new(io::ErrorKind::Interrupted, "probe-guard")));
        let allowed: content::ReaderGuard = Arc::new(|| Ok(()));
        let result = match opts.get("mode").map(String::as_str) {
            Some("guard-fail") => backend
                .write_native_guarded(spec(TEXT.as_bytes().to_vec()), refused)
                .map(|count| (count, false)),
            Some("cancel-fail") => backend
                .write_native_cancellable(spec(TEXT.as_bytes().to_vec()), allowed, refused)
                .map(|count| (count, false)),
            None | Some("pass") => backend
                .write_native_guarded(spec(TEXT.as_bytes().to_vec()), allowed)
                .map(|count| (count, true)),
            Some(other) => return Err(io::Error::new(io::ErrorKind::InvalidInput, other)),
        };
        match result {
            Ok((count, published)) => {
                emit(json!({
                    "event": "result",
                    "ok": true,
                    "published": published,
                    "count": count,
                    "length": TEXT.len(),
                    "sha256": digest(TEXT.as_bytes()),
                }));
            }
            Err(error) => {
                emit(json!({"event": "result", "ok": false, "kind": format!("{error:?}")}));
            }
        }
        hold(number(opts, "hold-ms", 0)?);
        emit(json!({"event": "exit"}));
        Ok(())
    }

    fn write_virtual(opts: &HashMap<String, String>, paths: &[PathBuf]) -> io::Result<()> {
        let entries: Vec<_> = paths.iter().map(|path| entry(path)).collect::<io::Result<_>>()?;
        let contents = Arc::new(LocalContents { paths: paths.to_vec() });
        let backend = backend()?;
        let count = backend
            .write_virtual_files(&entries, &[], contents, Sensitivity::Normal)
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        emit(json!({
            "event": "write",
            "count": count,
            "change_count": backend.change_count(),
            "entries": paths.len(),
        }));
        hold(number(opts, "hold-ms", 0)?);
        emit(json!({"event": "exit"}));
        Ok(())
    }

    struct LocalContents {
        paths: Vec<PathBuf>,
    }

    impl FileContents for LocalContents {
        fn open(&self, index: usize) -> io::Result<Box<dyn Read + Send>> {
            let path = self
                .paths
                .get(index)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "index"))?;
            Ok(Box::new(std::fs::File::open(path)?))
        }
    }
}
