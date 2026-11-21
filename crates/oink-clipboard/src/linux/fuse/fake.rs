//! Scripted Namespace for the FUSE bridge unit tests: configurable page
//! size, version bumps, failure injection and lease/open/list counting.
use crate::content::namespace::{Namespace, NativeLease};
use crate::content::unavailable;
use crate::model::{DirectoryCursor, DirectoryPage, Unavailable};
use oink_foundation::content::{ContentSize, FileNode, NodeId, NodeKind};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub(super) struct Fake {
    pub roots: Vec<FileNode>,
    pub children: HashMap<NodeId, Vec<FileNode>>,
    pub content: HashMap<NodeId, Vec<u8>>,
    /// Entries per DirectoryPage; 0 means everything in one page.
    pub page_size: usize,
    versions: Mutex<HashMap<NodeId, u64>>,
    fail_list: Mutex<Option<Unavailable>>,
    fail_open: Mutex<Option<Unavailable>>,
    pub lists: AtomicUsize,
    pub opens: AtomicUsize,
    pub selects: AtomicUsize,
    pub leases: Arc<AtomicUsize>,
    pub cancelled: AtomicBool,
}

impl Fake {
    pub fn new() -> Self {
        Self {
            roots: Vec::new(),
            children: HashMap::new(),
            content: HashMap::new(),
            page_size: 0,
            versions: Mutex::new(HashMap::new()),
            fail_list: Mutex::new(None),
            fail_open: Mutex::new(None),
            lists: AtomicUsize::new(0),
            opens: AtomicUsize::new(0),
            selects: AtomicUsize::new(0),
            leases: Arc::new(AtomicUsize::new(0)),
            cancelled: AtomicBool::new(false),
        }
    }

    pub fn add_root(&mut self, node: FileNode) {
        self.versions.lock().unwrap().insert(node.id, node.version);
        self.roots.push(node);
    }

    pub fn add_child(&mut self, parent: NodeId, node: FileNode, content: Option<Vec<u8>>) {
        self.versions.lock().unwrap().insert(node.id, node.version);
        if let Some(content) = content {
            self.content.insert(node.id, content);
        }
        self.children.entry(parent).or_default().push(node);
    }

    pub fn fail_lists_with(&self, kind: Unavailable) {
        *self.fail_list.lock().unwrap() = Some(kind);
    }

    pub fn fail_opens_with(&self, kind: Unavailable) {
        *self.fail_open.lock().unwrap() = Some(kind);
    }

    pub fn bump_version(&self, node: NodeId) {
        let mut versions = self.versions.lock().unwrap();
        let next = versions.get(&node).copied().unwrap_or(1) + 1;
        versions.insert(node, next);
    }

    fn version_of(&self, node: NodeId) -> u64 {
        self.versions.lock().unwrap().get(&node).copied().unwrap_or(1)
    }

    pub fn file(id: u64, parent: Option<NodeId>, name: &str, bytes: &[u8]) -> FileNode {
        FileNode {
            id: NodeId(id),
            parent,
            name: name.into(),
            kind: NodeKind::File,
            size: ContentSize::Known(bytes.len() as u64),
            version: 1,
            modified_ms: None,
            exec_bits: None,
        }
    }

    pub fn dir(id: u64, parent: Option<NodeId>, name: &str) -> FileNode {
        FileNode {
            id: NodeId(id),
            parent,
            name: name.into(),
            kind: NodeKind::Directory,
            size: ContentSize::Known(0),
            version: 1,
            modified_ms: None,
            exec_bits: None,
        }
    }

    fn find(&self, node: NodeId) -> Option<FileNode> {
        self.roots
            .iter()
            .chain(self.children.values().flatten())
            .find(|n| n.id == node)
            .cloned()
    }
}

impl Default for Fake {
    fn default() -> Self {
        Self::new()
    }
}

impl Namespace for Fake {
    fn roots(&self) -> Vec<FileNode> {
        self.roots.clone()
    }

    fn list(&self, node: NodeId, cursor: Option<DirectoryCursor>) -> io::Result<DirectoryPage> {
        self.lists.fetch_add(1, Ordering::Relaxed);
        if let Some(kind) = *self.fail_list.lock().unwrap() {
            return Err(unavailable(kind));
        }
        let version = self.version_of(node);
        if cursor.is_some_and(|c| c.version != version) {
            return Err(unavailable(Unavailable::Superseded));
        }
        let all = self.children.get(&node).cloned().unwrap_or_default();
        let start = cursor.map_or(0, |c| c.offset as usize);
        if start > all.len() {
            return Err(unavailable(Unavailable::InvalidData));
        }
        let take = if self.page_size == 0 { all.len() } else { self.page_size };
        let entries = all[start..all.len().min(start + take)].to_vec();
        let end = start + entries.len();
        let page = DirectoryPage {
            directory: node,
            version,
            entries,
            next: (end < all.len()).then_some(DirectoryCursor {
                version,
                offset: end as u32,
            }),
        };
        page.validate(cursor).map_err(unavailable)?;
        Ok(page)
    }

    fn select(&self, node: NodeId) -> io::Result<FileNode> {
        self.selects.fetch_add(1, Ordering::Relaxed);
        let found = self.find(node).ok_or_else(|| unavailable(Unavailable::NoSuchItem))?;
        let version = self.version_of(node);
        let mut found = found;
        found.version = version;
        Ok(found)
    }

    fn open(&self, node: NodeId, version: u64, offset: u64) -> io::Result<crate::FileReader> {
        self.opens.fetch_add(1, Ordering::Relaxed);
        if let Some(kind) = *self.fail_open.lock().unwrap() {
            return Err(unavailable(kind));
        }
        if self.version_of(node) != version {
            return Err(unavailable(Unavailable::Superseded));
        }
        let bytes = self.content.get(&node).ok_or_else(|| unavailable(Unavailable::NoSuchItem))?;
        let offset = usize::try_from(offset).map_err(|_| unavailable(Unavailable::InvalidData))?;
        if offset > bytes.len() {
            return Err(unavailable(Unavailable::InvalidData));
        }
        Ok(Box::new(std::io::Cursor::new(bytes[offset..].to_vec())))
    }

    fn native_lease(&self) -> io::Result<NativeLease> {
        self.leases.fetch_add(1, Ordering::SeqCst);
        let leases = self.leases.clone();
        struct Guard(Arc<AtomicUsize>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        Ok(Box::new(Guard(leases)))
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}
