//! Mount lifecycle: spawn the session, wait for it in /proc/self/mountinfo,
//! report the setuid fusermount3 block, and always leave the mount directory
//! removed. The bridge is read-only and owned by the mounting user; no
//! allow_other, suid, device or exec bits are ever granted.
use super::fs::{Bridge, Inject};
use crate::content::namespace::Namespace;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MOUNT_WAIT: Duration = Duration::from_secs(5);

pub(crate) struct Mounted {
    /// Option only so Drop can unmount explicitly before removing the
    /// directory; BackgroundSession's own Drop would run too late.
    session: Option<fuser::BackgroundSession>,
    dir: PathBuf,
    /// (mount id, source) parsed from /proc/self/mountinfo at spawn: the
    /// ownership proof for the only mount this struct will ever unmount.
    proof: Option<(u64, String)>,
}

impl Mounted {
    /// Mount `namespace` read-only at a fresh directory under `parent`.
    /// Returns the mount path, or None when mounting is impossible here.
    pub(crate) fn spawn(
        namespace: Arc<dyn Namespace>,
        parent: &Path,
        inject: Option<Inject>,
    ) -> io::Result<Option<Self>> {
        if let Some(reason) = blocked_reason() {
            eprintln!("oink FUSE bridge unavailable: {reason}");
            return Ok(None);
        }
        let dir = unique_dir(parent)?;
        let bridge = Bridge::new(namespace, inject);
        match fuser::spawn_mount(bridge, &dir, &Bridge::config()) {
            Ok(session) => match wait_mounted(&dir) {
                Ok(proof) => Ok(Some(Self {
                    session: Some(session),
                    dir,
                    proof: Some(proof),
                })),
                Err(error) => {
                    let _ = session.umount_and_join();
                    let _ = std::fs::remove_dir(&dir);
                    Err(error)
                }
            },
            Err(error) => {
                let _ = std::fs::remove_dir(&dir);
                Err(error)
            }
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        // Re-verify ownership before unmounting: only a mountinfo entry with
        // the id recorded at spawn is ours to remove.
        if let Some((id, _)) = &self.proof {
            let still_ours = mount_proof(&self.dir)
                .map(|found| found.is_some_and(|(mount_id, _)| mount_id == *id))
                .unwrap_or(false);
            if !still_ours {
                drop(self.session.take());
                let _ = std::fs::remove_dir(&self.dir);
                return;
            }
        }
        // Dropping the session unmounts and joins the session thread; only
        // then is the mount directory removed.
        drop(self.session.take());
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// Why mounting cannot work in this process: setuid fusermount3 refuses to
/// run under NoNewPrivs, which the agent sandbox sets. A normal user terminal
/// does not have the flag and mounts fine.
pub(crate) fn blocked_reason() -> Option<String> {
    if !no_new_privs() {
        return None;
    }
    match std::env::var("OINK_FUSE_ALLOW") {
        Ok(value) if value == "1" => None,
        _ => Some(
            "NoNewPrivs=1 blocks setuid fusermount3; run the mount command in a normal user terminal"
                .to_owned(),
        ),
    }
}

fn no_new_privs() -> bool {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|line| line.strip_prefix("NoNewPrivs:"))
        .map(|value| value.trim() == "1")
        .unwrap_or(false)
}

fn unique_dir(parent: &Path) -> io::Result<PathBuf> {
    let mounts = parent.join("oink/mounts");
    std::fs::create_dir_all(&mounts)?;
    let pid = std::process::id();
    for attempt in 0..100u32 {
        let dir = mounts.join(format!("oink-{pid}-{attempt}"));
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
                return Ok(dir);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free mount directory"))
}

/// Wait for the mount and return its (mount id, source) ownership proof.
fn wait_mounted(dir: &Path) -> io::Result<(u64, String)> {
    let deadline = Instant::now() + MOUNT_WAIT;
    while Instant::now() < deadline {
        if let Some(proof) = mount_proof(dir)? {
            return Ok(proof);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "mount did not appear in mountinfo",
    ))
}

fn mount_proof(dir: &Path) -> io::Result<Option<(u64, String)>> {
    let info = std::fs::read_to_string("/proc/self/mountinfo")?;
    // Fields: id parent major:minor root mount_point options ... - fstype source
    for line in info.lines() {
        let mut fields = line.split(' ');
        let Some(id) = fields.next().and_then(|f| f.parse::<u64>().ok()) else {
            continue;
        };
        let point = fields.nth(3);
        if point != Some(dir.to_str().unwrap_or_default()) {
            continue;
        }
        let fstype = line.split(" - ").nth(1).and_then(|tail| tail.split(' ').next());
        if fstype != Some("fuse.oink") {
            continue;
        }
        let source = line
            .split(" - ")
            .nth(1)
            .map(|tail| tail.split(' ').nth(1).unwrap_or("").to_owned())
            .unwrap_or_default();
        return Ok(Some((id, source)));
    }
    Ok(None)
}
