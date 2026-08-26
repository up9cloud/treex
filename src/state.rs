//! The shared session. Every view — the TUI, each browser tab, the filesystem
//! watcher — drives the same `Tree` through [`Command`] and observes it through
//! [`Snapshot`]. That is what keeps the terminal and the browser in step.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::tree::{Row, ScanOptions, Tree};

/// Views address nodes by path rather than by `NodeId`: ids are recycled when a
/// refresh drops a subtree, and a browser tab may well send a command based on
/// a snapshot that is already one revision stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Command {
    Toggle {
        path: PathBuf,
    },
    Expand {
        path: PathBuf,
    },
    Collapse {
        path: PathBuf,
    },
    Select {
        path: PathBuf,
    },
    /// Show a file, or `None` to stop. Showing one selects it too: a click in
    /// the browser goes straight to reading, with no separate step.
    View {
        path: Option<PathBuf>,
    },
    SelectRow {
        row: usize,
    },
    MoveSelection {
        delta: i64,
    },
    ExpandDepth {
        depth: usize,
    },
    CollapseAll,
    /// Show or hide dotfiles, and re-read everything on screen.
    SetHidden {
        show: bool,
    },
    Refresh,
    RefreshPath {
        path: PathBuf,
    },
}

impl Command {
    /// Whether applying this reads the filesystem. Callers on an async runtime
    /// use it to decide what has to go to a blocking thread; moving the cursor
    /// does not deserve a thread hop.
    pub fn may_block(&self) -> bool {
        matches!(
            self,
            Command::Toggle { .. }
                | Command::Expand { .. }
                | Command::ExpandDepth { .. }
                | Command::SetHidden { .. }
                | Command::Refresh
                | Command::RefreshPath { .. }
        )
    }
}

/// The cheap half of a [`Snapshot`]: where the cursor is, and whether the rows
/// need resending at all.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cursor {
    pub revision: u64,
    #[serde(skip)]
    pub shape: u64,
    pub selected: Option<usize>,
    pub viewing: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub revision: u64,
    /// Bumped only when `rows` differs from the previous snapshot. Not sent on
    /// the wire: it exists so a server can tell a cursor move from a reshape.
    #[serde(skip)]
    pub shape: u64,
    pub root: PathBuf,
    pub rows: Vec<Row>,
    /// Index into `rows`, absent when the selection is currently hidden.
    pub selected: Option<usize>,
    /// Index into `rows` of the file being read, which need not be `selected`.
    pub viewing: Option<usize>,
    /// Whether dotfiles are currently listed, so a view can show its toggle.
    pub show_hidden: bool,
}

pub struct Session {
    tree: Mutex<Tree>,
    changed: broadcast::Sender<u64>,
}

impl Session {
    pub fn new(root: impl AsRef<Path>, opts: ScanOptions) -> crate::Result<Arc<Self>> {
        let tree = Tree::new(root, opts)?;
        let (changed, _) = broadcast::channel(64);
        Ok(Arc::new(Self {
            tree: Mutex::new(tree),
            changed,
        }))
    }

    pub fn root(&self) -> PathBuf {
        self.tree.lock().unwrap().root_path().to_path_buf()
    }

    /// Fires on every revision bump. Receivers that fall behind get
    /// `Lagged` and should simply take a fresh snapshot.
    pub fn subscribe(&self) -> broadcast::Receiver<u64> {
        self.changed.subscribe()
    }

    /// The canonical path of `path`, but only if it is a file the tree is
    /// currently showing.
    ///
    /// This is the authorization check for serving file contents: membership in
    /// the tree already means the path is under the root and passed whatever
    /// hidden/ignore rules are in force, so no separate traversal check is
    /// needed and none can be forgotten.
    pub fn visible_file(&self, path: &Path) -> Option<Arc<Path>> {
        let tree = self.tree.lock().unwrap();
        let id = tree.id_for_path(path)?;
        let node = tree.get(id)?;
        (!node.is_dir()).then(|| node.path.clone())
    }

    /// Everything a view needs when the tree itself has not changed.
    ///
    /// Deliberately does not flatten: after the cursor/snapshot split this is
    /// the common case, and building the rows only to read four numbers was the
    /// remaining cost of moving the cursor on a large tree.
    pub fn cursor(&self) -> Cursor {
        let tree = self.tree.lock().unwrap();
        Cursor {
            revision: tree.revision,
            shape: tree.shape,
            selected: tree.visible_index(tree.selected),
            viewing: tree
                .viewing
                .as_ref()
                .and_then(|p| tree.id_for_path(p))
                .and_then(|id| tree.visible_index(id)),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let tree = self.tree.lock().unwrap();
        let rows = tree.rows();
        Snapshot {
            revision: tree.revision,
            shape: tree.shape,
            root: tree.root_path().to_path_buf(),
            selected: tree.selected_row(&rows),
            viewing: tree
                .viewing
                .as_ref()
                .and_then(|p| rows.iter().position(|r| &r.path == p)),
            show_hidden: tree.opts.show_hidden,
            rows,
        }
    }

    /// Runs `f` against the tree and notifies subscribers if it bumped the
    /// revision. Callers that need several mutations seen as one update should
    /// do them all inside a single `with_tree`.
    pub fn with_tree<T>(&self, f: impl FnOnce(&mut Tree) -> T) -> T {
        let (out, revision, changed) = {
            let mut tree = self.tree.lock().unwrap();
            let before = tree.revision;
            let out = f(&mut tree);
            (out, tree.revision, tree.revision != before)
        };
        if changed {
            let _ = self.changed.send(revision);
        }
        out
    }

    pub fn apply(&self, cmd: Command) {
        self.with_tree(|tree| {
            let lookup = |tree: &Tree, path: &Path| tree.id_for_path(path);
            match cmd {
                Command::Toggle { path } => {
                    if let Some(id) = lookup(tree, &path) {
                        tree.toggle(id);
                        tree.select(id);
                    }
                }
                Command::Expand { path } => {
                    if let Some(id) = lookup(tree, &path) {
                        tree.expand(id);
                    }
                }
                Command::Collapse { path } => {
                    if let Some(id) = lookup(tree, &path) {
                        tree.collapse(id);
                    }
                }
                Command::Select { path } => {
                    if let Some(id) = lookup(tree, &path) {
                        tree.select(id);
                    }
                }
                Command::View { path } => {
                    let file = path.and_then(|p| {
                        let id = lookup(tree, p.as_path())?;
                        tree.get(id)
                            .filter(|n| !n.is_dir())
                            .map(|n| (id, n.path.clone()))
                    });
                    match file {
                        Some((id, path)) => {
                            tree.select(id);
                            tree.view(Some(path));
                        }
                        None => tree.view(None),
                    }
                }
                Command::SelectRow { row } => {
                    let rows = tree.rows();
                    if let Some(r) = rows.get(row) {
                        tree.select(r.id);
                    }
                }
                Command::MoveSelection { delta } => {
                    let rows = tree.rows();
                    if rows.is_empty() {
                        return;
                    }
                    let cur = tree.selected_row(&rows).unwrap_or(0) as i64;
                    let next = (cur + delta).clamp(0, rows.len() as i64 - 1) as usize;
                    tree.select(rows[next].id);
                }
                Command::ExpandDepth { depth } => {
                    let root = tree.root();
                    tree.expand_to_depth(root, depth);
                }
                Command::CollapseAll => tree.collapse_all(),
                Command::SetHidden { show } => {
                    if tree.opts.show_hidden != show {
                        tree.opts.show_hidden = show;
                        tree.refresh_all();
                    }
                }
                Command::Refresh => tree.refresh_all(),
                Command::RefreshPath { path } => tree.refresh_path(&path),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_observe_each_others_commands() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        let session = Session::new(dir.path(), ScanOptions::default()).unwrap();

        let mut rx = session.subscribe();
        let sub = session.root().join("sub");
        session.apply(Command::Toggle { path: sub.clone() });

        assert!(rx.try_recv().is_ok(), "a toggle should notify subscribers");
        let snap = session.snapshot();
        let row = snap.rows.iter().find(|r| *r.path == *sub).unwrap();
        assert!(row.expanded);
        assert_eq!(snap.selected, Some(1));
    }

    #[test]
    fn a_no_op_command_does_not_notify() {
        let dir = tempfile::tempdir().unwrap();
        let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
        let mut rx = session.subscribe();
        session.apply(Command::Toggle {
            path: "/nonexistent/path".into(),
        });
        assert!(rx.try_recv().is_err());
    }
}
