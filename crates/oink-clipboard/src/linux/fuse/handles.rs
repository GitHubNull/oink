//! Open-handle table. A handle pins the version selected at open and holds
//! one native lease for its whole life, so the namespace keeps the consumer
//! alive between reads; release drops the reader, the lease, and the slot.
//! Reads reopen at the requested offset whenever the reader drifted, which
//! keeps out-of-order kernel preads correct against versioned sources.
use crate::FileReader;
use crate::content::namespace::{Namespace, NativeLease};
use crate::content;
use crate::model::Unavailable;
use oink_foundation::content::NodeId;
use std::collections::HashMap;
use std::io;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub(super) struct Handle {
    node: NodeId,
    version: u64,
    reader: Arc<Mutex<Option<(u64, FileReader)>>>,
    /// Keeping the lease alive past open is what separates a FUSE consumer
    /// from a one-shot clipboard read.
    _lease: Option<NativeLease>,
}

pub(super) struct Handles {
    namespace: Arc<dyn Namespace>,
    next: AtomicU64,
    table: Mutex<HashMap<u64, Handle>>,
}

impl Handles {
    pub(super) fn new(namespace: Arc<dyn Namespace>) -> Self {
        Self {
            namespace,
            next: AtomicU64::new(1),
            table: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn open(&self, node: NodeId) -> io::Result<(u64, u64)> {
        let selected = self.namespace.select(node)?;
        let lease = self.namespace.native_lease().ok();
        let fh = self.next.fetch_add(1, Ordering::Relaxed);
        self.table.lock().unwrap_or_else(|e| e.into_inner()).insert(
            fh,
            Handle {
                node,
                version: selected.version,
                reader: Arc::new(Mutex::new(None)),
                _lease: lease,
            },
        );
        Ok((fh, selected.version))
    }

    /// Read up to `size` bytes at `offset`, returning the short or empty
    /// result the kernel expects at the end of the file.
    pub(super) fn read(&self, fh: u64, offset: u64, size: usize) -> io::Result<Vec<u8>> {
        let reader = {
            let table = self.table.lock().unwrap_or_else(|e| e.into_inner());
            table.get(&fh).ok_or(io::ErrorKind::NotFound)?.reader.clone()
        };
        let (node, version) = {
            let table = self.table.lock().unwrap_or_else(|e| e.into_inner());
            let handle = table.get(&fh).ok_or(io::ErrorKind::NotFound)?;
            (handle.node, handle.version)
        };
        let mut guard = reader.lock().unwrap_or_else(|e| e.into_inner());
        let reopen = match guard.as_ref() {
            Some((position, _)) => *position != offset,
            None => true,
        };
        if reopen {
            // Version selection is revalidated by the namespace on open; a
            // superseded version fails here instead of serving stale bytes.
            let reader = self.namespace.open(node, version, offset)?;
            *guard = Some((offset, reader));
        }
        let Some((position, reader)) = guard.as_mut() else {
            return Err(io::ErrorKind::NotFound.into());
        };
        let mut bytes = vec![0u8; size];
        let count = reader.read(&mut bytes)?;
        bytes.truncate(count);
        *position = offset + count as u64;
        Ok(bytes)
    }

    pub(super) fn release(&self, fh: u64) {
        self.table.lock().unwrap_or_else(|e| e.into_inner()).remove(&fh);
    }

    #[cfg(test)]
    pub(super) fn live(&self) -> usize {
        self.table.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// Map a namespace failure to the errno the consumer should see.
pub(super) fn errno(error: &io::Error) -> fuser::Errno {
    use libc::{
        EACCES, ECANCELED, EIO, ENOENT, ENOSPC, ENOTSUP, EOVERFLOW, ESTALE, ETIMEDOUT,
    };
    let code = match content::reason(error) {
        Unavailable::NoSuchItem => ENOENT,
        Unavailable::Permission => EACCES,
        Unavailable::Unsupported => ENOTSUP,
        Unavailable::LimitExceeded => EOVERFLOW,
        Unavailable::NoProgress => ETIMEDOUT,
        Unavailable::StorageFull => ENOSPC,
        Unavailable::Superseded => ESTALE,
        Unavailable::Cancelled => ECANCELED,
        _ => EIO,
    };
    fuser::Errno::from_i32(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::fuse::fake::Fake;
    use oink_foundation::content::NodeId;

    fn fixture() -> (Arc<Fake>, Handles, NodeId) {
        let mut fake = Fake::new();
        let file = Fake::file(7, None, "seven", b"1234567");
        fake.add_root(file);
        fake.content.insert(NodeId(7), b"1234567".to_vec());
        let fake = Arc::new(fake);
        let handles = Handles::new(fake.clone());
        (fake, handles, NodeId(7))
    }

    #[test]
    fn reads_walk_offsets_and_reopen_at_drift() {
        let (fake, handles, node) = fixture();
        let (fh, _version) = handles.open(node).expect("open");
        assert_eq!(handles.read(fh, 0, 4).expect("read 0"), b"1234");
        assert_eq!(handles.read(fh, 4, 4).expect("read 4"), b"567");
        // An out-of-order read behind the cursor reopens at the new offset;
        // the sequential continuation between them never reopened.
        assert_eq!(handles.read(fh, 1, 2).expect("read 1"), b"23");
        assert_eq!(fake.opens.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn release_drops_handle_and_lease_together() {
        let (fake, handles, node) = fixture();
        let (fh, _) = handles.open(node).expect("open");
        assert_eq!(handles.live(), 1);
        assert_eq!(fake.leases.load(Ordering::SeqCst), 1);
        handles.release(fh);
        assert_eq!(handles.live(), 0);
        assert_eq!(fake.leases.load(Ordering::SeqCst), 0);
        assert!(handles.read(fh, 0, 1).is_err());
    }

    #[test]
    fn superseded_version_fails_instead_of_serving_stale_bytes() {
        let (fake, handles, node) = fixture();
        let (fh, _) = handles.open(node).expect("open");
        fake.bump_version(node);
        let error = handles.read(fh, 0, 1).expect_err("stale version must fail");
        assert_eq!(content::reason(&error), Unavailable::Superseded);
        assert_eq!(errno(&error).code(), libc::ESTALE);
    }

    #[test]
    fn errno_mapping_covers_contract_failures() {
        let cases = [
            (Unavailable::NoSuchItem, libc::ENOENT),
            (Unavailable::Permission, libc::EACCES),
            (Unavailable::Unsupported, libc::ENOTSUP),
            (Unavailable::LimitExceeded, libc::EOVERFLOW),
            (Unavailable::NoProgress, libc::ETIMEDOUT),
            (Unavailable::StorageFull, libc::ENOSPC),
            (Unavailable::Superseded, libc::ESTALE),
            (Unavailable::Cancelled, libc::ECANCELED),
            (Unavailable::InvalidData, libc::EIO),
        ];
        for (kind, want) in cases {
            let error = content::unavailable(kind);
            assert_eq!(errno(&error).code(), want, "{kind:?}");
        }
    }
}
