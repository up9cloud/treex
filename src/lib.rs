//! An interactive directory tree that keeps its shape.
//!
//! `treex` is a library first; the `treex` binary is a thin wrapper over it.
//! The model, the expansion state and the syncing all live here, and the two
//! bundled views — a ratatui terminal UI and an axum web server — are optional
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
//! ## Two cursors, not one
//!
//! [`Snapshot::selected`] is where the cursor is. [`Snapshot::viewing`] is a
//! file open for reading, which is always the selected node or nothing —
//! reading is a second step on top of the cursor, and moving the cursor leaves
//! it. [`Command::View`] selects as well as opens, since a pointing device has
//! nothing corresponding to "merely highlighted".
//!
//! # Features
//!
//! | Feature | Default | |
//! |---|---|---|
//! | `tui` | yes | [`tui`], the ratatui view and its mouse handling |
//! | `watch` | yes | [`watch`], filesystem events via `notify` |
//! | `web` | yes | [`web`], the axum server and the browser page |
//!
//! All three are on by default, because the binary wants all three. With
//! `default-features = false` you get [`Tree`], [`scan`], [`preview`] and
//! [`Session`] and nothing else. No runtime is started for you at any point;
//! [`Session`] uses a `tokio` broadcast channel but never spawns.

pub mod error;
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
