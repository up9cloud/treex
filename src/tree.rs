//! The tree model: an arena of nodes plus the expansion state layered on top.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Serialize, Serializer};

use crate::error::{Error, Result};

pub type NodeId = usize;

/// What an entry actually is.
///
/// Two booleans could not say this. A socket is not a file, a symlink to a
/// directory is not the same as a directory, and a dangling symlink is neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Dir,
    File,
    Socket,
    Fifo,
    CharDevice,
    BlockDevice,
    /// A symlink whose target could not be resolved.
    Broken,
}

impl Kind {
    /// Its slot in the wire's kind/flags byte.
    pub fn code(self) -> u8 {
        match self {
            Kind::Dir => 0,
            Kind::File => 1,
            Kind::Socket => 2,
            Kind::Fifo => 3,
            Kind::CharDevice => 4,
            Kind::BlockDevice => 5,
            Kind::Broken => 6,
        }
    }

    /// The suffix `ls -F` would use.
    pub fn suffix(self) -> &'static str {
        match self {
            Kind::Dir => "/",
            Kind::Socket => "=",
            Kind::Fifo => "|",
            Kind::Broken => "@",
            _ => "",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    /// Shared with the key in `Tree::by_path`. Absolute paths average over a
    /// hundred bytes here and every node needs one; storing them twice was a
    /// third of the tree's memory.
    pub path: Arc<Path>,
    pub parent: Option<NodeId>,
    pub kind: Kind,
    /// Whether this entry was reached through a symlink. Orthogonal to `kind`,
    /// which describes what the link points at.
    pub symlink: bool,
    pub size: u64,
    pub expanded: bool,
    /// Whether `children` reflects the filesystem. Directories start unloaded.
    pub loaded: bool,
    /// Entries left out of `children` because of `ScanOptions::max_entries`.
    pub omitted: usize,
    pub children: Vec<NodeId>,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }

    fn new(path: Arc<Path>, parent: Option<NodeId>, kind: Kind, symlink: bool, size: u64) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self {
            name,
            path,
            parent,
            kind,
            symlink,
            size,
            expanded: false,
            loaded: false,
            omitted: 0,
            children: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub show_hidden: bool,
    pub dirs_only: bool,
    pub follow_links: bool,
    /// Most entries to keep from one directory. Nobody scrolls a hundred
    /// thousand rows, and pretending to offer them costs every view dearly.
    pub max_entries: usize,
}

impl Default for ScanOptions {
    /// Hidden files are shown. treex is a tool for looking at source trees,
    /// where `.github/`, `.env` and `.gitignore` are things you came to see.
    fn default() -> Self {
        Self {
            show_hidden: true,
            dirs_only: false,
            follow_links: false,
            max_entries: 5_000,
        }
    }
}

impl Row {
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }
}

/// The third element of a row packs the kind and two flags into one number:
/// `kind << 2 | expanded | symlink << 1`. Spelling the kind out as `"d"` cost
/// nearly a tenth of the whole message.
pub const FLAG_EXPANDED: u8 = 1;
pub const FLAG_SYMLINK: u8 = 2;

impl Serialize for Row {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;

        let mut packed = self.kind.code() << 2;
        if self.expanded {
            packed |= FLAG_EXPANDED;
        }
        if self.symlink {
            packed |= FLAG_SYMLINK;
        }
        let len = if self.omitted > 0 { 4 } else { 3 };

        let mut seq = serializer.serialize_seq(Some(len))?;
        seq.serialize_element(&self.depth)?;
        seq.serialize_element(&self.name)?;
        seq.serialize_element(&packed)?;
        if self.omitted > 0 {
            seq.serialize_element(&self.omitted)?;
        }
        seq.end()
    }
}

/// One line of the flattened, currently-visible tree. This is the single source
/// of truth for both TUI rendering and browser rendering, and — because the
/// index is the screen row — for mouse hit-testing.
/// Serialized positionally, as `[depth, name, packed]` with an optional fourth
/// element for `omitted`, where `packed` is `kind << 2 | flags`.
///
/// A tree is thousands of near-identical records; field names and absolute
/// paths are almost all of the bytes and none of the information. `path` is not
/// sent at all — depth-first order means the client already knows every
/// ancestor and can rebuild it.
#[derive(Debug, Clone)]
pub struct Row {
    pub id: NodeId,
    pub depth: usize,
    pub name: String,
    pub path: Arc<Path>,
    pub kind: Kind,
    pub symlink: bool,
    pub expanded: bool,
    pub size: u64,
    /// True when this is the last child of its parent, for box-drawing.
    pub last: bool,
    /// How many of this directory's entries were left out, if any.
    pub omitted: usize,
}

pub struct Tree {
    slots: Vec<Option<Node>>,
    free: Vec<NodeId>,
    by_path: HashMap<Arc<Path>, NodeId>,
    root: NodeId,
    pub opts: ScanOptions,
    pub selected: NodeId,
    /// The file being read. Always the selected node, or nothing: reading is a
    /// second step on top of the cursor, and moving the cursor leaves it. Kept
    /// here rather than in the web module so the terminal can color the row.
    pub viewing: Option<Arc<Path>>,
    /// Bumped on every mutation so views can tell whether they are stale.
    pub revision: u64,
    /// Bumped only when `rows()` would come out different. Moving the cursor
    /// does not touch it, which is what lets a view skip resending the tree
    /// for what is really a two-field change.
    pub shape: u64,
}

impl Tree {
    /// Opens `root`, which must be a directory.
    pub fn new(root: impl AsRef<Path>, opts: ScanOptions) -> Result<Self> {
        let given = root.as_ref();
        let root = std::fs::canonicalize(given).map_err(|source| Error::Open {
            path: given.to_path_buf(),
            source,
        })?;

        let meta = std::fs::metadata(&root).map_err(|source| Error::Open {
            path: root.clone(),
            source,
        })?;
        // Without this, pointing treex at a file yields a one-row tree that
        // cannot be expanded and looks like success.
        if !meta.is_dir() {
            return Err(Error::NotADirectory(root));
        }

        let root: Arc<Path> = Arc::from(root);
        let node = Node::new(root.clone(), None, Kind::Dir, false, meta.len());
        let mut tree = Self {
            slots: vec![Some(node)],
            free: Vec::new(),
            by_path: HashMap::from([(root, 0)]),
            root: 0,
            opts,
            selected: 0,
            viewing: None,
            revision: 0,
            shape: 0,
        };
        tree.expand(tree.root);
        Ok(tree)
    }

    /// Records a change that alters what `rows()` produces.
    fn reshaped(&mut self) {
        self.shape += 1;
        self.revision += 1;
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn root_path(&self) -> &Path {
        &self.node(self.root).path
    }

    pub fn node(&self, id: NodeId) -> &Node {
        self.slots[id].as_ref().expect("dangling NodeId")
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.slots.get(id)?.as_ref()
    }

    pub fn id_for_path(&self, path: &Path) -> Option<NodeId> {
        self.by_path.get(path).copied()
    }

    fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.slots[id].as_mut().expect("dangling NodeId")
    }

    fn alloc(&mut self, node: Node) -> NodeId {
        let path = node.path.clone();
        let id = match self.free.pop() {
            Some(id) => {
                self.slots[id] = Some(node);
                id
            }
            None => {
                self.slots.push(Some(node));
                self.slots.len() - 1
            }
        };
        self.by_path.insert(path, id);
        id
    }

    /// Frees a node and everything under it.
    fn free_subtree(&mut self, id: NodeId) {
        let Some(node) = self.slots[id].take() else {
            return;
        };
        self.by_path.remove(&node.path);
        for child in node.children {
            self.free_subtree(child);
        }
        self.free.push(id);
    }

    // -- mutation ---------------------------------------------------------

    pub fn expand(&mut self, id: NodeId) {
        if !self.node(id).is_dir() {
            return;
        }
        // Only expanded directories are watched, so one that has been sitting
        // collapsed may have gone stale on disk. Re-read on the way open.
        let node = self.node(id);
        if node.loaded && node.expanded {
            return;
        }
        self.load_children(id);
        self.node_mut(id).expanded = true;
        self.reshaped();
    }

    pub fn collapse(&mut self, id: NodeId) {
        if self.node(id).is_dir() {
            self.node_mut(id).expanded = false;
            self.reshaped();
        }
    }

    pub fn toggle(&mut self, id: NodeId) {
        if self.node(id).expanded {
            self.collapse(id);
        } else {
            self.expand(id);
        }
    }

    /// Expands every directory down to `depth` levels below `id`.
    pub fn expand_to_depth(&mut self, id: NodeId, depth: usize) {
        if depth == 0 || !self.node(id).is_dir() {
            return;
        }
        self.expand(id);
        for child in self.node(id).children.clone() {
            self.expand_to_depth(child, depth - 1);
        }
    }

    pub fn collapse_all(&mut self) {
        for slot in self.slots.iter_mut().flatten() {
            slot.expanded = false;
        }
        self.node_mut(self.root).expanded = true;
        self.reshaped();
    }

    /// `None` stops showing anything.
    pub fn view(&mut self, path: Option<Arc<Path>>) {
        if self.viewing != path {
            self.viewing = path;
            self.revision += 1;
        }
    }

    /// Drops `viewing` if the file is no longer in the tree.
    fn prune_viewing(&mut self) {
        if let Some(path) = &self.viewing {
            if !self.by_path.contains_key(path) {
                self.viewing = None;
            }
        }
    }

    pub fn select(&mut self, id: NodeId) {
        if self.slots.get(id).and_then(|s| s.as_ref()).is_none() {
            return;
        }
        if self.selected != id {
            // Moving the cursor leaves whatever was being read.
            self.viewing = None;
        }
        self.selected = id;
        self.revision += 1;
    }

    /// Reads `id`'s directory entries and reconciles them against what is
    /// already there, so expansion state survives a refresh.
    ///
    /// Returns whether the listing changed in a way any view would draw. A
    /// touched mtime is not a reason to redraw every tree on screen.
    pub fn load_children(&mut self, id: NodeId) -> bool {
        let path = self.node(id).path.clone();
        let mut entries = crate::scan::read_dir(&path, &self.opts);

        let omitted = entries.len().saturating_sub(self.opts.max_entries);
        entries.truncate(self.opts.max_entries);

        let was_loaded = self.node(id).loaded;
        // Captured before anything is freed: these ids stop being valid below.
        let before: Vec<(Arc<Path>, Kind)> = self
            .node(id)
            .children
            .iter()
            .map(|&c| (self.node(c).path.clone(), self.node(c).kind))
            .collect();

        let existing: HashMap<Arc<Path>, NodeId> = self
            .node(id)
            .children
            .iter()
            .map(|&c| (self.node(c).path.clone(), c))
            .collect();

        let mut children = Vec::with_capacity(entries.len());
        let mut seen: std::collections::HashSet<Arc<Path>> =
            std::collections::HashSet::with_capacity(entries.len());
        for entry in entries {
            match existing.get(entry.path.as_path()) {
                // Kind changes (file replaced by a directory) invalidate the
                // subtree, so treat those as a fresh node rather than a reuse.
                Some(&old) if self.node(old).kind == entry.kind => {
                    let node = self.node_mut(old);
                    node.size = entry.size;
                    node.symlink = entry.symlink;
                    seen.insert(node.path.clone());
                    children.push(old);
                }
                _ => {
                    let node = Node::new(
                        Arc::from(entry.path),
                        Some(id),
                        entry.kind,
                        entry.symlink,
                        entry.size,
                    );
                    children.push(self.alloc(node));
                }
            }
        }

        for (path, old) in existing {
            if !seen.contains(&path) {
                self.free_subtree(old);
            }
        }

        let after: Vec<(Arc<Path>, Kind)> = children
            .iter()
            .map(|&c| (self.node(c).path.clone(), self.node(c).kind))
            .collect();

        let was_omitting = self.node(id).omitted;
        let node = self.node_mut(id);
        node.children = children;
        node.loaded = true;
        node.omitted = omitted;
        !was_loaded || before != after || was_omitting != omitted
    }

    /// Re-reads the directory listing that `path` could have changed.
    ///
    /// Anything outside the tree is ignored outright. Walking up to find *some*
    /// loaded ancestor would mean every write under an unlisted directory —
    /// `target/`, `node_modules/` — re-read the root and redraw every view.
    pub fn refresh_path(&mut self, path: &Path) {
        let dir = if path.is_dir() && self.by_path.contains_key(path) {
            path
        } else {
            // A path we do not know may still be a new entry in a parent we do.
            match path.parent() {
                Some(parent) => parent,
                None => return,
            }
        };

        let Some(&id) = self.by_path.get(dir) else {
            return;
        };
        if self.node(id).is_dir() && self.node(id).loaded && self.load_children(id) {
            self.prune_viewing();
            self.reshaped();
        }
    }

    pub fn refresh_all(&mut self) {
        let loaded: Vec<NodeId> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(id, s)| s.as_ref().filter(|n| n.is_dir() && n.loaded).map(|_| id))
            .collect();
        for id in loaded {
            if self.slots[id].is_some() {
                self.load_children(id);
            }
        }
        if self
            .slots
            .get(self.selected)
            .and_then(|s| s.as_ref())
            .is_none()
        {
            self.selected = self.root;
        }
        self.prune_viewing();
        self.reshaped();
    }

    // -- projection -------------------------------------------------------

    /// Depth-first flattening of everything currently visible.
    pub fn rows(&self) -> Vec<Row> {
        let mut out = Vec::new();
        self.push_rows(self.root, 0, true, &mut out);
        out
    }

    fn push_rows(&self, id: NodeId, depth: usize, last: bool, out: &mut Vec<Row>) {
        let node = self.node(id);
        out.push(Row {
            id,
            depth,
            name: node.name.clone(),
            path: node.path.clone(),
            kind: node.kind,
            symlink: node.symlink,
            expanded: node.expanded,
            size: node.size,
            last,
            omitted: node.omitted,
        });
        if node.expanded {
            let n = node.children.len();
            for (i, &child) in node.children.iter().enumerate() {
                self.push_rows(child, depth + 1, i + 1 == n, out);
            }
        }
    }

    /// Directories whose listing is on screen right now, and therefore the
    /// exact set worth watching for changes. A collapsed directory is not in
    /// here: nothing under it is drawn, and [`expand`](Self::expand) re-reads
    /// it on the way open.
    pub fn live_dirs(&self) -> Vec<Arc<Path>> {
        // Walks the arena rather than `rows()`: the answer is a handful of
        // paths, and flattening would clone a name and a path for every visible
        // row to throw almost all of them away.
        self.slots
            .iter()
            .flatten()
            .filter(|n| n.is_dir() && (n.expanded || n.parent.is_none()))
            .map(|n| n.path.clone())
            .collect()
    }

    /// Index of `selected` within `rows()`, if it is currently visible.
    pub fn selected_row(&self, rows: &[Row]) -> Option<usize> {
        rows.iter().position(|r| r.id == self.selected)
    }

    /// Where `target` would land in `rows()`, without building them.
    ///
    /// The same depth-first walk, counting instead of cloning a name and a path
    /// per row — which matters because it runs on every cursor move.
    pub fn visible_index(&self, target: NodeId) -> Option<usize> {
        fn walk(tree: &Tree, id: NodeId, target: NodeId, n: &mut usize) -> bool {
            if id == target {
                return true;
            }
            *n += 1;
            if tree.node(id).expanded {
                for &child in &tree.node(id).children {
                    if walk(tree, child, target, n) {
                        return true;
                    }
                }
            }
            false
        }

        let mut n = 0;
        walk(self, self.root, target, &mut n).then_some(n)
    }

    /// Reveals `id` by expanding every ancestor, then selects it.
    pub fn reveal(&mut self, id: NodeId) {
        let mut chain = Vec::new();
        let mut cur = self.node(id).parent;
        while let Some(p) = cur {
            chain.push(p);
            cur = self.node(p).parent;
        }
        for p in chain.into_iter().rev() {
            self.expand(p);
        }
        self.select(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(tree: &Tree) -> Vec<std::path::PathBuf> {
        tree.live_dirs().iter().map(|p| p.to_path_buf()).collect()
    }

    fn fixture() -> (tempfile::TempDir, Tree) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/deep.txt"), "x").unwrap();
        std::fs::write(root.join("a/one.txt"), "x").unwrap();
        std::fs::write(root.join("top.txt"), "x").unwrap();
        let tree = Tree::new(root, ScanOptions::default()).unwrap();
        (dir, tree)
    }

    #[test]
    fn a_file_is_not_something_to_browse() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();

        match Tree::new(&file, ScanOptions::default()).err() {
            Some(crate::Error::NotADirectory(p)) => assert!(p.ends_with("a.txt")),
            other => panic!("expected NotADirectory, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_path_says_which_one() {
        let err = Tree::new("/no/such/place", ScanOptions::default())
            .err()
            .expect("a missing path must not open");
        assert!(err.to_string().contains("/no/such/place"), "{err}");
    }

    #[test]
    fn root_starts_expanded_children_do_not() {
        let (_d, tree) = fixture();
        let rows = tree.rows();
        // root + a + top.txt
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].name, "a");
        assert!(!rows[1].expanded);
    }

    #[test]
    fn dirs_sort_before_files() {
        let (_d, tree) = fixture();
        let rows = tree.rows();
        assert!(rows[1].is_dir());
        assert!(!rows[2].is_dir());
    }

    #[test]
    fn expanding_reveals_children_lazily() {
        let (_d, mut tree) = fixture();
        let a = tree.id_for_path(&tree.root_path().join("a")).unwrap();
        assert!(!tree.node(a).loaded);
        tree.expand(a);
        assert!(tree.node(a).loaded);
        let names: Vec<_> = tree.rows().iter().map(|r| r.name.clone()).collect();
        assert!(names.contains(&"b".to_string()));
        assert!(names.contains(&"one.txt".to_string()));
        // b itself is still collapsed, so deep.txt stays hidden
        assert!(!names.contains(&"deep.txt".to_string()));
    }

    #[test]
    fn refresh_preserves_expansion_state() {
        let (dir, mut tree) = fixture();
        let a = tree.id_for_path(&tree.root_path().join("a")).unwrap();
        tree.expand(a);
        let b = tree.id_for_path(&tree.root_path().join("a/b")).unwrap();
        tree.expand(b);

        std::fs::write(dir.path().join("a/two.txt"), "x").unwrap();
        tree.refresh_path(&dir.path().join("a/two.txt"));

        let names: Vec<_> = tree.rows().iter().map(|r| r.name.clone()).collect();
        assert!(names.contains(&"two.txt".to_string()));
        // deep.txt is only visible if b stayed expanded across the reconcile
        assert!(names.contains(&"deep.txt".to_string()));
    }

    #[test]
    fn deleted_entries_disappear_and_free_their_subtree() {
        let (dir, mut tree) = fixture();
        let a = tree.id_for_path(&tree.root_path().join("a")).unwrap();
        tree.expand(a);
        let b = tree.id_for_path(&tree.root_path().join("a/b")).unwrap();
        tree.expand(b);

        std::fs::remove_dir_all(dir.path().join("a/b")).unwrap();
        tree.refresh_path(&dir.path().join("a/b"));

        assert!(tree.id_for_path(&dir.path().join("a/b")).is_none());
        assert!(tree.id_for_path(&dir.path().join("a/b/deep.txt")).is_none());
        assert!(tree.get(b).is_none());
    }

    #[test]
    fn writes_outside_the_tree_do_not_bump_the_revision() {
        let (dir, mut tree) = fixture();
        // `a` exists but was never expanded, so nothing inside it is visible.
        std::fs::create_dir_all(dir.path().join("a/b/deeper")).unwrap();
        let before = tree.revision;

        std::fs::write(dir.path().join("a/b/deeper/noise.txt"), "x").unwrap();
        tree.refresh_path(&dir.path().join("a/b/deeper/noise.txt"));
        tree.refresh_path(&dir.path().join("a/b/deeper"));

        assert_eq!(
            tree.revision, before,
            "a write under an unlisted directory redrew every view"
        );
    }

    #[test]
    fn touching_a_visible_file_without_changing_the_listing_is_not_a_change() {
        let (dir, mut tree) = fixture();
        let before = tree.revision;
        std::fs::write(dir.path().join("top.txt"), "different contents").unwrap();
        tree.refresh_path(&dir.path().join("top.txt"));
        assert_eq!(tree.revision, before);
    }

    #[test]
    fn a_new_entry_in_a_visible_directory_still_registers() {
        let (dir, mut tree) = fixture();
        let before = tree.revision;
        std::fs::create_dir(dir.path().join("fresh")).unwrap();
        tree.refresh_path(&dir.path().join("fresh"));

        assert_ne!(tree.revision, before);
        let names: Vec<_> = tree.rows().iter().map(|r| r.name.clone()).collect();
        assert!(names.contains(&"fresh".to_string()));
    }

    #[test]
    fn a_huge_directory_is_truncated_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..50 {
            std::fs::write(dir.path().join(format!("f{i:03}")), "").unwrap();
        }

        let opts = ScanOptions {
            max_entries: 10,
            ..ScanOptions::default()
        };
        let tree = Tree::new(dir.path(), opts).unwrap();
        let rows = tree.rows();

        assert_eq!(rows.len(), 11, "root plus ten entries");
        assert_eq!(rows[0].omitted, 40);
        assert!(rows[1..].iter().all(|r| r.omitted == 0));
    }

    #[test]
    fn visible_index_agrees_with_the_flattened_rows() {
        let (_d, mut tree) = fixture();
        let a = tree.id_for_path(&tree.root_path().join("a")).unwrap();
        tree.expand(a);
        let b = tree.id_for_path(&tree.root_path().join("a/b")).unwrap();
        tree.expand(b);

        // The cheap walk must land exactly where building the rows would.
        let rows = tree.rows();
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(tree.visible_index(row.id), Some(i), "{}", row.name);
        }

        tree.collapse(a);
        let hidden = tree.id_for_path(&tree.root_path().join("a/b")).unwrap();
        assert_eq!(
            tree.visible_index(hidden),
            None,
            "a collapsed row has no index"
        );
    }

    #[test]
    fn live_dirs_covers_exactly_what_is_drawn() {
        let (_d, mut tree) = fixture();
        assert_eq!(live(&tree), vec![tree.root_path().to_path_buf()]);

        let a = tree.id_for_path(&tree.root_path().join("a")).unwrap();
        tree.expand(a);
        let dirs = live(&tree);
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        assert!(dirs.contains(&tree.root_path().join("a")));
        // `a/b` is listed but collapsed, so its own contents are not drawn.
        assert!(!dirs.contains(&tree.root_path().join("a/b")));

        tree.collapse(a);
        assert_eq!(live(&tree), vec![tree.root_path().to_path_buf()]);
    }

    #[test]
    fn reopening_a_collapsed_directory_picks_up_what_changed_meanwhile() {
        let (dir, mut tree) = fixture();
        let a = tree.id_for_path(&tree.root_path().join("a")).unwrap();
        tree.expand(a);
        tree.collapse(a);

        // Nothing is watching `a` while it is shut.
        std::fs::write(dir.path().join("a/while-closed.txt"), "x").unwrap();
        tree.expand(a);

        let names: Vec<_> = tree.rows().iter().map(|r| r.name.clone()).collect();
        assert!(names.contains(&"while-closed.txt".to_string()), "{names:?}");
    }

    #[test]
    fn moving_the_cursor_leaves_the_file_being_read() {
        let (dir, mut tree) = fixture();
        let top = tree.id_for_path(&dir.path().join("top.txt")).unwrap();
        tree.select(top);
        tree.view(Some(tree.node(top).path.clone()));

        let a = tree.id_for_path(&tree.root_path().join("a")).unwrap();
        tree.select(a);

        assert_eq!(tree.viewing, None);
    }

    #[test]
    fn selecting_the_row_already_under_the_cursor_keeps_reading() {
        let (dir, mut tree) = fixture();
        let path = dir.path().join("top.txt");
        let top = tree.id_for_path(&path).unwrap();
        tree.select(top);
        tree.view(Some(Arc::from(path.clone())));

        tree.select(top);
        assert_eq!(tree.viewing.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn a_deleted_file_stops_being_the_one_on_screen() {
        let (dir, mut tree) = fixture();
        let path = dir.path().join("top.txt");
        tree.view(Some(Arc::from(path.clone())));

        std::fs::remove_file(&path).unwrap();
        tree.refresh_path(&path);

        assert_eq!(tree.viewing, None, "left showing a file that is gone");
    }

    #[test]
    fn reveal_expands_ancestors() {
        let (_d, mut tree) = fixture();
        let a = tree.id_for_path(&tree.root_path().join("a")).unwrap();
        tree.expand(a);
        let b = tree.id_for_path(&tree.root_path().join("a/b")).unwrap();
        tree.expand(b);
        let deep = tree
            .id_for_path(&tree.root_path().join("a/b/deep.txt"))
            .unwrap();
        tree.collapse_all();
        tree.reveal(deep);
        assert_eq!(tree.selected_row(&tree.rows()).map(|_| true), Some(true));
    }
}
