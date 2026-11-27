//! Catcher (target) side of XDND: the drag session plus the eager capture
//! worker that reads every offered representation while the foreign drag is
//! still alive, then builds the provider the agent registers.
use super::window::{self, Xdnd};
use crate::content::{self, namespace::Local};
use crate::dnd::{DragOperation, DragOps, PlatformDndEvent};
use crate::formats;
use crate::linux::atoms;
use crate::linux::capture::LinuxTranslator;
use crate::linux::selection::Requestor;
use crate::linux::uris;
use crate::linux::x11::Conn;
use crate::model::{
    ContentItem, ContentSize, Inventory, ItemId, Medium, NativeFormat, NodeId, REPRESENTATION_BYTES,
    Representation, RepresentationData, RepresentationId, StandardFormat, Unavailable,
};
use crate::{ClipData, FileEntry};
use oink_foundation::Point;
use oink_foundation::clipboard::ClipPlatform;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// All standard reps are captured eagerly behind the shared representation
/// ceiling; a single huge peer cannot pin the drag.
const CAPTURE_TOTAL: u64 = REPRESENTATION_BYTES;
/// Privacy control values are small metadata, never content payloads.
const PRIVACY_INLINE: usize = 65536;

/// The capture outcome for one foreign drag, delivered by the worker thread.
pub(super) enum ForeignResult {
    Done(Arc<Capture>),
    Failed,
}

/// Snapshot of one foreign drag: items plus a provider served from memory.
pub(super) struct Capture {
    pub(super) operation: DragOperation,
    pub(super) provider: Arc<dyn content::Provider>,
    pub(super) items: Vec<ClipData>,
}

/// One drag session on the catcher, keyed by the source window.
pub(super) struct Foreign {
    pub(super) operation: DragOperation,
    pub(super) source: u64,
    /// True once finish(accept) was submitted; the next Drop is the final one.
    pub(super) finishing: bool,
    pub(super) accepted: bool,
    /// Receives the eager capture outcome; set to None once consumed.
    pending: Arc<Mutex<Option<ForeignResult>>>,
    /// The capture outcome was consumed; `usable` says whether it succeeded.
    reported: bool,
    usable: bool,
    finishing_since: Option<Instant>,
}

impl Foreign {
    pub(super) fn new(operation: DragOperation, source: u64, types: Vec<String>) -> Self {
        let pending: Arc<Mutex<Option<ForeignResult>>> = Arc::new(Mutex::new(None));
        spawn_capture(operation, source, types, &pending);
        Self {
            operation,
            source,
            finishing: false,
            accepted: false,
            pending,
            reported: false,
            usable: false,
            finishing_since: None,
        }
    }

    pub(super) fn usable(&self) -> bool {
        self.usable
    }

    pub(super) fn reported(&self) -> bool {
        self.reported
    }

    pub(super) fn mark_reported(&mut self, usable: bool) {
        self.reported = true;
        self.usable = usable;
    }

    pub(super) fn start_finish(&mut self) {
        self.finishing_since = Some(Instant::now());
    }

    pub(super) fn finish_age(&self) -> Option<std::time::Duration> {
        self.finishing_since.map(|since| since.elapsed())
    }

    /// Take the capture outcome, if the worker has finished.
    pub(super) fn take_capture(&mut self) -> Option<ForeignResult> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// XDND replies name the sending target window in data[0]; GTK matches
    /// its drag context against that xid and drops mismatched messages.
    pub(super) fn reply_status(&self, conn: &Conn, xdnd: &Xdnd, from: u64, accept: bool) {
        conn.send_client_message(
            self.source,
            xdnd.status,
            window::status(from, accept, true, xdnd.action_copy),
        );
    }

    /// Reject the drop: a Status carrying action None clears the source's
    /// selected action, so its button release cancels the drag instead of
    /// dropping. XdndLeave is source-directed by the spec; a target-sent
    /// Leave never reaches the source's drag state in GTK.
    pub(super) fn reply_reject(&self, conn: &Conn, xdnd: &Xdnd, from: u64) {
        conn.send_client_message(self.source, xdnd.status, window::status(from, false, false, 0));
    }

    pub(super) fn reply_finished(&self, conn: &Conn, xdnd: &Xdnd, from: u64) {
        conn.send_client_message(
            self.source,
            xdnd.finished,
            window::finished(from, self.accepted, xdnd.action_copy),
        );
    }
}

/// Spawn the eager capture for a foreign drag; the worker uses its own X
/// connection (the drag thread must keep answering Position/Status).
fn spawn_capture(
    operation: DragOperation,
    source: u64,
    types: Vec<String>,
    done: &Arc<Mutex<Option<ForeignResult>>>,
) {
    let pending = done.clone();
    let spawned = std::thread::Builder::new()
        .name("oink-x11-dnd-capture".into())
        .spawn(move || {
            let result = match capture(operation, source, &types) {
                Ok(capture) => ForeignResult::Done(Arc::new(capture)),
                Err(_) => ForeignResult::Failed,
            };
            *pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        });
    if spawned.is_err() {
        *done.lock().unwrap_or_else(|e| e.into_inner()) = Some(ForeignResult::Failed);
    }
}

/// Read every offered target through XdndSelection while the drag is alive and
/// build the provider the agent registers for the session. Reads are bounded
/// so a slow peer can never block the whole target.
fn capture(operation: DragOperation, source: u64, types: &[String]) -> io::Result<Capture> {
    let started = Instant::now();
    let requestor = Requestor::open_for("XdndSelection")?;
    if requestor.conn.selection_owner(requestor.selection) != source {
        return Err(content::unavailable(Unavailable::NoSuchItem));
    }
    let mut remaining = CAPTURE_TOTAL;
    let mut reps: Vec<(String, Vec<u8>)> = Vec::new();
    for name in types {
        let standard = if formats::is_marker(ClipPlatform::Linux, name) {
            Some(StandardFormat::Privacy)
        } else {
            atoms::standard(name)
        };
        let wanted = standard.is_some() || formats::linux_custom(name);
        if !wanted || reps.iter().any(|(existing, _)| existing == name) {
            continue;
        }
        let Ok(atom) = requestor.conn.atom(name, true) else {
            continue;
        };
        if atom == 0 {
            continue;
        }
        let limit = remaining
            .min(if standard == Some(StandardFormat::Privacy) {
                PRIVACY_INLINE as u64
            } else {
                REPRESENTATION_BYTES
            }) as usize;
        let Ok(bytes) = requestor.bytes(atom, limit.max(1)) else {
            continue;
        };
        remaining = remaining.saturating_sub(bytes.len() as u64);
        reps.push((name.clone(), bytes));
        if remaining == 0 {
            break;
        }
    }
    if reps.is_empty() {
        return Err(content::unavailable(Unavailable::ReadFailed));
    }
    let snapshot: Arc<Mutex<std::collections::HashMap<u32, Vec<u8>>>> = Arc::new(Mutex::new(
        reps.iter().enumerate().map(|(index, (_, bytes))| (index as u32 + 1, bytes.clone())).collect(),
    ));
    let mut items: Vec<ClipData> = Vec::new();
    let mut representations = Vec::new();
    let mut files: Option<Arc<dyn content::namespace::Namespace>> = None;
    let mut next_id = 1u32;
    for (name, bytes) in &reps {
        let standard = if formats::is_marker(ClipPlatform::Linux, name) {
            Some(StandardFormat::Privacy)
        } else {
            atoms::standard(name)
        };
        if standard == Some(StandardFormat::Privacy) {
            items.push(formats::custom(ClipPlatform::Linux, name, bytes.clone()));
            representations.push(representation(next_id, name, standard, bytes.len() as u64));
            next_id += 1;
            continue;
        }
        if name == atoms::URI_LIST || name == atoms::GNOME_COPIED_FILES {
            if files.is_none()
                && let Ok(paths) = uris::parse(bytes, name == atoms::GNOME_COPIED_FILES)
                && !paths.is_empty()
            {
                items.push(ClipData::Files(file_entries(&paths)));
                files = Some(local_files(&paths));
            }
            continue;
        }
        items.extend(translate_item(name, bytes));
        representations.push(representation(next_id, name, standard, bytes.len() as u64));
        next_id += 1;
    }
    let inventory = Inventory {
        id: oink_foundation::content::OfferId {
            clip: oink_foundation::content::ClipId {
                origin: oink_foundation::DeviceId([0; 32]),
                seq: 0,
            },
            generation: operation.sequence,
        },
        platform: ClipPlatform::Linux,
        items: vec![ContentItem {
            id: ItemId(0),
            virtual_files: false,
            file: None,
            representations,
        }],
        roots: Vec::new(),
    };
    inventory.validate().map_err(content::unavailable)?;
    let read_snapshot = snapshot.clone();
    let read: content::clipboard::ReadRepresentation = Box::new(move |id| {
        let bytes = read_snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id.0)
            .cloned()
            .ok_or_else(|| content::unavailable(Unavailable::NoSuchItem))?;
        Ok(Box::new(io::Cursor::new(bytes)) as crate::FileReader)
    });
    let check: content::clipboard::Check = Arc::new(|| Ok(()));
    let provider =
        content::clipboard::Clipboard::new(inventory, read, check, Arc::new(LinuxTranslator), files)?;
    crate::dnd::preparation_timing(
        crate::dnd::PreparationStage::SourceRead,
        started,
        Instant::now(),
    );
    Ok(Capture {
        operation,
        provider,
        items,
    })
}

fn representation(id: u32, name: &str, standard: Option<StandardFormat>, size: u64) -> Representation {
    Representation {
        id: RepresentationId(id),
        format: NativeFormat::LinuxAtom(name.to_owned()),
        medium: Medium::Bytes,
        aspect: 1,
        index: -1,
        standard,
        data: RepresentationData::Reader {
            size: ContentSize::Known(size),
        },
    }
}

fn local_files(paths: &[PathBuf]) -> Arc<dyn content::namespace::Namespace> {
    let roots: Vec<(NodeId, PathBuf)> = paths
        .iter()
        .enumerate()
        .map(|(index, path)| (NodeId(index as u64 + 1), path.clone()))
        .collect();
    // The caller only builds a namespace for lists it already parsed, so the
    // fallback empty namespace is unreachable in practice.
    match Local::clipboard(roots) {
        Ok(local) => local,
        Err(_) => Arc::new(EmptyNamespace),
    }
}

struct EmptyNamespace;
impl content::namespace::Namespace for EmptyNamespace {
    fn roots(&self) -> Vec<crate::model::FileNode> {
        Vec::new()
    }
    fn list(
        &self,
        _node: NodeId,
        _cursor: Option<crate::model::DirectoryCursor>,
    ) -> io::Result<crate::model::DirectoryPage> {
        Err(content::unavailable(Unavailable::Unsupported))
    }
    fn select(&self, _node: NodeId) -> io::Result<crate::model::FileNode> {
        Err(content::unavailable(Unavailable::Unsupported))
    }
    fn open(&self, _node: NodeId, _version: u64, _offset: u64) -> io::Result<crate::FileReader> {
        Err(content::unavailable(Unavailable::Unsupported))
    }
    fn cancel(&self) {}
}

fn file_entries(paths: &[PathBuf]) -> Vec<FileEntry> {
    paths
        .iter()
        .map(|path| {
            let metadata = std::fs::symlink_metadata(path).ok();
            let is_dir = metadata.as_ref().is_some_and(|m| m.is_dir());
            let size = if is_dir { 0 } else { metadata.as_ref().map_or(0, |m| m.len()) };
            let modified_ms = metadata
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64);
            FileEntry {
                path: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                is_dir,
                size,
                modified_ms,
                local_path: Some(path.clone()),
            }
        })
        .collect()
}

fn translate_item(name: &str, bytes: &[u8]) -> Vec<ClipData> {
    let Some(standard) = atoms::standard(name) else {
        return Vec::new();
    };
    match standard {
        StandardFormat::Text => {
            let text = if name == atoms::STRING {
                atoms::latin1(bytes)
            } else {
                String::from_utf8_lossy(bytes).into_owned()
            };
            vec![ClipData::Text(text)]
        }
        StandardFormat::Html => vec![ClipData::Html(String::from_utf8_lossy(bytes).into_owned())],
        StandardFormat::Rtf => vec![ClipData::Rtf(bytes.to_vec())],
        StandardFormat::Png => {
            if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                vec![ClipData::Png(bytes.to_vec())]
            } else {
                Vec::new()
            }
        }
        StandardFormat::Url | StandardFormat::Privacy => Vec::new(),
    }
}

/// The sink reports a recognized foreign drag; the backend emits Detected.
pub(super) fn detected_event(operation: DragOperation, items: Vec<ClipData>) -> PlatformDndEvent {
    PlatformDndEvent::Detected {
        operation,
        items,
        ops: DragOps::COPY,
    }
}

/// Normalize the requested probe point into catcher window geometry: 8 px
/// across the approached edge, 64 px along it. The normal axis keeps the old
/// footprint, but the tangent length covers probe follow lag (12 reference
/// units between `Probe` updates) so a drag sliding along the edge cannot
/// leave the window before an authorized hold replaces it with the root span.
pub(super) fn catcher_geometry(at: Point, root: (u32, u32)) -> (i32, i32, u32, u32) {
    const DEPTH: i64 = 8;
    const LONG: i64 = 64;
    let width = i64::from(root.0);
    let height = i64::from(root.1);
    let x = at.x.round() as i64;
    let y = at.y.round() as i64;
    let along = |start: i64, limit: i64| start.clamp(0, (limit - LONG).max(0)) as i32;
    let near = |value: i64, limit: i64| (value - DEPTH / 2).clamp(0, (limit - DEPTH).max(0)) as i32;
    let left_or_right = x.abs().min((width - 1 - x).abs()) <= y.abs().min((height - 1 - y).abs());
    if left_or_right {
        (near(x, width), along(y - LONG / 2, height), DEPTH as u32, LONG as u32)
    } else {
        (along(x - LONG / 2, width), near(y, height), LONG as u32, DEPTH as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catcher_geometry_extends_along_the_near_edge_and_clamps() {
        let root = (1920, 1080);
        assert_eq!(catcher_geometry(Point::new(2.0, 500.0), root), (0, 468, 8, 64));
        assert_eq!(catcher_geometry(Point::new(1918.0, 500.0), root), (1912, 468, 8, 64));
        assert_eq!(catcher_geometry(Point::new(900.0, 1.0), root), (868, 0, 64, 8));
        assert_eq!(catcher_geometry(Point::new(900.0, 1078.0), root), (868, 1072, 64, 8));
        assert_eq!(catcher_geometry(Point::new(1.0, 3.0), root), (0, 0, 8, 64));
        assert_eq!(catcher_geometry(Point::new(0.0, 1075.0), root), (0, 1016, 8, 64));
        assert_eq!(catcher_geometry(Point::new(100.0, 100.0), root), (96, 68, 8, 64));
    }

    #[test]
    fn text_translation_decodes_latin1_and_drops_bad_png() {
        assert_eq!(
            translate_item(atoms::STRING, b"A\xe9"),
            vec![ClipData::Text("A\u{e9}".into())]
        );
        assert_eq!(translate_item(atoms::UTF8, "hi".as_bytes()), vec![ClipData::Text("hi".into())]);
        assert!(translate_item(atoms::IMAGE_PNG, b"notpng").is_empty());
        assert!(translate_item(atoms::URI_LIST, b"file:///x").is_empty());
    }
}
