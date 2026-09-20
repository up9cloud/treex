//! An interactive directory tree that keeps its shape.
//!
//! `treex` is a library first; the `treex` binary is a thin wrapper over it.
//! The model, the expansion state and the syncing all live here, and the two
//! bundled views — a ratatui terminal UI and a web server — are optional
//! features layered on top.
//!
//! # The model
//!
//! [`Tree`] is an arena of nodes with expansion state on top. It is plain and
//! synchronous: no async, no terminal, no server. Directories load lazily, and
//! a refresh reconciles against what is already there so expansion survives it.
//!
//! ```
//! use treex::{ScanOptions, Tree};
//!
//! let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
//! let mut tree = Tree::new(root, ScanOptions::default())?;
//!
//! let id = tree.id_for_path(&tree.root_path().join("tui")).unwrap();
//! tree.expand(id);
//!
//! for row in tree.rows() {
//!     println!("{}{}", "  ".repeat(row.depth), row.name);
//! }
//! # Ok::<(), treex::Error>(())
//! ```
//!
//! # Several views at once
//!
//! [`Session`] wraps a [`Tree`] so more than one view can drive it. Every view
//! — the terminal, each browser tab, the filesystem watcher — sends a
//! [`Command`] and observes a [`Snapshot`]. That is the whole of how they stay
//! in step; there is no second copy of the state anywhere.
//!
//! ```
//! use treex::{Command, ScanOptions, Session};
//!
//! let session = Session::new(env!("CARGO_MANIFEST_DIR"), ScanOptions::default())?;
//! let mut changed = session.subscribe();
//!
//! session.apply(Command::ExpandDepth { depth: 2 });
//!
//! // Every mutation bumps the revision and wakes every subscriber.
//! assert!(changed.try_recv().is_ok());
//! assert!(session.snapshot().rows.len() > 1);
//! # Ok::<(), treex::Error>(())
//! ```
//!
//! [`Row`] is the shared projection: one flattened, currently-visible line
//! carrying its own depth. Both bundled renderers draw from it, and because a
//! row's index *is* its screen line, it is also what mouse clicks resolve
//! against.
//!
//! ## The cursor and what it shows
//!
//! [`Snapshot::selected`] is where the cursor is and [`Snapshot::viewing`] is
//! the file on display, but they are not two steps: [`Tree::select`] shows
//! whatever the cursor can show, so one click or one arrow key is all it takes.
//! A directory has nothing of its own to display, so the file already up stays
//! up — moving through a tree is not a reason to blank the pane being read.
//! [`Tree::select_viewed`] is the way back: working that pane says the file is
//! the subject again, so the cursor rejoins it.
//!
//! [`Snapshot::view_line`] is the first visible line of that file, shared the
//! same way, with each view clamping it to its own height.
//!
//! Both bundled views show the file's contents, and neither is handed them:
//! the session says *which* file, and each view reads it through [`preview`]
//! for itself.
//!
//! [`git`] answers the other question a tree wants asked of a repository —
//! what it would ignore — by running `git check-ignore` rather than reading
//! rule files, so nested, negated and global rules all behave as git's do.
//! [`Row::ignored`] is that answer; no view hides those rows, they are drawn
//! dimmed.
//!
//! # Colors
//!
//! [`highlight`] colors a file by running the user's own `bat` and reading the
//! ANSI back, so there is no highlighting engine in the dependency tree and no
//! theme of treex's own. It returns styled byte ranges over text the caller
//! already holds, and `None` where there is no `bat` — a view that draws the
//! plain text on `None` needs no other fallback.
//!
//! # Features
//!
//! | Feature | Default | |
//! |---|---|---|
//! | `tui` | yes | [`tui`], the ratatui view and its mouse handling |
//! | `watch` | yes | [`watch`], filesystem events via `notify` |
//! | `web` | yes | [`web`], the HTTP server and the browser page |
//!
//! All three are on by default, because the binary wants all three. With
//! `default-features = false` you get [`Tree`], [`scan`], [`preview`],
//! [`highlight`], [`git`] and [`Session`] and nothing else. No runtime is started for you at any point;
//! [`Session`] uses a `tokio` broadcast channel but never spawns.

pub mod error;
pub mod git;
pub mod highlight;
pub mod preview;
pub mod scan;
pub mod state;
pub mod tree;

#[cfg(feature = "tui")]
pub mod tui;

#[cfg(feature = "watch")]
pub mod watch;

#[cfg(feature = "web")]
pub mod web;

pub use error::{Error, Result};
pub use preview::{Preview, PreviewOptions};
pub use state::{Command, Session, Snapshot};
pub use tree::{Node, NodeId, Row, ScanOptions, Tree};
