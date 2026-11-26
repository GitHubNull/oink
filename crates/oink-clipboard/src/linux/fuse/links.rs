//! Inode index: the synthetic mount root (ino 1) holds the namespace's
//! top-level roots; every other inode wraps one NodeId materialized on
//! demand. Children are resolved through paginated namespace listings only,
//! so no path can escape the offered tree and duplicate root names resolve
//! deterministically to the first entry.
use crate::content::namespace::Namespace;
use crate::model::{DirectoryCursor, FileNode};
use oink_foundation::content::{NodeId, NodeKind};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub(super) const ROOT_INO: u64 = 1;

pub(super) struct Entry {
    pub(super) node: FileNode,
}

pub(super) struct Index {
    namespace: Arc<dyn Namespace>,
    next_ino: AtomicU64,
    by_ino: Mutex<HashMap<u64, Entry>>,
    by_node: Mutex<HashMap<NodeId, u64>>,
    /// Duplicate top-level names hidden behind the first entry.
    pub(super) conflicts: AtomicU64,
}

impl Index {
    pub(super) fn new(namespace: Arc<dyn Namespace>) -> Self {
        Self {
            namespace,
            next_ino: AtomicU64::new(ROOT_INO + 1),
            by_ino: Mutex::new(HashMap::new()),
            by_node: Mutex::new(HashMap::new()),
            conflicts: AtomicU64::new(0),
        }
    }

    pub(super) fn namespace(&self) -> &Arc<dyn Namespace> {
        &self.namespace
    }

    pub(super) fn node(&self, ino: u64) -> io::Result<NodeId> {
        self.entry(ino).map(|node| node.id)
    }

    /// A materialized node's attributes. `Namespace::select` is the file-read
    /// contract and refuses directories, so the bridge answers those from the
    /// listing that produced the inode.
    pub(super) fn entry(&self, ino: u64) -> io::Result<FileNode> {
        if ino == ROOT_INO {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.by_ino
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&ino)
            .map(|entry| entry.node.clone())
            .ok_or(io::ErrorKind::NotFound.into())
    }

    /// Resolve one path component under an indexed directory.
    pub(super) fn child(&self, parent: u64, name: &OsStr) -> io::Result<u64> {
        let name = name
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "non-utf8 name"))?;
        if !valid_name(name) {
            return Err(io::ErrorKind::NotFound.into());
        }
        let candidates = if parent == ROOT_INO {
            self.namespace.roots()
        } else {
            let node = self.node(parent)?;
            self.list_all(node)?
        };
        let mut found = None;
        for candidate in candidates {
            if candidate.name == name && found.is_none() {
                found = Some(candidate);
            } else if candidate.name == name {
                self.conflicts.fetch_add(1, Ordering::Relaxed);
            }
        }
        let node = found.ok_or(io::ErrorKind::NotFound)?;
        Ok(self.materialize(node))
    }

    /// Ino for a node; reuses the existing inode for repeated materialization.
    pub(super) fn materialize(&self, node: FileNode) -> u64 {
        if let Some(ino) = self.by_node.lock().unwrap_or_else(|e| e.into_inner()).get(&node.id) {
            return *ino;
        }
        let ino = self.next_ino.fetch_add(1, Ordering::Relaxed);
        let id = node.id;
        self.by_ino
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ino, Entry { node });
        self.by_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, ino);
        ino
    }

    /// Direct children of an indexed directory, following pagination.
    pub(super) fn list_all(&self, node: NodeId) -> io::Result<Vec<FileNode>> {
        let mut entries = Vec::new();
        let mut cursor: Option<DirectoryCursor> = None;
        loop {
            let page = self.namespace.list(node, cursor)?;
            let next = page.next;
            entries.extend(page.entries);
            match next {
                Some(next) => cursor = Some(next),
                None => return Ok(entries),
            }
        }
    }

}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 4096
        && !name.contains(['/', '\\', '\0'])
}

/// Symlink bodies come from the wire-validated namespace; re-check before
/// the kernel follows them so a hostile target can never leave the mount.
pub(super) fn safe_link(node: &FileNode) -> io::Result<&str> {
    let target = match &node.kind {
        NodeKind::Symlink(target) => target.as_str(),
        _ => return Err(io::ErrorKind::InvalidData.into()),
    };
    if !oink_foundation::content::safe_symlink_target(target, 256) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unsafe link target"));
    }
    Ok(target)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::linux::fuse::fake::Fake;

    fn root(id: u64, name: &str) -> FileNode {
        Fake::file(id, None, name, name.as_bytes())
    }

    #[test]
    fn duplicate_root_names_resolve_to_first_and_count_conflicts() {
        let mut fake = Fake::new();
        fake.add_root(root(1, "dup"));
        fake.add_root(root(2, "dup"));
        fake.add_root(root(3, "other"));
        let index = Index::new(Arc::new(fake));
        let ino = index.child(ROOT_INO, OsStr::new("dup")).expect("lookup");
        assert_eq!(index.node(ino).expect("node"), NodeId(1));
        assert_eq!(index.conflicts.load(Ordering::Relaxed), 1);
        // Re-lookup reuses the same inode; the third root is a fresh one.
        assert_eq!(index.child(ROOT_INO, OsStr::new("dup")).expect("re-lookup"), ino);
        let other = index.child(ROOT_INO, OsStr::new("other")).expect("other");
        assert_ne!(other, ino);
        assert_eq!(index.node(other).expect("other node"), NodeId(3));
    }

    #[test]
    fn lookup_rejects_dotdot_and_separators() {
        let mut fake = Fake::new();
        fake.add_root(root(1, "plain"));
        let index = Index::new(Arc::new(fake));
        for bad in ["..", ".", "a/b", "a\\b"] {
            assert!(index.child(ROOT_INO, OsStr::new(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn materialize_reuses_the_same_inode_for_a_node() {
        let mut fake = Fake::new();
        fake.add_root(root(1, "same"));
        let index = Index::new(Arc::new(fake));
        let first = index.materialize(root(1, "same"));
        let second = index.materialize(root(1, "same"));
        assert_eq!(first, second);
    }

    #[test]
    fn names_reject_separators_and_dot_entries() {
        assert!(valid_name("plain.txt"));
        for bad in ["", ".", "..", "a/b", "a\\b", "a\0b", &"x".repeat(4097)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn link_targets_must_stay_relative_and_bounded() {
        let node = |target: &str| FileNode {
            id: NodeId(1),
            parent: None,
            name: "link".into(),
            kind: NodeKind::Symlink(target.into()),
            size: oink_foundation::content::ContentSize::Known(target.len() as u64),
            version: 1,
            modified_ms: None,
            exec_bits: None,
        };
        assert!(safe_link(&node("dir/file.txt")).is_ok());
        assert!(safe_link(&node("/etc/passwd")).is_err());
        // Bounded relative parents are the platform contract; past the depth
        // budget the link is refused.
        let deep = format!("{}/etc/passwd", "../".repeat(300));
        assert!(safe_link(&node(&deep)).is_err());
    }
}
