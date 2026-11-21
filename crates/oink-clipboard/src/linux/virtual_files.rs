//! Local file lists become the two X11 file-list flavours. Remote content has
//! no local path until the FUSE bridge exists, so no path is invented here.
use crate::linux::atoms;
use crate::linux::binding::{Binding, Source};
use crate::linux::uris;
use oink_foundation::clipboard::FileEntry;
use std::path::PathBuf;
use std::sync::Arc;

/// Only top-level entries of the selection name files; children are reached
/// through their root directory. Entries without a local path come from
/// another platform and stay off the X11 clipboard.
pub(crate) fn top_paths(entries: &[FileEntry]) -> Vec<PathBuf> {
    entries
        .iter()
        .filter(|entry| !entry.path.contains('/'))
        .filter_map(|entry| entry.local_path.clone())
        .collect()
}

pub(crate) fn add(binding: &mut Binding, paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }
    binding.add(atoms::URI_LIST, Source::Bytes(Arc::new(uris::list(paths))));
    binding.add(
        atoms::GNOME_COPIED_FILES,
        Source::Bytes(Arc::new(uris::gnome(paths))),
    );
}
