//! Reading a single directory level.

use std::path::{Path, PathBuf};

use crate::tree::{Kind, ScanOptions};

pub struct Entry {
    pub path: PathBuf,
    pub kind: Kind,
    /// Whether the entry itself is a symlink, whatever it points at.
    pub symlink: bool,
    pub size: u64,
}

/// Lists the immediate children of `dir`, directories first and then by name.
///
/// Only one level is ever read, so this needs no directory walker. Unreadable
/// directories come back empty rather than erroring — a permission-denied
/// folder should still be visible in the tree, just without contents.
pub fn read_dir(dir: &Path, opts: &ScanOptions) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            if !opts.show_hidden && e.file_name().to_string_lossy().starts_with('.') {
                return None;
            }

            let path = e.path();
            let link = e.path().symlink_metadata().ok()?.is_symlink();
            // A symlink is shown as whatever it points at, so that a link to a
            // directory opens like one; `symlink` keeps the fact it was a link.
            let meta = std::fs::metadata(&path).ok();
            let kind = match meta.as_ref() {
                Some(meta) => classify(meta),
                None if link => Kind::Broken,
                None => return None,
            };

            if opts.dirs_only && kind != Kind::Dir {
                return None;
            }
            Some(Entry {
                path,
                kind,
                symlink: link,
                size: meta.map(|m| m.len()).unwrap_or(0),
            })
        })
        .collect();

    entries.sort_by(|a, b| {
        (b.kind == Kind::Dir)
            .cmp(&(a.kind == Kind::Dir))
            .then_with(|| by_name(&a.path, &b.path))
    });
    entries
}

#[cfg(unix)]
fn classify(meta: &std::fs::Metadata) -> Kind {
    use std::os::unix::fs::FileTypeExt;

    let t = meta.file_type();
    if t.is_dir() {
        Kind::Dir
    } else if t.is_socket() {
        Kind::Socket
    } else if t.is_fifo() {
        Kind::Fifo
    } else if t.is_char_device() {
        Kind::CharDevice
    } else if t.is_block_device() {
        Kind::BlockDevice
    } else {
        Kind::File
    }
}

#[cfg(not(unix))]
fn classify(meta: &std::fs::Metadata) -> Kind {
    if meta.is_dir() {
        Kind::Dir
    } else {
        Kind::File
    }
}

fn by_name(a: &Path, b: &Path) -> std::cmp::Ordering {
    let a = a
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let b = b
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    a.cmp(&b)
}
