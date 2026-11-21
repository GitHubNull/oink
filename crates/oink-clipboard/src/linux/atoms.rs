//! Atom registry and the MIME subset that maps to portable standards.
use crate::model::StandardFormat;
use crate::linux::x11::Conn;
use std::io;

pub(crate) const TARGETS: &str = "TARGETS";
pub(crate) const TIMESTAMP: &str = "TIMESTAMP";
pub(crate) const INCR: &str = "INCR";
pub(crate) const MULTIPLE: &str = "MULTIPLE";
pub(crate) const UTF8: &str = "UTF8_STRING";
pub(crate) const TEXT: &str = "TEXT";
pub(crate) const STRING: &str = "STRING";
pub(crate) const TEXT_PLAIN: &str = "text/plain";
pub(crate) const TEXT_PLAIN_UTF8: &str = "text/plain;charset=utf-8";
pub(crate) const TEXT_HTML: &str = "text/html";
pub(crate) const TEXT_HTML_UTF8: &str = "text/html;charset=utf-8";
pub(crate) const TEXT_RTF: &str = "text/rtf";
pub(crate) const APPLICATION_RTF: &str = "application/rtf";
pub(crate) const IMAGE_PNG: &str = "image/png";
pub(crate) const URI_LIST: &str = "text/uri-list";
pub(crate) const GNOME_COPIED_FILES: &str = "x-special/gnome-copied-files";
pub(crate) const OINK_SELECTION: &str = "OINK_SELECTION";
pub(crate) const OINK_TIMESTAMP: &str = "OINK_TIMESTAMP";

/// Text targets fed by one portable Text representation.
#[cfg(test)]
pub(crate) const TEXT_TARGETS: &[&str] = &[UTF8, TEXT, TEXT_PLAIN, TEXT_PLAIN_UTF8, STRING];
pub(crate) const HTML_TARGETS: &[&str] = &[TEXT_HTML, TEXT_HTML_UTF8];
pub(crate) const RTF_TARGETS: &[&str] = &[TEXT_RTF, APPLICATION_RTF];

pub(crate) struct Atoms {
    pub clipboard: u64,
    pub targets: u64,
    pub timestamp: u64,
    pub incr: u64,
    pub multiple: u64,
    pub integer: u64,
    pub atom_type: u64,
    pub oink_selection: u64,
    pub oink_timestamp: u64,
}

impl Atoms {
    pub(crate) fn load(conn: &Conn) -> io::Result<Self> {
        let names = [
            "CLIPBOARD",
            TARGETS,
            TIMESTAMP,
            INCR,
            MULTIPLE,
            "INTEGER",
            "ATOM",
            OINK_SELECTION,
            OINK_TIMESTAMP,
        ];
        let atoms = conn.intern(&names)?;
        Ok(Self {
            clipboard: atoms[0],
            targets: atoms[1],
            timestamp: atoms[2],
            incr: atoms[3],
            multiple: atoms[4],
            integer: atoms[5],
            atom_type: atoms[6],
            oink_selection: atoms[7],
            oink_timestamp: atoms[8],
        })
    }
}

/// Control atoms never become content representations.
pub(crate) fn is_control(name: &str) -> bool {
    matches!(
        name,
        TARGETS
            | TIMESTAMP
            | INCR
            | MULTIPLE
            | "SAVE_TARGETS"
            | "DELETE"
            | "INSERT_SELECTION"
            | "INSERT_PROPERTY"
            | "LINK"
            | "PIXMAP"
            | "BITMAP"
            | "COLORMAP"
            | "DRAWABLE"
            | "WINDOW"
            | "COMPOUND_TEXT"
    ) || name.is_empty()
}

/// Portable standard for a native X11 target.
pub(crate) fn standard(name: &str) -> Option<StandardFormat> {
    match name {
        UTF8 | TEXT | STRING | TEXT_PLAIN | TEXT_PLAIN_UTF8 | "text/plain;charset=UTF-8" => {
            Some(StandardFormat::Text)
        }
        TEXT_HTML | TEXT_HTML_UTF8 | "text/html;charset=UTF-8" => Some(StandardFormat::Html),
        TEXT_RTF | APPLICATION_RTF => Some(StandardFormat::Rtf),
        IMAGE_PNG => Some(StandardFormat::Png),
        URI_LIST => Some(StandardFormat::Url),
        _ => None,
    }
}

/// ISO-8859-1 is the only defined `STRING` encoding.
pub(crate) fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

pub(crate) fn latin1_bytes(text: &str) -> Option<Vec<u8>> {
    text.chars()
        .map(|ch| u8::try_from(u32::from(ch)).ok())
        .collect::<Option<Vec<u8>>>()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_targets_share_one_standard_and_control_atoms_stay_out() {
        for name in TEXT_TARGETS {
            assert_eq!(standard(name), Some(StandardFormat::Text), "{name}");
        }
        assert_eq!(standard(TEXT_HTML), Some(StandardFormat::Html));
        assert_eq!(standard(APPLICATION_RTF), Some(StandardFormat::Rtf));
        assert_eq!(standard(IMAGE_PNG), Some(StandardFormat::Png));
        assert_eq!(standard(URI_LIST), Some(StandardFormat::Url));
        assert_eq!(standard(TARGETS), None);
        assert!(is_control(TARGETS) && is_control(INCR) && is_control(MULTIPLE));
        assert!(!is_control(UTF8));
    }
    #[test]
    fn latin1_round_trip_rejects_wide_characters() {
        assert_eq!(latin1(b"\x41\xe9"), "A\u{e9}");
        assert_eq!(latin1_bytes("A\u{e9}"), Some(b"A\xe9".to_vec()));
        assert_eq!(latin1_bytes("\u{4e2d}"), None);
    }
}
