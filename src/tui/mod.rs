//! The terminal view.

pub mod hit;
pub mod render;

use std::io::{self, Stdout};
use std::sync::Arc;

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::state::{Command, Session};
use hit::Hit;
use render::{Theme, View};

#[derive(Debug, Clone)]
pub struct TuiOptions {
    pub mouse: bool,
    /// Single-clicking a directory name toggles it, the way VS Code's explorer
    /// behaves. With this off, only the twistie toggles.
    pub click_toggles_dirs: bool,
    /// Shown on the right of the status bar; the web view puts its URL here.
    pub status_note: Option<String>,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            mouse: true,
            click_toggles_dirs: true,
            status_note: None,
        }
    }
}

struct Terminals {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    mouse: bool,
}

impl Terminals {
    fn enter(mouse: bool) -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        if mouse {
            execute!(stdout, EnableMouseCapture)?;
        }
        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(stdout))?,
            mouse,
        })
    }
}

impl Drop for Terminals {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        if self.mouse {
            let _ = execute!(stdout, DisableMouseCapture);
        }
        let _ = execute!(stdout, LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

pub async fn run(session: Arc<Session>, opts: TuiOptions) -> anyhow::Result<()> {
    // Without this, a panic leaves the user staring at a raw-mode terminal.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        default_hook(info);
    }));

    let mut term = Terminals::enter(opts.mouse)?;
    let result = event_loop(&mut term.terminal, session, &opts).await;
    drop(term);
    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    session: Arc<Session>,
    opts: &TuiOptions,
) -> anyhow::Result<()> {
    let theme = Theme::default();
    let mut events = EventStream::new();
    let mut changed = session.subscribe();
    let mut offset: usize = 0;
    let mut inner = Rect::default();

    loop {
        let snapshot = session.snapshot();
        let size = terminal.size()?;
        let viewport = render::body_inner(Rect::new(0, 0, size.width, size.height)).height as usize;
        offset = clamp_offset(offset, snapshot.selected, viewport, snapshot.rows.len());

        let status = status_line(&snapshot, opts);
        terminal.draw(|frame| {
            inner = render::body_inner(frame.area());
            render::draw(
                frame,
                &View {
                    snapshot: &snapshot,
                    offset,
                    theme: &theme,
                    status: &status,
                },
            );
        })?;

        tokio::select! {
            event = events.next() => {
                let action = match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        handle_key(key, &snapshot)
                    }
                    Some(Ok(Event::Mouse(mouse))) => {
                        handle_mouse(mouse, &snapshot, inner, offset, opts)
                    }
                    Some(Ok(_)) => Action::Nothing,
                    Some(Err(err)) => return Err(err.into()),
                    None => return Ok(()),
                };

                match action {
                    Action::Quit => return Ok(()),
                    Action::Nothing => {}
                    Action::Scroll(by) => {
                        let max = snapshot.rows.len().saturating_sub(1) as i64;
                        offset = (offset as i64 + by as i64).clamp(0, max) as usize;
                    }
                    // Reading a directory is blocking and unbounded — a hung
                    // network mount can hold it for a minute — so it must not
                    // run on a runtime worker.
                    Action::Run(cmd) if cmd.may_block() => {
                        let session = session.clone();
                        let _ = tokio::task::spawn_blocking(move || session.apply(cmd)).await;
                    }
                    Action::Run(cmd) => session.apply(cmd),
                }
            }
            // A revision bump from the browser or the file watcher; redraw.
            res = changed.recv() => {
                if let Err(tokio::sync::broadcast::error::RecvError::Closed) = res {
                    return Ok(());
                }
            }
        }
    }
}

fn status_line(snapshot: &crate::state::Snapshot, opts: &TuiOptions) -> String {
    // The URL and whatever the browser has open go first: a narrow terminal
    // truncates the tail, and the key hints are the least surprising loss.
    let mut s = String::from(" ");
    if let Some(note) = &opts.status_note {
        s.push_str(note);
        s.push_str(" · ");
    }
    s.push_str(&format!(
        "{} rows · ↑↓ move · → open/expand · ← back · . hidden · q quit",
        snapshot.rows.len()
    ));
    s
}

fn clamp_offset(offset: usize, selected: Option<usize>, viewport: usize, total: usize) -> usize {
    let viewport = viewport.max(1);
    let max_offset = total.saturating_sub(viewport);
    let mut offset = offset.min(max_offset);
    if let Some(sel) = selected {
        if sel < offset {
            offset = sel;
        } else if sel >= offset + viewport {
            offset = sel + 1 - viewport;
        }
    }
    offset
}

/// What a key or a click means. Returning the intent rather than applying it
/// keeps these functions pure — testable without a `Session`, and leaving the
/// event loop to decide what has to run off the runtime.
#[derive(Debug, PartialEq)]
enum Action {
    Nothing,
    Quit,
    Run(Command),
    Scroll(i32),
}

fn handle_key(key: KeyEvent, snapshot: &crate::state::Snapshot) -> Action {
    let current = snapshot.selected.and_then(|i| snapshot.rows.get(i));
    let run = |cmd| Action::Run(cmd);

    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL)
        | (KeyCode::Char('q'), _)
        | (KeyCode::Esc, _) => Action::Quit,

        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => run(Command::MoveSelection { delta: 1 }),
        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => run(Command::MoveSelection { delta: -1 }),
        (KeyCode::PageDown, _) => run(Command::MoveSelection { delta: 20 }),
        (KeyCode::PageUp, _) => run(Command::MoveSelection { delta: -20 }),
        (KeyCode::Home, _) | (KeyCode::Char('g'), _) => run(Command::SelectRow { row: 0 }),
        (KeyCode::End, _) | (KeyCode::Char('G'), _) => run(Command::SelectRow {
            row: snapshot.rows.len().saturating_sub(1),
        }),

        // W3C tree semantics: right descends, left ascends.
        (KeyCode::Right, _) | (KeyCode::Char('l'), _) => match current {
            Some(row) if row.is_dir() && !row.expanded => run(Command::Expand {
                path: row.path.to_path_buf(),
            }),
            Some(row) if row.is_dir() => run(Command::MoveSelection { delta: 1 }),
            Some(row) => run(Command::View {
                path: Some(row.path.to_path_buf()),
            }),
            None => Action::Nothing,
        },
        (KeyCode::Left, _) | (KeyCode::Char('h'), _) => match current {
            _ if snapshot.viewing.is_some() => run(Command::View { path: None }),
            Some(row) if row.is_dir() && row.expanded => run(Command::Collapse {
                path: row.path.to_path_buf(),
            }),
            Some(row) => match row.path.parent() {
                Some(parent) => run(Command::Select {
                    path: parent.to_path_buf(),
                }),
                None => Action::Nothing,
            },
            None => Action::Nothing,
        },
        (KeyCode::Enter, _) | (KeyCode::Char(' '), _) => match current {
            Some(row) if row.is_dir() => run(Command::Toggle {
                path: row.path.to_path_buf(),
            }),
            Some(row) => run(Command::View {
                path: Some(row.path.to_path_buf()),
            }),
            None => Action::Nothing,
        },

        (KeyCode::Char('z'), _) => run(Command::CollapseAll),
        (KeyCode::Char('E'), _) => run(Command::ExpandDepth { depth: 99 }),
        (KeyCode::Char('2'), _) => run(Command::ExpandDepth { depth: 2 }),
        (KeyCode::Char('3'), _) => run(Command::ExpandDepth { depth: 3 }),
        (KeyCode::Char('r'), _) => run(Command::Refresh),
        (KeyCode::Char('.'), _) => run(Command::SetHidden {
            show: !snapshot.show_hidden,
        }),
        _ => Action::Nothing,
    }
}

fn handle_mouse(
    mouse: MouseEvent,
    snapshot: &crate::state::Snapshot,
    inner: Rect,
    offset: usize,
    opts: &TuiOptions,
) -> Action {
    let at = || hit::hit(inner, offset, &snapshot.rows, mouse.column, mouse.row);

    match mouse.kind {
        MouseEventKind::ScrollDown => Action::Scroll(3),
        MouseEventKind::ScrollUp => Action::Scroll(-3),

        // Right-click reads a file; left-click only moves the cursor, which is
        // the distinction the keyboard makes too.
        MouseEventKind::Down(MouseButton::Right) => match at() {
            Hit::Twistie(i) | Hit::Name(i) => match snapshot.rows.get(i) {
                Some(row) if !row.is_dir() => Action::Run(Command::View {
                    path: Some(row.path.to_path_buf()),
                }),
                _ => Action::Nothing,
            },
            Hit::Nothing => Action::Nothing,
        },
        MouseEventKind::Down(MouseButton::Left) => match at() {
            Hit::Twistie(i) => match snapshot.rows.get(i) {
                Some(row) => Action::Run(Command::Toggle {
                    path: row.path.to_path_buf(),
                }),
                None => Action::Nothing,
            },
            Hit::Name(i) => match snapshot.rows.get(i) {
                Some(row) if row.is_dir() && opts.click_toggles_dirs => {
                    Action::Run(Command::Toggle {
                        path: row.path.to_path_buf(),
                    })
                }
                Some(row) => Action::Run(Command::Select {
                    path: row.path.to_path_buf(),
                }),
                None => Action::Nothing,
            },
            Hit::Nothing => Action::Nothing,
        },
        _ => Action::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::state::Snapshot;
    use crate::tree::{Kind, Row};

    fn row(name: &str, is_dir: bool, expanded: bool) -> Row {
        Row {
            id: 0,
            depth: 1,
            name: name.into(),
            path: std::sync::Arc::from(PathBuf::from("/root").join(name)),
            kind: if is_dir { Kind::Dir } else { Kind::File },
            symlink: false,
            expanded,
            size: 0,
            last: false,
            omitted: 0,
        }
    }

    fn snapshot(rows: Vec<Row>, selected: usize, viewing: Option<usize>) -> Snapshot {
        Snapshot {
            revision: 1,
            shape: 1,
            root: PathBuf::from("/root"),
            rows,
            selected: Some(selected),
            viewing,
            show_hidden: true,
        }
    }

    fn press(code: KeyCode, snap: &Snapshot) -> Action {
        handle_key(KeyEvent::new(code, KeyModifiers::NONE), snap)
    }

    #[test]
    fn right_expands_a_directory_but_opens_a_file() {
        let rows = vec![row("dir", true, false), row("a.txt", false, false)];

        let on_dir = snapshot(rows.clone(), 0, None);
        assert_eq!(
            press(KeyCode::Right, &on_dir),
            Action::Run(Command::Expand {
                path: "/root/dir".into()
            })
        );

        let on_file = snapshot(rows, 1, None);
        assert_eq!(
            press(KeyCode::Right, &on_file),
            Action::Run(Command::View {
                path: Some("/root/a.txt".into())
            })
        );
    }

    #[test]
    fn left_leaves_the_reader_before_it_does_anything_else() {
        let rows = vec![row("dir", true, true)];

        // Reading: the first thing left does is close it, not collapse.
        let reading = snapshot(rows.clone(), 0, Some(0));
        assert_eq!(
            press(KeyCode::Left, &reading),
            Action::Run(Command::View { path: None })
        );

        let browsing = snapshot(rows, 0, None);
        assert_eq!(
            press(KeyCode::Left, &browsing),
            Action::Run(Command::Collapse {
                path: "/root/dir".into()
            })
        );
    }

    #[test]
    fn arrow_keys_never_touch_the_disk() {
        let snap = snapshot(vec![row("dir", true, false)], 0, None);
        for code in [KeyCode::Up, KeyCode::Down, KeyCode::Home, KeyCode::End] {
            match press(code, &snap) {
                Action::Run(cmd) => assert!(
                    !cmd.may_block(),
                    "{code:?} would be sent to a blocking thread"
                ),
                other => panic!("{code:?} did nothing: {other:?}"),
            }
        }
    }

    #[test]
    fn expanding_and_refreshing_are_the_blocking_ones() {
        let snap = snapshot(vec![row("dir", true, false)], 0, None);
        for code in [KeyCode::Char('r'), KeyCode::Char('E'), KeyCode::Char('.')] {
            match press(code, &snap) {
                Action::Run(cmd) => assert!(cmd.may_block(), "{code:?} reads the filesystem"),
                other => panic!("{code:?} did nothing: {other:?}"),
            }
        }
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        let snap = snapshot(vec![row("a", false, false)], 0, None);
        assert_eq!(press(KeyCode::Char('q'), &snap), Action::Quit);
        assert_eq!(
            handle_key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &snap
            ),
            Action::Quit
        );
    }

    #[test]
    fn offset_follows_the_selection_into_view() {
        assert_eq!(clamp_offset(0, Some(30), 10, 100), 21);
        assert_eq!(clamp_offset(50, Some(3), 10, 100), 3);
        assert_eq!(clamp_offset(0, Some(5), 10, 100), 0);
    }

    #[test]
    fn offset_never_scrolls_past_the_end() {
        assert_eq!(clamp_offset(99, None, 10, 20), 10);
        assert_eq!(clamp_offset(5, None, 10, 3), 0);
    }
}
