//! The read-only FUSE filesystem bridging a Namespace. Kernel attribute and
//! entry caching are disabled at the mount; every operation revalidates
//! through the index, so a superseded namespace fails closed with ESTALE/EIO
//! instead of serving stale bytes. `inject` lets the probe and unit tests
//! force failures per operation.
use super::{cache::Cache, handles, handles::Handles, links};
use crate::content::namespace::Namespace;
use fuser::{
    FileAttr, FileType, Filesystem, Generation, INodeNo, MountOption, OpenAccMode, ReplyAttr,
    ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen, Request, SessionACL, FopenFlags, FileHandle,
};
use oink_foundation::content::{ContentSize, NodeKind};
use std::ffi::OsStr;
use std::io;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

/// No kernel caching: pasters must always see the namespace's current truth.
const NO_TTL: Duration = Duration::ZERO;
const GENERATION: Generation = Generation(1);

pub(crate) type Inject = Arc<dyn Fn(&'static str) -> io::Result<()> + Send + Sync>;

pub(crate) struct Bridge {
    index: links::Index,
    cache: Cache,
    handles: Handles,
    inject: Option<Inject>,
}

impl Bridge {
    pub(crate) fn new(namespace: Arc<dyn Namespace>, inject: Option<Inject>) -> Self {
        Self {
            index: links::Index::new(namespace.clone()),
            cache: Cache::new(namespace.clone()),
            handles: Handles::new(namespace),
            inject,
        }
    }

    fn check(&self, op: &'static str) -> io::Result<()> {
        if let Some(inject) = &self.inject {
            inject(op)?;
        }
        Ok(())
    }

    fn attr(&self, ino: u64, node: &oink_foundation::content::FileNode) -> FileAttr {
        let (kind, perm) = match &node.kind {
            NodeKind::File => {
                let exec = node.exec_bits.map(|bits| bits & 0o111).unwrap_or(0);
                (FileType::RegularFile, 0o444 | exec)
            }
            NodeKind::Directory => (FileType::Directory, 0o555),
            NodeKind::Symlink(_) => (FileType::Symlink, 0o444),
        };
        let size = match node.size {
            ContentSize::Known(size) => size,
            ContentSize::Unknown => 0,
        };
        let mtime = node
            .modified_ms
            .and_then(|ms| u64::try_from(ms).ok())
            .map(|ms| UNIX_EPOCH + Duration::from_millis(ms))
            .unwrap_or(UNIX_EPOCH);
        let (uid, gid) = owner_ids();
        FileAttr {
            ino: INodeNo(ino),
            size,
            blocks: size.div_ceil(512),
            atime: mtime,
            mtime,
            ctime: mtime,
            crtime: mtime,
            kind,
            perm,
            nlink: if kind == FileType::Directory { 2 } else { 1 },
            uid,
            gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn root_attr(&self) -> FileAttr {
        let (uid, gid) = owner_ids();
        FileAttr {
            ino: INodeNo(links::ROOT_INO),
            size: 0,
            blocks: 0,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::Directory,
            perm: 0o555,
            nlink: 2,
            uid,
            gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    /// `None` is the synthetic root directory. Only files are re-selected
    /// through the namespace: `select` is the file-read contract, so a
    /// directory or symlink would fail there even though its attributes are
    /// already known from the listing that materialized the inode.
    fn selected(&self, ino: u64) -> io::Result<Option<oink_foundation::content::FileNode>> {
        if ino == links::ROOT_INO {
            return Ok(None);
        }
        let stored = self.index.entry(ino)?;
        if stored.kind == oink_foundation::content::NodeKind::File {
            self.index.namespace().select(stored.id).map(Some)
        } else {
            Ok(Some(stored))
        }
    }

    fn errno(&self, error: io::Error) -> fuser::Errno {
        handles::errno(&error)
    }

    /// Walk every directory page through the bounded cache.
    fn listing(&self, node: oink_foundation::content::NodeId) -> io::Result<Vec<oink_foundation::content::FileNode>> {
        let mut entries = Vec::new();
        let mut cursor = None;
        loop {
            let page = self.cache.page(node, cursor)?;
            entries.extend(page.entries.iter().cloned());
            match page.next {
                Some(next) => cursor = Some(next),
                None => return Ok(entries),
            }
        }
    }

    fn mount_options() -> Vec<MountOption> {
        vec![
            MountOption::RO,
            MountOption::NoDev,
            MountOption::NoSuid,
            MountOption::NoExec,
            MountOption::FSName("oink".into()),
            MountOption::Subtype("oink".into()),
        ]
    }

    pub(crate) fn config() -> fuser::Config {
        let mut config = fuser::Config::default();
        config.mount_options = Self::mount_options();
        config.acl = SessionACL::Owner;
        config
    }
}

impl Filesystem for Bridge {
    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let ino = ino.0;
        let result = self
            .check("getattr")
            .and_then(|()| self.selected(ino))
            .map(|node| match node {
                None => self.root_attr(),
                Some(node) => self.attr(ino, &node),
            });
        match result {
            Ok(attr) => reply.attr(&NO_TTL, &attr),
            Err(error) => reply.error(handles::errno(&error)),
        }
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let result = self.check("lookup").and_then(|()| {
            let ino = self.index.child(parent.0, name)?;
            let node = self
                .selected(ino)?
                .expect("child is never the synthetic root");
            Ok((ino, node))
        });
        match result {
            Ok((ino, node)) => reply.entry(&NO_TTL, &self.attr(ino, &node), GENERATION),
            Err(error) => reply.error(self.errno(error)),
        }
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        let result = self.check("readlink").and_then(|()| {
            let node = self
                .selected(ino.0)?
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            links::safe_link(&node).map(str::to_owned)
        });
        match result {
            Ok(target) => reply.data(target.as_bytes()),
            Err(error) => reply.error(self.errno(error)),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: fuser::OpenFlags, _kill: bool, reply: ReplyOpen) {
        if flags.acc_mode() != OpenAccMode::O_RDONLY {
            reply.error(fuser::Errno::from_i32(libc::EROFS));
            return;
        }
        let result = self.check("open").and_then(|()| {
            let node = self
                .selected(ino.0)?
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EISDIR))?;
            if node.kind != NodeKind::File {
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            }
            self.handles.open(self.index.node(ino.0)?).map(|(fh, _)| fh)
        });
        match result {
            Ok(fh) => reply.opened(FileHandle(fh), FopenFlags::empty()),
            Err(error) => reply.error(self.errno(error)),
        }
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: fuser::OpenFlags,
        _lock: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        let result = self
            .check("read")
            .and_then(|()| self.handles.read(fh.0, offset, size as usize));
        match result {
            Ok(bytes) => reply.data(&bytes),
            Err(error) => reply.error(self.errno(error)),
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: fuser::OpenFlags,
        _lock: Option<fuser::LockOwner>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        self.handles.release(fh.0);
        reply.ok();
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, reply: ReplyDirectory) {
        let result = self.check("readdir").and_then(|()| {
            let entries: Vec<(String, u64, FileType)> = if ino.0 == links::ROOT_INO {
                self.index
                    .namespace()
                    .roots()
                    .iter()
                    .map(|node| {
                        (node.name.clone(), self.index.materialize(node.clone()), kind(&node.kind))
                    })
                    .collect()
            } else {
                let node = self.index.node(ino.0)?;
                self.listing(node)?
                    .iter()
                    .map(|child| {
                        (child.name.clone(), self.index.materialize(child.clone()), kind(&child.kind))
                    })
                    .collect()
            };
            Ok(entries)
        });
        match result {
            Ok(entries) => {
                let mut buffer = reply;
                for (index, (name, child_ino, file_type)) in entries.iter().skip(offset as usize).enumerate() {
                    let next = offset + index as u64 + 1;
                    if buffer.add(INodeNo(*child_ino), next, *file_type, name) {
                        break;
                    }
                }
                buffer.ok();
            }
            Err(error) => reply.error(self.errno(error)),
        }
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: fuser::ReplyStatfs) {
        // Synthetic read-only volumes: one block free would be a lie, so
        // report a plain zeroed geometry with sane name limits.
        reply.statfs(0, 0, 0, 0, 0, 4096, 255, 4096);
    }

    fn destroy(&mut self) {
        self.cache.flush_all();
    }
}

fn kind(kind: &NodeKind) -> FileType {
    match kind {
        NodeKind::File => FileType::RegularFile,
        NodeKind::Directory => FileType::Directory,
        NodeKind::Symlink(_) => FileType::Symlink,
    }
}

/// Mount owner ids: the bridge is always mounted by (and for) the current
/// user, never root or another uid.
fn owner_ids() -> (u32, u32) {
    // SAFETY: geteuid/getgid take no arguments and are safe in any context.
    unsafe { (libc::geteuid(), libc::getgid()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::fuse::fake::Fake;
    use oink_foundation::content::NodeId;
    use std::sync::atomic::Ordering;

    #[test]
    fn directories_and_symlinks_get_attributes_without_a_file_select() {
        let mut fake = Fake::new();
        fake.file_only_select();
        fake.add_root(Fake::dir(1, None, "tree"));
        fake.add_child(NodeId(1), Fake::dir(2, Some(NodeId(1)), "sub"), None);
        fake.add_child(NodeId(1), Fake::file(3, Some(NodeId(1)), "a.txt", b"hi"), Some(b"hi".to_vec()));
        let mut link = Fake::file(4, Some(NodeId(1)), "link", b"");
        link.kind = NodeKind::Symlink("a.txt".into());
        link.size = ContentSize::Known("a.txt".len() as u64);
        fake.add_child(NodeId(1), link, None);
        let fake = Arc::new(fake);
        let bridge = Bridge::new(fake.clone(), None);

        let tree = bridge.index.child(links::ROOT_INO, OsStr::new("tree")).expect("lookup tree");
        let node = bridge.selected(tree).expect("getattr tree").expect("node");
        assert_eq!(node.kind, NodeKind::Directory);
        let sub = bridge.index.child(tree, OsStr::new("sub")).expect("lookup sub");
        assert_eq!(
            bridge.selected(sub).expect("getattr sub").expect("node").kind,
            NodeKind::Directory
        );
        let link = bridge.index.child(tree, OsStr::new("link")).expect("lookup link");
        assert_eq!(
            bridge.selected(link).expect("getattr link").expect("node").kind,
            NodeKind::Symlink("a.txt".into())
        );
        assert_eq!(fake.selects.load(Ordering::Relaxed), 0, "only files reach select");

        let file = bridge.index.child(tree, OsStr::new("a.txt")).expect("lookup a.txt");
        let node = bridge.selected(file).expect("getattr a.txt").expect("node");
        assert_eq!(node.kind, NodeKind::File);
        assert_eq!(fake.selects.load(Ordering::Relaxed), 1, "files revalidate through select");
    }
}
