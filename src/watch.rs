//! Filesystem watching. Turns debounced notify events into refresh commands so
//! that every view — terminal and browser alike — sees files appear and vanish
//! without anyone pressing a key.
//!
//! Only the directories currently drawn on screen are watched, and only one
//! level deep each. Watching the root recursively would cost thousands of
//! inotify descriptors on a checkout with a `target/` in it, to deliver events
//! that every view then discards.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::new_debouncer;

use crate::state::{Command, Session};

type Debouncer = notify_debouncer_full::Debouncer<
    notify::RecommendedWatcher,
    notify_debouncer_full::RecommendedCache,
>;

/// Watching stops when this is dropped.
pub struct Watcher {
    _inner: Arc<Mutex<Debouncer>>,
}

pub fn watch(session: Arc<Session>, debounce: Duration) -> notify::Result<Watcher> {
    let (tx, rx) = std::sync::mpsc::channel();
    let debouncer = Arc::new(Mutex::new(new_debouncer(debounce, None, tx)?));

    spawn_event_pump(session.clone(), rx);
    spawn_watch_sync(session, debouncer.clone());

    Ok(Watcher { _inner: debouncer })
}

fn spawn_event_pump(
    session: Arc<Session>,
    rx: std::sync::mpsc::Receiver<notify_debouncer_full::DebounceEventResult>,
) {
    std::thread::spawn(move || {
        for batch in rx {
            let Ok(events) = batch else { continue };

            // One refresh per affected directory, not per event: a `cargo build`
            // can produce thousands of events touching a handful of directories.
            let mut dirs: Vec<&Path> = Vec::new();
            for event in &events {
                for path in &event.paths {
                    let dir = if path.is_dir() {
                        path.as_path()
                    } else {
                        path.parent().unwrap_or(path)
                    };
                    if !dirs.contains(&dir) {
                        dirs.push(dir);
                    }
                }
            }
            for dir in dirs {
                session.apply(Command::RefreshPath {
                    path: dir.to_path_buf(),
                });
            }
        }
    });
}

/// Brings the watched set in line with the drawn set, returning the new set.
fn sync_watches(
    session: &Session,
    debouncer: &Mutex<Debouncer>,
    watched: HashSet<Arc<Path>>,
) -> HashSet<Arc<Path>> {
    let wanted: HashSet<Arc<Path>> = session
        .with_tree(|tree| tree.live_dirs())
        .into_iter()
        .collect();

    let mut debouncer = debouncer.lock().unwrap();
    for dir in wanted.difference(&watched) {
        // A directory can vanish between the snapshot and here; its parent's
        // refresh will drop it from the tree anyway.
        let _ = debouncer.watch(dir, RecursiveMode::NonRecursive);
    }
    for dir in watched.difference(&wanted) {
        let _ = debouncer.unwatch(dir);
    }
    wanted
}

/// Keeps the watched set equal to the drawn set, re-syncing whenever the tree
/// changes shape.
///
/// The first sync runs on the caller's thread, before this returns: a watch
/// registered a moment later would silently miss everything that happened in
/// between, and at startup that is exactly when things happen.
fn spawn_watch_sync(session: Arc<Session>, debouncer: Arc<Mutex<Debouncer>>) {
    let mut changed = session.subscribe();
    let mut watched = sync_watches(&session, &debouncer, HashSet::new());
    let mut synced_shape = session.with_tree(|tree| tree.shape);

    std::thread::spawn(move || loop {
        match changed.blocking_recv() {
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }

        // The watched set can only differ when the tree reshaped. Cursor moves
        // bump the revision too, and on a large tree there are a lot of them.
        let shape = session.with_tree(|tree| tree.shape);
        if shape == synced_shape {
            continue;
        }
        synced_shape = shape;
        watched = sync_watches(&session, &debouncer, watched);
    });
}
