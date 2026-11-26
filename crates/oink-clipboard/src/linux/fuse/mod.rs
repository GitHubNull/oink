//! Read-only FUSE bridge exposing a remote file namespace as local paths, so
//! X11 URI lists can name content that only exists behind a provider. Mounts
//! are owned by the mounting user and come in two lifetimes: clipboard
//! publications keep the mount until their namespace disappears; explicit
//! consumers (CLI, probes) hold a FileShare guard that unmounts on drop.
mod cache;
#[cfg(test)]
mod fake;
mod fs;
mod handles;
mod links;
mod mount;

pub(crate) use mount::blocked_reason;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

type Namespace = dyn crate::content::namespace::Namespace;

struct Share {
    id: u64,
    namespace: Weak<Namespace>,
    path: PathBuf,
    // Never read: keeping the mount alive is the field's whole job.
    _mounted: mount::Mounted,
}

impl Drop for Share {
    fn drop(&mut self) {
        registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|entry| entry.0 != self.id);
    }
}

/// Guard for an explicit mount: dropping the last guard unmounts.
pub struct FileShare {
    inner: Arc<Share>,
}

impl FileShare {
    pub fn path(&self) -> &Path {
        &self.inner.path
    }
}

impl std::fmt::Debug for FileShare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileShare").field("path", &self.inner.path).finish()
    }
}

struct RegistryEntry(u64, Weak<Share>);

fn registry() -> &'static Mutex<VecDeque<RegistryEntry>> {
    static REGISTRY: OnceLock<Mutex<VecDeque<RegistryEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(VecDeque::new()))
}

type KeeperList = Vec<(Weak<Namespace>, Arc<Share>)>;

/// Clipboard publications hold their mount alive here until the namespace
/// disappears; the next mount attempt sweeps the dead entries.
fn keepers() -> &'static Mutex<KeeperList> {
    static KEEPERS: OnceLock<Mutex<KeeperList>> = OnceLock::new();
    KEEPERS.get_or_init(|| Mutex::new(Vec::new()))
}

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn mount_result(
    namespace: Arc<Namespace>,
) -> std::io::Result<Option<Arc<Share>>> {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, existing)) = registry.iter().find_map(|entry| {
        entry
            .1
            .upgrade()
            .filter(|share| {
                share
                    .namespace
                    .upgrade()
                    .is_some_and(|current| Arc::ptr_eq(&current, &namespace))
            })
            .map(|share| (entry.0, share))
    }) {
        return Ok(Some(existing));
    }
    let Some(mounted) = mount::Mounted::spawn(namespace.clone(), &mount_parent(), None)? else {
        return Ok(None);
    };
    let share = Arc::new(Share {
        id: next_id(),
        namespace: Arc::downgrade(&namespace),
        path: mounted.path().to_path_buf(),
        _mounted: mounted,
    });
    registry.push_back(RegistryEntry(share.id, Arc::downgrade(&share)));
    Ok(Some(share))
}

/// Remote top-level entries gain this mount root as their local path. The
/// mount outlives the call and stays until the namespace disappears.
pub(crate) fn share_for(
    namespace: Arc<Namespace>,
) -> Option<PathBuf> {
    let share = mount_result(namespace.clone()).ok()??;
    let path = share.path.clone();
    let mut keepers = keepers().lock().unwrap_or_else(|e| e.into_inner());
    keepers.retain(|(weak, _)| weak.strong_count() > 0);
    keepers.push((Arc::downgrade(&namespace), share));
    Some(path)
}

/// Explicit mount with guard lifetime. Ok(None) means mounting is blocked in
/// this process; Err is an unexpected mount failure.
pub fn mount_file_share(
    namespace: Arc<Namespace>,
) -> std::io::Result<Option<FileShare>> {
    mount_result(namespace).map(|found| found.map(|inner| FileShare { inner }))
}

fn mount_parent() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Process-exit sweep: keepers live in a global static that the language
/// never drops, so without this the kernel keeps every clipboard mount with
/// a dead transport after the process exits. Dropping the shares unmounts.
pub(crate) fn shutdown() {
    // Shares' own Drop locks the registry, so release them before that lock.
    keepers().lock().unwrap_or_else(|e| e.into_inner()).clear();
    registry().lock().unwrap_or_else(|e| e.into_inner()).clear();
}
