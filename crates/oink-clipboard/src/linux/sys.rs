//! Raw descriptors, property buffers and poll helpers shared by the X11
//! backend threads. Nothing here owns an Xlib connection.
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::time::Duration;

pub(crate) struct Property {
    pub(crate) type_: u64,
    pub(crate) format: i32,
    pub(crate) items: usize,
    pub(crate) bytes_after: usize,
    pub(crate) bytes: Vec<u8>,
}

/// Decode a format-32 property buffer; values are stored as `c_long` words.
pub(crate) fn words(bytes: &[u8]) -> Vec<u32> {
    let word = std::mem::size_of::<std::ffi::c_long>();
    bytes
        .chunks_exact(word)
        .map(|chunk| {
            let mut value = [0u8; 8];
            value[..chunk.len().min(8)].copy_from_slice(&chunk[..chunk.len().min(8)]);
            i64::from_ne_bytes(value) as u32
        })
        .collect()
}

/// Poll an X descriptor without blocking other owner-thread work.
pub(crate) fn wait_readable(fd: RawFd, timeout: Duration) -> io::Result<()> {
    wait_fds(&mut [libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }], timeout)
}

pub(crate) fn wait_readable2(a: RawFd, b: RawFd, timeout: Duration) -> io::Result<()> {
    wait_fds(
        &mut [
            libc::pollfd {
                fd: a,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: b,
                events: libc::POLLIN,
                revents: 0,
            },
        ],
        timeout,
    )
}

fn wait_fds(fds: &mut [libc::pollfd], timeout: Duration) -> io::Result<()> {
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: the slice matches the declared count; the timeout is bounded.
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, millis) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(());
        }
        return Err(error);
    }
    Ok(())
}

/// Non-blocking wake pipe between backend threads and their event loops.
pub(crate) struct Wakeup {
    pub(crate) read: OwnedFd,
    pub(crate) write: OwnedFd,
}

impl Wakeup {
    pub(crate) fn new() -> io::Result<Self> {
        let mut fds = [0; 2];
        // SAFETY: two descriptors are written into the array.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: pipe2 returned two owned descriptors.
        let (read, write) = unsafe {
            (
                std::os::fd::FromRawFd::from_raw_fd(fds[0]),
                std::os::fd::FromRawFd::from_raw_fd(fds[1]),
            )
        };
        Ok(Self { read, write })
    }

    pub(crate) fn wake(&self) {
        let byte = 1u8;
        // SAFETY: best-effort byte into a non-blocking pipe.
        unsafe { libc::write(self.write.as_raw_fd(), (&byte as *const u8).cast(), 1) };
    }

    pub(crate) fn drain(&self) {
        let mut buffer = [0u8; 64];
        loop {
            // SAFETY: bounded buffer on a non-blocking descriptor.
            let n = unsafe { libc::read(self.read.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            if n <= 0 {
                break;
            }
        }
    }

    pub(crate) fn fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }
}
