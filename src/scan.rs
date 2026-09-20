//! Reading a single directory level.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::tree::{Kind, ScanOptions};

pub struct Entry {
    pub path: PathBuf,
    pub kind: Kind,
    /// Whether the entry itself is a symlink, whatever it points at.
    pub symlink: bool,
    pub size: u64,
    /// Whether git would ignore this. Drawn dimmed; never hidden.
    pub ignored: bool,
}

/// Lists the immediate children of `dir`, directories first and then by name.
///
/// Only one level is ever read, so this needs no directory walker. Unreadable
/// directories come back empty rather than erroring — a permission-denied
/// folder should still be visible in the tree, just without contents.
///
/// `parent_ignored` says the directory itself is ignored, in which case git is
/// not asked about its contents at all.
pub fn read_dir(dir: &Path, opts: &ScanOptions, parent_ignored: bool) -> Vec<Entry> {
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
                ignored: false,
            })
        })
        .collect();

    entries.sort_by(|a, b| {
        (b.kind == Kind::Dir)
            .cmp(&(a.kind == Kind::Dir))
            .then_with(|| by_name(&a.path, &b.path))
    });
    mark_ignored(dir, &mut entries, opts, parent_ignored);
    entries
}

/// Asks git about the whole listing at once, if it is worth asking.
fn mark_ignored(dir: &Path, entries: &mut [Entry], opts: &ScanOptions, parent_ignored: bool) {
    if !opts.git_ignore {
        return;
    }
    // Everything below an ignored directory is ignored: git cannot re-include
    // a path whose parent is excluded, so there is nothing left to ask.
    let ignored: HashSet<OsString> = if parent_ignored {
        HashSet::new()
    } else {
        let names: Vec<OsString> = entries
            .iter()
            .filter_map(|e| e.path.file_name().map(|n| n.to_os_string()))
            .collect();
        crate::git::ignored(dir, &names)
    };

    for entry in entries {
        let name = entry.path.file_name().unwrap_or_default();
        // git never calls its own directory ignored — it is not untracked, it
        // is the repository. It is still not what anyone opened a tree to read.
        entry.ignored = parent_ignored || name == ".git" || ignored.contains(name);
    }
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
