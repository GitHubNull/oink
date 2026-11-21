//! Read the current CLIPBOARD as metadata only. Payload bytes are fetched when
//! a consumer opens a representation, and every read revalidates the owner.
use crate::content::namespace::Local;
use crate::content::{self, Provider, Translator};
use crate::formats;
use crate::linux::atoms;
use crate::linux::selection::{self, Requestor};
use crate::linux::{Engine, Identity, platform, uris};
use crate::model::{
    ClipId, ContentItem, ContentSize, Inventory, ItemId, MAX_REPRESENTATIONS, Medium, NativeFormat, NodeId, OfferId,
    REPRESENTATION_BYTES, Representation, RepresentationData, RepresentationId, StandardFormat, Unavailable,
};
use crate::{CapturedClipboard, ClipData, ClipSnapshot, ClipboardError, Sensitivity};
use oink_foundation::DeviceId;
use oink_foundation::clipboard::ClipPlatform;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

/// Privacy control values are small metadata, never content payloads.
const PRIVACY_INLINE: usize = 65536;

pub(crate) fn capture(engine: &Engine) -> Result<CapturedClipboard, ClipboardError> {
    let requestor = Requestor::open().map_err(platform)?;
    let identity = requestor.identity().map_err(platform)?;
    let (mut inventory, files) = match identity {
        Some(identity) => {
            let (inventory, files) = inventory(&requestor).map_err(|error| {
                ClipboardError::ContentUnavailable(content::reason(&error))
            })?;
            // A conversion and every metadata read above belong to one owner.
            if requestor.identity().map_err(platform)? != Some(identity) {
                return Err(ClipboardError::Busy);
            }
            engine.note_captured(identity);
            (inventory, files)
        }
        None => (empty_inventory(), None),
    };
    let count = engine.count();
    inventory.id = offer(count);
    let sensitivity = sensitivity_of(&inventory);
    let check: content::clipboard::Check = Arc::new(move || match identity {
        Some(expected) if selection::matches(expected)? => Ok(()),
        Some(_) => Err(content::clipboard::changed()),
        None => Ok(()),
    });
    let source = inventory.clone();
    let expected = identity;
    let read = Box::new(move |id: RepresentationId| {
        let expected = expected.ok_or_else(|| content::unavailable(Unavailable::NoSuchItem))?;
        read_representation(expected, &source, id)
    });
    let native = content::clipboard::Clipboard::new(inventory, read, check, Arc::new(LinuxTranslator), files)
        .map_err(|error| ClipboardError::ContentUnavailable(content::reason(&error)))?;
    Ok(CapturedClipboard {
        snapshot: ClipSnapshot {
            items: content::items(&native.inventory()),
            sensitivity,
            change_count: count,
        },
        content: Some(native),
    })
}

fn offer(generation: u64) -> OfferId {
    OfferId {
        clip: ClipId {
            origin: DeviceId([0; 32]),
            seq: 0,
        },
        generation,
    }
}

fn empty_inventory() -> Inventory {
    Inventory {
        id: offer(0),
        platform: ClipPlatform::Linux,
        items: Vec::new(),
        roots: Vec::new(),
    }
}

/// TARGETS order is preserved; file lists become a namespace of local paths
/// instead of byte representations. Unknown targets stay opaque so
/// same-machine peers can round-trip private formats.
fn inventory(
    requestor: &Requestor,
) -> io::Result<(Inventory, Option<Arc<dyn content::namespace::Namespace>>)> {
    let targets = requestor.targets()?;
    let mut seen = std::collections::HashSet::new();
    let mut representations = Vec::new();
    let mut file_lists = Vec::new();
    for atom in targets {
        if !seen.insert(atom) {
            continue;
        }
        let Some(name) = requestor.conn.atom_name(atom) else {
            continue;
        };
        if atoms::is_control(&name) {
            continue;
        }
        if name == atoms::GNOME_COPIED_FILES {
            file_lists.push((atom, name, true));
            continue;
        }
        if name == atoms::URI_LIST {
            // Classify before fetching: file copies are never exposed as URLs.
            match requestor.file_list_target(atom) {
                Ok(Some(true)) => {
                    file_lists.push((atom, name, false));
                    continue;
                }
                Ok(_) => {}
                Err(_) => continue,
            }
        }
        if representations.len() >= MAX_REPRESENTATIONS {
            return Err(content::unavailable(Unavailable::LimitExceeded));
        }
        let standard = if formats::is_marker(ClipPlatform::Linux, &name) {
            Some(StandardFormat::Privacy)
        } else {
            atoms::standard(&name)
        };
        let data = if standard == Some(StandardFormat::Privacy) {
            match requestor.bytes(atom, PRIVACY_INLINE) {
                Ok(bytes) if bytes.len() <= PRIVACY_INLINE => RepresentationData::Inline(bytes),
                _ => RepresentationData::Reader {
                    size: ContentSize::Unknown,
                },
            }
        } else {
            RepresentationData::Reader {
                size: ContentSize::Unknown,
            }
        };
        representations.push(Representation {
            id: RepresentationId(representations.len() as u32 + 1),
            format: NativeFormat::LinuxAtom(name),
            medium: Medium::Bytes,
            aspect: 1,
            index: -1,
            standard,
            data,
        });
    }
    let (file_names, mut files) = file_namespace(requestor, &file_lists);
    let mut items = Vec::new();
    let mut next_rep = representations.len() as u32 + 1;
    if !representations.is_empty() {
        items.push(ContentItem {
            id: ItemId(0),
            virtual_files: false,
            file: None,
            representations,
        });
    }
    if let Some(namespace) = &files {
        // A link-only list has no publishable root on Linux.
        let anchor = namespace
            .roots()
            .iter()
            .find(|node| node.kind.symlink_target().is_none())
            .map(|node| node.id);
        match anchor {
            Some(anchor) if !file_names.is_empty() => {
                let file_reps: Vec<_> = file_names
                    .into_iter()
                    .map(|name| {
                        let rep = Representation {
                            id: RepresentationId(next_rep),
                            format: NativeFormat::LinuxAtom(name),
                            medium: Medium::Bytes,
                            aspect: 1,
                            index: -1,
                            standard: None,
                            data: RepresentationData::File(anchor),
                        };
                        next_rep += 1;
                        rep
                    })
                    .collect();
                items.push(ContentItem {
                    id: ItemId(items.len() as u32),
                    virtual_files: false,
                    file: Some(anchor),
                    representations: file_reps,
                });
            }
            _ => files = None,
        }
    }
    let inventory = Inventory {
        id: offer(0),
        platform: ClipPlatform::Linux,
        items,
        roots: Vec::new(),
    };
    inventory.validate().map_err(content::unavailable)?;
    Ok((inventory, files))
}

/// File-list payloads are list metadata, not file contents: every path is
/// decoded through `uris` and rebuilt as a real local namespace.
fn file_namespace(
    requestor: &Requestor,
    lists: &[(u64, String, bool)],
) -> (Vec<String>, Option<Arc<dyn content::namespace::Namespace>>) {
    let mut files: Option<Arc<dyn content::namespace::Namespace>> = None;
    let mut names = Vec::new();
    for (atom, name, gnome) in lists {
        let Ok(bytes) = requestor.bytes(*atom, REPRESENTATION_BYTES as usize) else {
            continue;
        };
        let Ok(paths) = uris::parse(&bytes, *gnome) else {
            continue;
        };
        if paths.is_empty() {
            continue;
        }
        if files.is_none() {
            let roots: Vec<(NodeId, PathBuf)> = paths
                .iter()
                .enumerate()
                .map(|(index, path)| (NodeId(index as u64 + 1), path.clone()))
                .collect();
            match Local::clipboard(roots) {
                Ok(namespace) => files = Some(namespace),
                Err(_) => return (Vec::new(), None),
            }
        }
        names.push(name.clone());
    }
    (names, files)
}

fn read_representation(expected: Identity, inventory: &Inventory, id: RepresentationId) -> io::Result<crate::FileReader> {
    let rep = content::find(inventory, id).ok_or_else(|| content::unavailable(Unavailable::NoSuchItem))?;
    if matches!(rep.data, RepresentationData::File(_)) {
        // File reps are opened through the namespace, never as X11 properties.
        return Err(content::unavailable(Unavailable::Unsupported));
    }
    let NativeFormat::LinuxAtom(name) = &rep.format else {
        return Err(content::unavailable(Unavailable::Unsupported));
    };
    let requestor = Requestor::open()?;
    if requestor.identity()? != Some(expected) {
        return Err(content::clipboard::changed());
    }
    let atom = requestor.conn.atom(name, true)?;
    if atom == 0 {
        return Err(content::unavailable(Unavailable::NoSuchItem));
    }
    let bytes = requestor.bytes(atom, REPRESENTATION_BYTES as usize)?;
    if requestor.identity()? != Some(expected) {
        return Err(content::clipboard::changed());
    }
    Ok(Box::new(io::Cursor::new(bytes)))
}

fn sensitivity_of(inventory: &Inventory) -> Sensitivity {    inventory
        .items
        .iter()
        .flat_map(|item| &item.representations)
        .fold(Sensitivity::Normal, |level, rep| {
            let NativeFormat::LinuxAtom(name) = &rep.format else {
                return level;
            };
            let RepresentationData::Inline(bytes) = &rep.data else {
                return level;
            };
            let next = if name == formats::LINUX_MARKERS[1] {
                Sensitivity::Transient
            } else if name == formats::LINUX_MARKERS[0] {
                if bytes.as_slice() == formats::LINUX_SECRET {
                    Sensitivity::Concealed
                } else {
                    return level;
                }
            } else if name == formats::LINUX_MARKERS[2] {
                Sensitivity::AutoGenerated
            } else {
                return level;
            };
            if rank(next) > rank(level) { next } else { level }
        })
}

fn rank(level: Sensitivity) -> u8 {
    match level {
        Sensitivity::Normal => 0,
        Sensitivity::AutoGenerated => 1,
        Sensitivity::Concealed => 2,
        Sensitivity::Transient => 3,
    }
}

struct LinuxTranslator;
impl Translator for LinuxTranslator {
    fn standard(&self, rep: &Representation, bytes: &[u8]) -> io::Result<Vec<ClipData>> {
        let NativeFormat::LinuxAtom(name) = &rep.format else {
            return Err(content::unavailable(Unavailable::Unsupported));
        };
        let data = match rep.standard {
            Some(StandardFormat::Text) => ClipData::Text(normalize_newlines(&decode_text(name, bytes)?)),
            Some(StandardFormat::Html) => ClipData::Html(decode_utf8(bytes)?),
            Some(StandardFormat::Rtf) => ClipData::Rtf(bytes.to_vec()),
            Some(StandardFormat::Png) => {
                if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                    return Err(content::unavailable(Unavailable::InvalidData));
                }
                ClipData::Png(bytes.to_vec())
            }
            Some(StandardFormat::Privacy) => formats::custom(ClipPlatform::Linux, name, bytes.to_vec()),
            Some(StandardFormat::Url) | None => return Err(content::unavailable(Unavailable::Unsupported)),
        };
        Ok(vec![data])
    }
}

fn decode_text(name: &str, bytes: &[u8]) -> io::Result<String> {
    if name == atoms::STRING {
        return Ok(atoms::latin1(bytes));
    }
    decode_utf8(bytes)
}

fn decode_utf8(bytes: &[u8]) -> io::Result<String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| content::unavailable(Unavailable::InvalidData))
}

/// Text newlines cross systems as LF; lone carriage returns are preserved.
fn normalize_newlines(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' && chars.peek() == Some(&'\n') {
            continue;
        }
        out.push(ch);
    }
    out
}

// The clipboard snapshot is consumed by the wire encoder on another thread.
#[cfg(all(test, debug_assertions))]
mod tests {
    use super::*;

    #[test]
    fn text_newlines_are_normalized_and_latin1_is_decoded() {
        assert_eq!(normalize_newlines("a\r\nb\nc\r"), "a\nb\nc\r");
        let rep = Representation {
            id: RepresentationId(1),
            format: NativeFormat::LinuxAtom(atoms::STRING.into()),
            medium: Medium::Bytes,
            aspect: 1,
            index: -1,
            standard: Some(StandardFormat::Text),
            data: RepresentationData::Reader {
                size: ContentSize::Unknown,
            },
        };
        let items = LinuxTranslator.standard(&rep, b"A\xe9").unwrap();
        assert_eq!(items, vec![ClipData::Text("A\u{e9}".into())]);
    }

    #[test]
    fn sensitivity_takes_the_strictest_linux_marker() {
        let mut inventory = empty_inventory();
        inventory.items = vec![ContentItem {
            id: ItemId(0),
            virtual_files: false,
            file: None,
            representations: vec![
                marker_rep(1, formats::LINUX_MARKERS[2], b""),
                marker_rep(2, formats::LINUX_MARKERS[0], formats::LINUX_SECRET),
            ],
        }];
        assert_eq!(sensitivity_of(&inventory), Sensitivity::Concealed);
        inventory.items[0].representations[1] = marker_rep(2, formats::LINUX_MARKERS[1], b"");
        assert_eq!(sensitivity_of(&inventory), Sensitivity::Transient);
        inventory.items[0].representations[1] = marker_rep(2, formats::LINUX_MARKERS[0], b"not-secret");
        assert_eq!(sensitivity_of(&inventory), Sensitivity::AutoGenerated);
    }

    fn marker_rep(id: u32, name: &str, bytes: &[u8]) -> Representation {
        Representation {
            id: RepresentationId(id),
            format: NativeFormat::LinuxAtom(name.into()),
            medium: Medium::Bytes,
            aspect: 1,
            index: -1,
            standard: Some(StandardFormat::Privacy),
            data: RepresentationData::Inline(bytes.to_vec()),
        }
    }
}
