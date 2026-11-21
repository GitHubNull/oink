//! Phase 5 FUSE probe: mounts a scripted fake Namespace through the product
//! file_share bridge and verifies the mounted tree by reading it back.
//! Emits one JSON object per line; payloads are lengths and SHA-256 digests.
//!
//!   phase5_fuse_probe [--big N] [--stale|--offline|--self-check]
//!
//! --self-check never mounts: it reads the fake through the Namespace API so
//! the fixture and digests are validated even where mounting is blocked.
//! In the agent sandbox (NoNewPrivs=1) mounting is impossible: the probe
//! emits {"event":"blocked","reason":...} and exits 0, like the C probe.
//! In a normal user terminal it mounts, walks, reads and emits evidence.
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("phase5_fuse_probe is Linux only");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn main() {
    imp::run();
}

#[cfg(target_os = "linux")]
mod imp {
    use oink_clipboard::FileReader;
    use oink_clipboard::content::namespace::Namespace;
    use oink_clipboard::model::{DirectoryCursor, DirectoryPage};
    use oink_foundation::content::{ContentSize, FileNode, NodeId, NodeKind};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::io::{self, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    const HELLO: &str = "oink phase5 fuse hello";
    const LINK: &str = "hello.txt";
    const ESCAPE: &str = "/etc/passwd";

    pub(crate) fn run() {
        let mut args = std::env::args().skip(1);
        let mut big_kib = 64usize;
        let mut mode = Mode::Mount;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--big" => {
                    let Some(value) = args.next() else {
                        emit(json!({"event": "error", "reason": "missing --big value"}));
                        std::process::exit(2);
                    };
                    big_kib = value.parse().unwrap_or(64);
                }
                "--stale" => mode = Mode::Stale,
                "--offline" => mode = Mode::Offline,
                "--self-check" => mode = Mode::SelfCheck,
                other => {
                    emit(json!({"event": "error", "reason": "unknown argument", "arg": other}));
                    std::process::exit(2);
                }
            }
        }
        let fake = Arc::new(Fake::new(big_kib, mode == Mode::Stale, mode == Mode::Offline));
        if mode == Mode::SelfCheck {
            self_check(&fake);
            return;
        }
        if let Some(reason) = oink_clipboard::file_share::blocked_reason() {
            emit(json!({"event": "blocked", "reason": reason}));
            return;
        }
        let share = match oink_clipboard::file_share::mount(fake.clone()) {
            Ok(Some(share)) => share,
            Ok(None) => {
                emit(json!({"event": "blocked", "reason": "mount returned no path"}));
                return;
            }
            Err(error) => {
                emit(json!({"event": "error", "kind": format!("{:?}", error.kind()), "reason": error.to_string()}));
                std::process::exit(1);
            }
        };
        let root = share.path().to_path_buf();
        emit(json!({"event": "mounted", "path": root.display().to_string()}));
        let mut failures = walk(&root, 0);
        failures += verify_file(&root.join("hello.txt"), HELLO.as_bytes(), &mode);
        failures += verify_file(&root.join("folder/a.txt"), &a_text(), &mode);
        let big = pattern(big_kib * 1024);
        failures += verify_file(&root.join("big.bin"), &big, &mode);
        failures += check_link(&root.join("link"), LINK, true, &mode);
        failures += check_link(&root.join("escape"), ESCAPE, false, &mode);
        emit(
            json!({"event": "counters", "lists": fake.lists.load(Ordering::Relaxed), "opens": fake.opens.load(Ordering::Relaxed)}),
        );
        emit(json!({"event": "done", "failures": failures}));
        drop(share);
        if !root.exists() {
            emit(json!({"event": "unmounted", "path": root.display().to_string()}));
        } else {
            emit(json!({"event": "unmount-leftover", "path": root.display().to_string()}));
            std::process::exit(1);
        }
        std::process::exit(i32::from(failures > 0));
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Mount,
        Stale,
        Offline,
        SelfCheck,
    }

    fn expected_failure(mode: &Mode) -> bool {
        matches!(mode, Mode::Stale | Mode::Offline)
    }

    fn self_check(fake: &Arc<Fake>) {
        // Read straight through the Namespace API: proves the fixture,
        // pagination and digests without needing mount privileges.
        let roots = fake.roots();
        emit(json!({"event": "self-check", "roots": roots.len()}));
        let deep = roots.iter().find(|n| n.name == "deep").expect("deep root");
        let mut cursor = None;
        let mut seen = 0u64;
        loop {
            let page = fake.list(deep.id, cursor).expect("deep page");
            seen += page.entries.len() as u64;
            match page.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        let lists = fake.lists.load(Ordering::Relaxed);
        emit(json!({"event": "self-check", "deep_entries": seen, "lists": lists}));
        for (node, want) in [(NodeId(1), HELLO.as_bytes().to_vec()), (NodeId(4), pattern(64 * 1024))] {
            let mut reader = fake.open(node, 1, 0).expect("open");
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut reader, &mut bytes).expect("read");
            emit(
                json!({"event": "file", "name": "self-check", "length": bytes.len(), "sha256": digest(&bytes), "ok": bytes == want}),
            );
        }
        emit(json!({"event": "done", "failures": 0}));
    }

    fn walk(root: &std::path::Path, depth: u32) -> u64 {
        if depth > 8 {
            return 1;
        }
        let mut failures = 0u64;
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) => {
                emit(json!({"event": "walk-error", "dir": root.display().to_string(), "reason": error.to_string()}));
                return 1;
            }
        };
        let mut names = Vec::new();
        for entry in entries.flatten() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        for name in names {
            let path = root.join(&name);
            match std::fs::symlink_metadata(&path) {
                Ok(meta) => {
                    let kind = if meta.file_type().is_symlink() {
                        "symlink"
                    } else if meta.file_type().is_dir() {
                        "dir"
                    } else {
                        "file"
                    };
                    emit(json!({"event": "entry", "name": name, "kind": kind, "size": meta.len()}));
                    if meta.file_type().is_dir() {
                        failures += walk(&path, depth + 1);
                    }
                }
                Err(error) => {
                    failures += 1;
                    emit(json!({"event": "stat-error", "name": name, "reason": error.to_string()}));
                }
            }
        }
        failures
    }

    fn verify_file(path: &std::path::Path, expected: &[u8], mode: &Mode) -> u64 {
        let actual = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                emit(
                    json!({"event": "read-error", "name": path.display().to_string(), "errno": errno_code(&error), "reason": error.to_string(), "expected": expected_failure(mode)}),
                );
                return u64::from(!expected_failure(mode));
            }
        };
        let want: String = Sha256::digest(expected).iter().map(|b| format!("{b:02x}")).collect();
        let ok = digest(&actual) == want;
        emit(json!({
            "event": "file",
            "name": path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
            "length": actual.len(),
            "sha256": digest(&actual),
            "ok": ok,
        }));
        u64::from(!ok)
    }

    fn check_link(path: &std::path::Path, target: &str, safe: bool, mode: &Mode) -> u64 {
        match std::fs::read_link(path) {
            Ok(actual) if actual == std::path::Path::new(target) => {
                emit(
                    json!({"event": "symlink", "name": path.display().to_string(), "target": actual.display().to_string(), "safe": safe}),
                );
                u64::from(!safe)
            }
            other => {
                emit(
                    json!({"event": "symlink-guard", "name": path.display().to_string(), "result": format!("{other:?}"), "safe": safe, "expected": expected_failure(mode)}),
                );
                u64::from(safe && !expected_failure(mode))
            }
        }
    }

    fn errno_code(error: &io::Error) -> i32 {
        error.raw_os_error().unwrap_or(-1)
    }

    fn emit(value: Value) {
        let stdout = io::stdout();
        let mut guard = stdout.lock();
        let _ = writeln!(guard, "{value}");
        let _ = guard.flush();
    }

    fn digest(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn a_text() -> Vec<u8> {
        "folder a".repeat(64).into_bytes()
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| b'a' + (i % 26) as u8).collect()
    }

    struct Catalog {
        roots: Vec<FileNode>,
        children: HashMap<NodeId, Vec<FileNode>>,
        content: HashMap<NodeId, Vec<u8>>,
    }

    struct Fake {
        big_kib: usize,
        lists: AtomicU64,
        opens: AtomicU64,
        stale: AtomicU64,
        offline: AtomicU64,
    }

    impl Fake {
        fn new(big_kib: usize, stale: bool, offline: bool) -> Self {
            Self {
                big_kib,
                lists: AtomicU64::new(0),
                opens: AtomicU64::new(0),
                stale: AtomicU64::new(u64::from(stale)),
                offline: AtomicU64::new(u64::from(offline)),
            }
        }

        fn nodes(&self) -> Catalog {
            let file = |id: u64, parent: Option<NodeId>, name: &str, size: usize| FileNode {
                id: NodeId(id),
                parent,
                name: name.into(),
                kind: NodeKind::File,
                size: ContentSize::Known(size as u64),
                version: 1,
                modified_ms: None,
                exec_bits: None,
            };
            let dir = |id: u64, parent: Option<NodeId>, name: &str| FileNode {
                id: NodeId(id),
                parent,
                name: name.into(),
                kind: NodeKind::Directory,
                size: ContentSize::Known(0),
                version: 1,
                modified_ms: None,
                exec_bits: None,
            };
            let link = |id: u64, name: &str, target: &str| FileNode {
                id: NodeId(id),
                parent: None,
                name: name.into(),
                kind: NodeKind::Symlink(target.into()),
                size: ContentSize::Known(target.len() as u64),
                version: 1,
                modified_ms: None,
                exec_bits: None,
            };
            let roots = vec![
                file(1, None, "hello.txt", HELLO.len()),
                dir(2, None, "folder"),
                dir(6, None, "deep"),
                file(4, None, "big.bin", self.big_kib * 1024),
                link(5, "link", LINK),
                link(7, "escape", ESCAPE),
                file(81, None, "dup.txt", 3),
                file(82, None, "dup.txt", 5),
            ];
            let mut children = HashMap::new();
            children.insert(
                NodeId(2),
                vec![
                    file(21, Some(NodeId(2)), "a.txt", a_text().len()),
                    file(22, Some(NodeId(2)), "b.bin", 7),
                ],
            );
            children.insert(
                NodeId(6),
                (0..5u8)
                    .map(|i| file(60 + u64::from(i), Some(NodeId(6)), &format!("d{i}.txt"), 3))
                    .collect(),
            );
            let content: HashMap<NodeId, Vec<u8>> = HashMap::from([
                (NodeId(1), HELLO.as_bytes().to_vec()),
                (NodeId(21), a_text()),
                (NodeId(22), vec![7u8; 7]),
                (NodeId(4), pattern(self.big_kib * 1024)),
                (NodeId(81), vec![1u8; 3]),
                (NodeId(82), vec![2u8; 5]),
            ]);
            Catalog {
                roots,
                children,
                content,
            }
        }
    }

    impl Namespace for Fake {
        fn roots(&self) -> Vec<FileNode> {
            self.nodes().roots
        }

        fn list(&self, node: NodeId, cursor: Option<DirectoryCursor>) -> io::Result<DirectoryPage> {
            self.lists.fetch_add(1, Ordering::Relaxed);
            let children = self.nodes().children;
            let all = children.get(&node).cloned().unwrap_or_default();
            let start = cursor.map_or(0, |c| c.offset as usize);
            if start > all.len() {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            // Two entries per page exercises the bridge's cursor walk.
            let take = 2;
            let entries = all[start..all.len().min(start + take)].to_vec();
            let end = start + entries.len();
            Ok(DirectoryPage {
                directory: node,
                version: 1,
                entries,
                next: (end < all.len()).then_some(DirectoryCursor {
                    version: 1,
                    offset: end as u32,
                }),
            })
        }

        fn select(&self, node: NodeId) -> io::Result<FileNode> {
            let roots = self.nodes().roots;
            let children = self.nodes().children;
            roots
                .into_iter()
                .chain(children.values().flatten().cloned())
                .find(|n| n.id == node)
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }

        fn open(&self, node: NodeId, version: u64, offset: u64) -> io::Result<FileReader> {
            self.opens.fetch_add(1, Ordering::Relaxed);
            if self.offline.load(Ordering::SeqCst) > 0 {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            if self.stale.load(Ordering::SeqCst) > 0 || version != 1 {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            let content = self.nodes().content;
            let bytes = content.get(&node).ok_or(io::ErrorKind::NotFound)?;
            let offset = usize::try_from(offset).map_err(|_| io::ErrorKind::InvalidData)?;
            if offset > bytes.len() {
                return Err(io::ErrorKind::InvalidData.into());
            }
            Ok(Box::new(std::io::Cursor::new(bytes[offset..].to_vec())))
        }
    }
}
