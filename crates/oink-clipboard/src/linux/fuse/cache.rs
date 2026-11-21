//! Bounded directory-page cache. Kernel caching is disabled at the mount, so
//! this cache only amortizes repeated listings inside one consumer (a file
//! manager refreshing a view); it holds at most PAGE_CACHE_PAGES pages and is
//! invalidated wholesale on any superseded listing or namespace cancel.
use crate::content::namespace::Namespace;
use crate::content;
use crate::model::{DirectoryCursor, DirectoryPage, Unavailable};
use oink_foundation::content::NodeId;
use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

/// A page is at most DIRECTORY_PAGE_BYTES; 32 pages bound the cache at
/// ~32 MiB while covering deep listings a consumer walks page by page.
const PAGE_CACHE_PAGES: usize = 32;

struct Slot {
    page: DirectoryPage,
    age: u64,
}

struct Inner {
    slots: HashMap<(NodeId, u64, u32), Slot>,
    clock: u64,
}

pub(super) struct Cache {
    namespace: Arc<dyn Namespace>,
    inner: Mutex<Inner>,
}

impl Cache {
    pub(super) fn new(namespace: Arc<dyn Namespace>) -> Self {
        Self {
            namespace,
            inner: Mutex::new(Inner { slots: HashMap::new(), clock: 0 }),
        }
    }

    pub(super) fn page(
        &self,
        node: NodeId,
        cursor: Option<DirectoryCursor>,
    ) -> io::Result<DirectoryPage> {
        let key = (node, cursor.map_or(0, |c| c.version), cursor.map_or(0, |c| c.offset));
        let hit = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let page = inner.slots.get(&key).map(|slot| slot.page.clone());
            if page.is_some() {
                inner.clock += 1;
            }
            page
        };
        if let Some(page) = hit {
            return Ok(page);
        }
        let page = self
            .namespace
            .list(node, cursor)
            .inspect_err(|error| {
                if content::reason(error) == Unavailable::Superseded {
                    self.flush_node(node);
                }
            })?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.clock += 1;
        let age = inner.clock;
        inner.slots.insert(key, Slot { page: page.clone(), age });
        while inner.slots.len() > PAGE_CACHE_PAGES {
            let oldest = inner
                .slots
                .iter()
                .min_by_key(|(_, slot)| slot.age)
                .map(|(key, _)| *key);
            if let Some(oldest) = oldest {
                inner.slots.remove(&oldest);
            } else {
                break;
            }
        }
        Ok(page)
    }

    pub(super) fn flush_node(&self, node: NodeId) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .slots
            .retain(|key, _| key.0 != node);
    }

    pub(super) fn flush_all(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).slots.clear();
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).slots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::fuse::fake::Fake;
    use oink_foundation::content::NodeId;

    fn walk(cache: &Cache, node: NodeId) -> Vec<oink_foundation::content::FileNode> {
        let mut entries = Vec::new();
        let mut cursor = None;
        loop {
            let page = cache.page(node, cursor).expect("page");
            entries.extend(page.entries.iter().cloned());
            match page.next {
                Some(next) => cursor = Some(next),
                None => return entries,
            }
        }
    }

    #[test]
    fn walks_paginated_listings_following_cursors() {
        let mut fake = Fake::new();
        let root = Fake::dir(2, None, "folder");
        fake.add_root(root.clone());
        for index in 0..5u8 {
            let bytes = vec![index; 1];
            let child = Fake::file(10 + u64::from(index), Some(root.id), &format!("f{index}"), &bytes);
            fake.add_child(root.id, child, Some(bytes));
        }
        fake.page_size = 2;
        let fake = Arc::new(fake);
        let cache = Cache::new(fake.clone());
        let entries = walk(&cache, root.id);
        assert_eq!(entries.len(), 5);
        assert_eq!(fake.lists.load(std::sync::atomic::Ordering::Relaxed), 3);
    }

    #[test]
    fn repeat_page_hits_cache_without_listing_again() {
        let mut fake = Fake::new();
        let root = Fake::dir(2, None, "folder");
        fake.add_root(root.clone());
        fake.add_child(root.id, Fake::file(10, Some(root.id), "a", b"a"), Some(b"a".to_vec()));
        let fake = Arc::new(fake);
        let cache = Cache::new(fake.clone());
        let first = cache.page(root.id, None).expect("first");
        let second = cache.page(root.id, None).expect("second");
        assert_eq!(first.entries, second.entries);
        assert_eq!(fake.lists.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn superseded_listing_flushes_that_node() {
        let mut fake = Fake::new();
        let root = Fake::dir(2, None, "folder");
        fake.add_root(root.clone());
        fake.add_child(root.id, Fake::file(10, Some(root.id), "a", b"a"), Some(b"a".to_vec()));
        let fake = Arc::new(fake);
        let cache = Cache::new(fake.clone());
        cache.page(root.id, None).expect("prime");
        assert_eq!(cache.len(), 1);
        // Stale cursor against a bumped version fails closed and evicts.
        let stale = Some(DirectoryCursor {
            version: 99,
            offset: 0,
        });
        let error = cache.page(root.id, stale).expect_err("stale must fail");
        assert_eq!(content::reason(&error), Unavailable::Superseded);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn cache_is_bounded_at_32_pages() {
        let mut fake = Fake::new();
        let root = Fake::dir(2, None, "folder");
        fake.add_root(root.clone());
        for index in 0..40u8 {
            let bytes = vec![index; 1];
            let child = Fake::file(10 + u64::from(index), Some(root.id), &format!("f{index}"), &bytes);
            fake.add_child(root.id, child, Some(bytes));
        }
        fake.page_size = 1;
        let fake = Arc::new(fake);
        let cache = Cache::new(fake.clone());
        let entries = walk(&cache, root.id);
        assert_eq!(entries.len(), 40);
        assert!(cache.len() <= 32, "cache grew past its bound: {}", cache.len());
        assert_eq!(cache.len(), 32);
    }
}
