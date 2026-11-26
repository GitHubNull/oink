//! file:// URI rendering and the safe subset accepted from other X11 clients.
//! Only real local paths survive: host authorities, traversal and NUL bytes
//! are refused so a peer can never name a file outside its own selection.
use std::io;
use std::path::{Component, Path, PathBuf};

pub(crate) const MAX_PATHS: usize = 100_000;

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "file list")
}

/// Parse a `text/uri-list` (CRLF, comments skipped) or GNOME's copied-files
/// flavour, whose first line is `copy` or `cut`.
pub(crate) fn parse(bytes: &[u8], gnome: bool) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut header = gnome;
    for raw in bytes.split(|byte| *byte == b'\n') {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        let line = trim(line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        if header {
            header = false;
            if line == b"copy" || line == b"cut" {
                continue;
            }
        }
        let rest = line.strip_prefix(b"file://").ok_or_else(invalid)?;
        let (authority, tail) = match rest.iter().position(|byte| *byte == b'/') {
            Some(index) => (&rest[..index], &rest[index + 1..]),
            None => (rest, &b""[..]),
        };
        if !authority.is_empty() && authority != b"localhost" {
            return Err(invalid());
        }
        if tail.is_empty() {
            return Err(invalid());
        }
        let decoded = unescape(tail).ok_or_else(invalid)?;
        if decoded.contains(&0) {
            return Err(invalid());
        }
        let text = String::from_utf8(decoded).map_err(|_| invalid())?;
        let path = PathBuf::from(format!("/{text}"));
        if !local_absolute(&path) {
            return Err(invalid());
        }
        if seen.insert(path.clone()) {
            if paths.len() >= MAX_PATHS {
                return Err(io::Error::new(io::ErrorKind::FileTooLarge, "file list"));
            }
            paths.push(path);
        }
    }
    Ok(paths)
}

fn trim(line: &[u8]) -> &[u8] {
    let start = line.iter().position(|byte| !byte.is_ascii_whitespace()).unwrap_or(line.len());
    let end = line.iter().rposition(|byte| !byte.is_ascii_whitespace()).map_or(start, |index| index + 1);
    &line[start..end]
}

fn local_absolute(path: &Path) -> bool {
    let mut components = path.components();
    components.next() == Some(Component::RootDir) && components.all(|part| matches!(part, Component::Normal(_)))
}

fn unescape(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex(*bytes.get(index + 1)?)?;
            let low = hex(*bytes.get(index + 2)?)?;
            out.push(high * 16 + low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    Some(out)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/')
}

fn write_uri(out: &mut Vec<u8>, path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    out.extend_from_slice(b"file://");
    for &byte in path.as_os_str().as_bytes() {
        if unreserved(byte) {
            out.push(byte);
        } else {
            out.extend_from_slice(format!("%{byte:02X}").as_bytes());
        }
    }
}

/// `text/uri-list` bytes: CRLF-separated URIs.
pub(crate) fn list(paths: &[PathBuf]) -> Vec<u8> {
    let mut out = Vec::new();
    for path in paths {
        write_uri(&mut out, path);
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// `x-special/gnome-copied-files`: a `copy` header, then LF-separated URIs.
/// Nemo's parser keeps a trailing empty line as a real (empty) entry and its
/// copy job then errors on it, so the last URI must not be newline-terminated
/// — match Nemo's own writer, which prepends the LF before each URI.
pub(crate) fn gnome(paths: &[PathBuf]) -> Vec<u8> {
    let mut out = b"copy".to_vec();
    for path in paths {
        out.push(b'\n');
        write_uri(&mut out, path);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_files_survive_escaping_and_duplicates_collapse() {
        let parsed = parse(b"# copied\r\nfile:///tmp/a%20b\r\nfile://localhost/tmp/c\r\nfile:///tmp/a%20b\r\n", false)
            .unwrap();
        assert_eq!(parsed, vec![PathBuf::from("/tmp/a b"), PathBuf::from("/tmp/c")]);
    }

    #[test]
    fn gnome_header_and_cut_are_not_files() {
        let parsed = parse(b"copy\nfile:///tmp/x\nfile:///tmp/y\n", true).unwrap();
        assert_eq!(parsed, vec![PathBuf::from("/tmp/x"), PathBuf::from("/tmp/y")]);
        assert_eq!(parse(b"cut\n", true).unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn foreign_schemes_hosts_and_traversal_are_refused() {
        for line in [
            b"https://example.com/a\r\n".as_slice(),
            b"file://other/tmp/a\r\n".as_slice(),
            b"file:///tmp/../etc/passwd\r\n".as_slice(),
            b"file:///tmp/%00x\r\n".as_slice(),
            b"relative/path\r\n".as_slice(),
            b"file:///\r\n".as_slice(),
            b"file:///tmp/%zz\r\n".as_slice(),
        ] {
            assert!(parse(line, false).is_err(), "{:?}", String::from_utf8_lossy(line));
        }
    }

    #[test]
    fn rendered_lists_round_trip_through_the_parser() {
        let paths = vec![PathBuf::from("/tmp/a b/中.txt"), PathBuf::from("/tmp/x#y")];
        assert_eq!(parse(&list(&paths), false).unwrap(), paths);
        assert_eq!(parse(&gnome(&paths), true).unwrap(), paths);
        assert!(list(&paths).starts_with(b"file:///tmp/a%20b/"));
    }

    #[test]
    fn gnome_flavor_matches_nemo_writer_exactly() {
        // Nemo's parser treats a trailing empty line as a real (empty) file
        // entry, so the last URI must not be newline-terminated.
        let paths = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")];
        assert_eq!(gnome(&paths), b"copy\nfile:///tmp/a\nfile:///tmp/b");
        assert_eq!(gnome(&[]), b"copy");
    }
}
