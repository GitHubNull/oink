//! Requestor side of the ICCCM selection protocol. Each operation opens a
//! short-lived connection, so no Xlib object crosses a thread boundary.
use crate::content::unavailable;
use crate::linux::atoms::Atoms;
use crate::linux::sys::wait_readable;
use crate::linux::x11::{self, Conn};
use crate::linux::Identity;
use crate::model::Unavailable;
use std::io;
use std::time::{Duration, Instant};

const NOTIFY_TIMEOUT: Duration = Duration::from_secs(5);
const CHUNK_TIMEOUT: Duration = Duration::from_secs(30);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_TARGETS: usize = 4096;
/// ICCCM requires INCR for data beyond the request window. Direct reads stay
/// bounded so a peer cannot force one enormous property reply.
const DIRECT_BYTES: usize = 16 * 1024 * 1024;
const URI_PREFIX: usize = 16 * 1024;

/// A short-lived requestor connection and its private property window.
pub(crate) struct Requestor {
    pub(crate) conn: Conn,
    pub(crate) atoms: Atoms,
    pub(crate) window: u64,
    /// The selection every conversion below addresses (CLIPBOARD or XdndSelection).
    pub(crate) selection: u64,
}

impl Requestor {
    pub(crate) fn open() -> io::Result<Self> {
        Self::open_for("CLIPBOARD")
    }

    pub(crate) fn open_for(selection: &str) -> io::Result<Self> {
        let conn = Conn::open()?;
        let atoms = Atoms::load(&conn)?;
        let selection = conn.atom(selection, false)?;
        if selection == 0 {
            return Err(io::Error::new(io::ErrorKind::NotFound, "selection atom is absent"));
        }
        let window = conn.create_window(x11::PROPERTY_CHANGE_MASK)?;
        Ok(Self { conn, atoms, window, selection })
    }

    /// Owner window plus ownership timestamp; None when the selection is unowned.
    pub(crate) fn identity(&self) -> io::Result<Option<Identity>> {
        let owner = self.conn.selection_owner(self.selection);
        if owner == 0 {
            return Ok(None);
        }
        let timestamp = self.timestamp().unwrap_or(0);
        Ok(Some(Identity { owner, timestamp }))
    }

    fn timestamp(&self) -> io::Result<u32> {
        self.convert(self.atoms.timestamp)?;
        let Some((type_, values)) = self.conn.read_values32(self.window, self.atoms.oink_selection, 1)? else {
            return Ok(0);
        };
        if type_ != self.atoms.integer {
            return Ok(0);
        }
        Ok(values.first().copied().unwrap_or(0))
    }

    /// TARGETS in the owner's advertised order.
    pub(crate) fn targets(&self) -> io::Result<Vec<u64>> {
        self.convert(self.atoms.targets)?;
        let Some((_, values)) = self.conn.read_values32(self.window, self.atoms.oink_selection, MAX_TARGETS)? else {
            return Err(unavailable(Unavailable::InvalidData));
        };
        Ok(values.into_iter().map(u64::from).collect())
    }

    /// One bounded byte representation, following INCR responses to completion.
    pub(crate) fn bytes(&self, target: u64, limit: usize) -> io::Result<Vec<u8>> {
        self.convert(target)?;
        let invalid = || unavailable(Unavailable::InvalidData);
        let Some(property) = self
            .conn
            .read_any(self.window, self.atoms.oink_selection, limit.min(DIRECT_BYTES))?
        else {
            return Err(invalid());
        };
        if property.type_ != self.atoms.incr {
            return if property.format == 8 {
                Ok(property.bytes)
            } else {
                Err(invalid())
            };
        }
        if property.format != 32 {
            return Err(invalid());
        }
        let total = u64::from(super::sys::words(&property.bytes).first().copied().unwrap_or(0));
        if total > limit as u64 {
            return Err(unavailable(Unavailable::LimitExceeded));
        }
        let mut out = Vec::new();
        let deadline = Instant::now() + TOTAL_TIMEOUT;
        loop {
            self.wait_property(x11::EVENT_STATE_NEW_VALUE, deadline)?;
            let Some(property) = self
                .conn
                .read_any(self.window, self.atoms.oink_selection, limit - out.len())?
            else {
                return Err(unavailable(Unavailable::ReadFailed));
            };
            if property.format != 8 {
                return Err(invalid());
            }
            if property.bytes.is_empty() {
                return if out.len() as u64 == total {
                    Ok(out)
                } else {
                    Err(invalid())
                };
            }
            out.extend_from_slice(&property.bytes);
            if out.len() > limit {
                return Err(unavailable(Unavailable::LimitExceeded));
            }
        }
    }

    /// Classify a uri-list without fetching it: Some(true) means the target
    /// holds local files (or an INCR list too large to be a URL), None means
    /// the owner refused.
    pub(crate) fn file_list_target(&self, target: u64) -> io::Result<Option<bool>> {
        self.convert(target)?;
        let Some((type_, bytes)) =
            self.conn
                .read_bytes_prefix(self.window, self.atoms.oink_selection, URI_PREFIX)?
        else {
            return Ok(None);
        };
        if type_ == self.atoms.incr {
            return Ok(Some(true));
        }
        Ok(Some(is_file_list(&bytes)))
    }
}

impl Requestor {
    fn convert(&self, target: u64) -> io::Result<()> {
        self.conn.convert(self.selection, target, self.atoms.oink_selection, self.window, 0);
        let deadline = Instant::now() + NOTIFY_TIMEOUT;
        loop {
            while self.conn.pending() {
                let event = self.conn.next_event();
                // SAFETY: the union is plain data; type_ is always initialized.
                if unsafe { event.type_ } != x11::EVENT_TYPE_SELECTION_NOTIFY {
                    continue;
                }
                // SAFETY: the event type selects the SelectionNotify arm.
                let selection = unsafe { event.selection };
                if selection.requestor != self.window
                    || selection.selection != self.selection
                    || selection.target != target
                {
                    continue;
                }
                return if selection.property == 0 {
                    Err(io::Error::new(io::ErrorKind::PermissionDenied, "selection refused"))
                } else {
                    Ok(())
                };
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "selection notify"));
            }
            wait_readable(self.conn.fd(), deadline - now)?;
        }
    }

    fn wait_property(&self, state: i32, deadline: Instant) -> io::Result<()> {
        loop {
            while self.conn.pending() {
                let event = self.conn.next_event();
                // SAFETY: the union is plain data; type_ is always initialized.
                if unsafe { event.type_ } != x11::EVENT_TYPE_PROPERTY_NOTIFY {
                    continue;
                }
                // SAFETY: the event type selects the PropertyNotify arm.
                let property = unsafe { event.property };
                if property.window == self.window
                    && property.atom == self.atoms.oink_selection
                    && property.state == state
                {
                    return Ok(());
                }
            }
            let now = Instant::now();
            let wait = deadline.min(now + CHUNK_TIMEOUT);
            if now >= wait {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "selection property"));
            }
            wait_readable(self.conn.fd(), wait - now)?;
        }
    }
}

/// Identity of the CLIPBOARD owner on a fresh connection.
pub(crate) fn identity() -> io::Result<Option<Identity>> {
    Requestor::open()?.identity()
}

/// True when the clipboard still belongs to the same owner at the same time.
pub(crate) fn matches(expected: Identity) -> io::Result<bool> {
    Ok(identity()? == Some(expected))
}

/// URLs never carry file paths across systems; file lists stay files.
pub(crate) fn is_file_list(bytes: &[u8]) -> bool {
    bytes
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .map(|line| std::str::from_utf8(line).unwrap_or_default().trim())
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .is_some_and(|uri| uri.starts_with("file:"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content;
    use crate::linux::atoms;
    #[test]
    fn file_lists_are_not_urls_and_comments_are_skipped() {
        assert!(is_file_list(b"# copied\r\nfile:///tmp/a\r\n"));
        assert!(!is_file_list(b"https://example.com/a\r\n"));
        assert!(!is_file_list(b"# only comments\r\n"));
        assert!(!is_file_list(&[]));
        assert_eq!(content::reason(&unavailable(Unavailable::NoSuchItem)), Unavailable::NoSuchItem);
        let _ = atoms::URI_LIST;
    }
}
