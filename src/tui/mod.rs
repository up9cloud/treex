//! The terminal view.

pub mod hit;
pub mod render;
pub mod viewer;

use std::io::{self, Stdout};
use std::path::Path;
use std::sync::Arc;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect};
use ratatui::Terminal;

use crate::highlight::Run;
use crate::preview::{Preview, PreviewOptions};
use crate::state::{Command, Session};
use hit::Hit;
use render::{Theme, View};
use viewer::Viewer;

#[derive(Debug, Clone)]
pub struct TuiOptions {
    pub mouse: bool,
    /// Single-clicking a directory name toggles it, the way VS Code's explorer
    /// behaves. With this off, only the twistie toggles.
    pub click_toggles_dirs: bool,
    /// Shown on the right of the status bar; the web view puts its URL here.
    pub status_note: Option<String>,
    /// `None` leaves the terminal with no file pane at all, which is what
    /// `--no-preview` means here.
    pub preview: Option<PreviewOptions>,
    /// Color file contents by asking `bat`, when there is one.
    pub highlight: bool,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            mouse: true,
            click_toggles_dirs: true,
            status_note: None,
            preview: Some(PreviewOptions::default()),
            highlight: true,
        }
    }
}

struct Terminals {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Terminals {
    fn enter(mouse: bool) -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        // Without this a paste arrives as ordinary keystrokes, so pasted text
        // runs whatever it happens to spell — `2` and `E` expand, `q` quits.
        execute!(stdout, EnableBracketedPaste)?;
        if mouse {
            execute!(stdout, EnableMouseCapture)?;
        }
        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(stdout))?,
        })
    }
}

impl Drop for Terminals {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        // Unconditionally: `m` can turn capture on in a run that started with
        // `--no-mouse`, and a terminal left capturing has no selection of its
        // own until something resets it. Sending this when it was never on
        // costs nothing.
        let _ = execute!(stdout, DisableMouseCapture);
        let _ = execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

pub async fn run(session: Arc<Session>, opts: TuiOptions) -> anyhow::Result<()> {
    // Without this, a panic leaves the user staring at a raw-mode terminal.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
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
    // What the offset was last pulled to. The cursor is brought into view when
    // it *moves*, not on every frame — otherwise the wheel scrolls the tree
    // and the next draw yanks it straight back to the cursor.
    let mut revealed: Option<usize> = None;
    let mut viewer: Option<Viewer> = None;
    let mut focus = Focus::Tree;
    // Where the divider was dragged to, and — while a drag is under way — how
    // far the grab was from the edge, so the panes do not jump on the press.
    let mut split: Option<u16> = None;
    let mut grab: Option<u16> = None;
    // Everything a reader turns off to get the terminal's own selection onto
    // clean text: the tree, the numbers beside it, and treex's grip on the
    // mouse.
    let mut folded = false;
    let mut numbers = true;
    // On by default: a stray `\r` or a tab where spaces were meant is exactly
    // the kind of thing a file viewer should not hide.
    let mut marks = true;
    let mut mouse = opts.mouse;
    // What `c` put aside, and therefore whether `c` is currently engaged.
    let mut restore: Option<(bool, bool, bool, bool)> = None;
    // Reading a file is blocking, so the pane is filled by a message rather
    // than by the draw: the loop keeps redrawing while the read is in flight.
    let (reads, mut read) = tokio::sync::mpsc::channel::<Read>(4);

    loop {
        let snapshot = session.snapshot();

        // `viewing` lives in the session, so the browser opening a file opens
        // it here too. Only a change of path starts a read — or moves the
        // focus, so that a deliberate Tab back to the tree survives a redraw.
        let open = snapshot
            .viewing
            .and_then(|i| snapshot.rows.get(i))
            .map(|row| row.path.clone());
        match (&open, &viewer) {
            (Some(path), Some(showing)) if showing.path == *path => {}
            (Some(path), _) if opts.preview.is_some() => {
                viewer = Some(Viewer::loading(path.clone()));
                spawn_read(path.clone(), opts, reads.clone());
            }
            (None, Some(_)) => {
                viewer = None;
                focus = Focus::Tree;
            }
            _ => {}
        }

        let size = terminal.size()?;
        let panes = render::panes(
            Rect::new(0, 0, size.width, size.height),
            viewer.is_some(),
            split,
            folded,
        );
        let rows = panes.tree.map(render::inner).unwrap_or_default();
        let file = panes.viewer.map(render::inner).unwrap_or_default();
        let divider = panes.divider();
        let fold = panes
            .viewer
            .map(|area| viewer::fold_zone(area, panes.tree.is_some()))
            .unwrap_or_default();
        let tree_width = panes.tree.map(|pane| pane.width).unwrap_or_default();
        let page = file.height as usize;
        let moved = snapshot.selected != revealed;
        revealed = snapshot.selected;
        offset = clamp_offset(
            offset,
            snapshot.selected.filter(|_| moved),
            rows.height as usize,
            snapshot.rows.len(),
        );

        if let Some(showing) = &mut viewer {
            showing.scroll_to(snapshot.view_line, page);
        }

        let modes = Modes {
            folded,
            numbers,
            marks,
            dotfiles: snapshot.show_hidden,
            mouse,
            copying: restore.is_some(),
        };
        let (status, mode_zones) =
            status_line(opts, &theme, viewer.as_ref(), focus, modes, panes.footer);
        terminal.draw(|frame| {
            render::draw(
                frame,
                &View {
                    snapshot: &snapshot,
                    offset,
                    theme: &theme,
                    status: status.clone(),
                    viewer: viewer.as_ref(),
                    split,
                    folded,
                    numbers,
                    marks,
                    reading: focus == Focus::Viewer,
                },
            );
        })?;

        tokio::select! {
            event = events.next() => {
                let action = match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        handle_key(key, &snapshot, focus, viewer.is_some())
                    }
                    Some(Ok(Event::Mouse(mouse))) => {
                        // Which pane the pointer is over decides where the
                        // keyboard goes next, the way clicking a pane does
                        // everywhere else.
                        if let MouseEventKind::Down(_) = mouse.kind {
                            if let Some(pane) = pane_at(rows, file, mouse.column, mouse.row) {
                                focus = pane;
                            }
                        }
                        handle_mouse(
                            mouse,
                            &snapshot,
                            Panes {
                                rows,
                                file,
                                modes: mode_zones,
                                divider,
                                fold,
                            },
                            offset,
                            opts,
                            grab.is_some(),
                        )
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
                    Action::ScrollFile(motion) => {
                        if let Some(showing) = &viewer {
                            let line =
                                motion.to_share(showing.offset, showing.line_count(), page);
                            if let Some(line) = line {
                                session.apply(Command::ScrollView { line });
                            }
                        }
                    }
                    Action::Focus(pane) => {
                        focus = pane;
                        if pane == Focus::Viewer {
                            use_file(&session, viewer.as_ref());
                        }
                    }
                    Action::GrabDivider(column) => grab = Some(tree_width.saturating_sub(column)),
                    Action::Split(column) => {
                        // Clamped as it is stored rather than only as it is
                        // drawn: a width kept past the limit would have to be
                        // dragged back through the slack before anything moved.
                        let wanted = column.saturating_add(grab.unwrap_or(0));
                        split = Some(wanted.clamp(
                            render::TREE_MIN_WIDTH,
                            size.width.saturating_sub(render::VIEWER_MIN_WIDTH),
                        ));
                    }
                    Action::Release => grab = None,
                    Action::Fold => {
                        folded = !folded;
                        // Nothing to move the keyboard to while it is away.
                        if folded {
                            focus = Focus::Viewer;
                            use_file(&session, viewer.as_ref());
                        }
                    }
                    Action::LineNumbers => numbers = !numbers,
                    Action::Marks => marks = !marks,
                    Action::Mouse => {
                        mouse = !mouse;
                        set_mouse(mouse)?;
                    }
                    // One key for the whole arrangement, since wanting to copy
                    // something is one thought rather than three.
                    Action::Copy => {
                        match restore.take() {
                            Some((was_folded, had_numbers, had_marks, had_mouse)) => {
                                folded = was_folded;
                                numbers = had_numbers;
                                marks = had_marks;
                                mouse = had_mouse;
                            }
                            None => {
                                restore = Some((folded, numbers, marks, mouse));
                                folded = true;
                                numbers = false;
                                // A `·` for every space would be copied as a
                                // `·`, which is the whole thing this is
                                // getting out of the way of.
                                marks = false;
                                mouse = false;
                                focus = Focus::Viewer;
                                use_file(&session, viewer.as_ref());
                            }
                        }
                        set_mouse(mouse)?;
                    }
                    // Reading a directory is blocking and unbounded — a hung
                    // network mount can hold it for a minute — so it must not
                    // run on a runtime worker.
                    Action::Run(cmd) if cmd.may_block() => {
                        let session = session.clone();
                        let _ = tokio::task::spawn_blocking(move || session.apply(cmd)).await;
                    }
                    Action::Run(cmd) => {
                        // Enter and `→` are a deliberate "open this", so the
                        // keyboard goes with it. The cursor merely landing on
                        // a file shows it without taking the keyboard away.
                        let deliberate = matches!(cmd, Command::View { path: Some(_) });
                        session.apply(cmd);
                        if deliberate {
                            focus = Focus::Viewer;
                        }
                    }
                }
            }
            // A revision bump from the browser or the file watcher; redraw.
            res = changed.recv() => {
                if let Err(tokio::sync::broadcast::error::RecvError::Closed) = res {
                    return Ok(());
                }
            }
            // A file came back. A slow read of something the cursor has since
            // left is dropped on the floor rather than drawn.
            Some(done) = read.recv() => {
                if let Some(showing) = &mut viewer {
                    if showing.path == done.path {
                        showing.fill(done.preview, done.runs);
                    }
                }
            }
        }
    }
}

/// One file, read off the runtime and colored on the way.
struct Read {
    path: Arc<Path>,
    preview: Preview,
    runs: Vec<Run>,
}

fn spawn_read(path: Arc<Path>, opts: &TuiOptions, done: tokio::sync::mpsc::Sender<Read>) {
    let Some(limits) = opts.preview else { return };
    let highlight = opts.highlight;

    tokio::task::spawn_blocking(move || {
        let preview = crate::preview::read(&path, &limits, None);
        // `bat` is another process and a megabyte of Rust keeps it busy for
        // the best part of a second, which is the other reason this is not on
        // a runtime worker.
        let runs = match (&preview, highlight) {
            (Preview::Ok { content, .. }, true) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                crate::highlight::paint(&name, content).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let _ = done.blocking_send(Read {
            path,
            preview,
            runs,
        });
    });
}

/// Whether treex is holding the mouse or the terminal is. The terminal cannot
/// select while treex has it.
fn set_mouse(on: bool) -> io::Result<()> {
    let mut out = io::stdout();
    if on {
        execute!(out, EnableMouseCapture)
    } else {
        execute!(out, DisableMouseCapture)
    }
}

/// Says the file is what is being worked on, which puts the cursor back on it
/// when it had been left on a directory.
fn use_file(session: &Session, viewer: Option<&Viewer>) {
    if let Some(showing) = viewer {
        session.apply(Command::View {
            path: Some(showing.path.to_path_buf()),
        });
    }
}

/// Which pane has the keyboard. The tree's cursor and the file's scroll are
/// two different positions, and `↑↓` has to mean one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Viewer,
}

/// The inner rects of the two panes, i.e. where their contents are drawn, and
/// the columns the divider between them occupies.
#[derive(Debug, Clone, Copy, Default)]
struct Panes {
    rows: Rect,
    file: Rect,
    modes: ModeZones,
    divider: Option<(u16, u16)>,
    /// The file header's fold control, which is a click target like any
    /// button — the `b` key is the same thing without a mouse.
    fold: Rect,
}

impl Panes {
    fn on_divider(&self, column: u16) -> bool {
        matches!(self.divider, Some((left, right)) if column == left || column == right)
    }
}

/// The pane a point is in, if any. An empty rect contains nothing, which is
/// what a missing pane is.
fn pane_at(rows: Rect, file: Rect, column: u16, row: u16) -> Option<Focus> {
    let at = Position::new(column, row);
    if file.contains(at) {
        Some(Focus::Viewer)
    } else if rows.contains(at) {
        Some(Focus::Tree)
    } else {
        None
    }
}

/// The footer. Every mode indicator says what the state *is*, not what the key
/// would do: the glyph or the word is the answer, and dim means off.
fn status_line(
    opts: &TuiOptions,
    theme: &Theme,
    viewer: Option<&Viewer>,
    focus: Focus,
    modes: Modes,
    footer: Rect,
) -> (ratatui::text::Line<'static>, ModeZones) {
    use ratatui::text::Span;

    let mut zones = ModeZones::default();
    let mut spans = vec![Span::styled(" ", theme.status)];
    // The URL and whatever is open go first: a narrow terminal truncates the
    // tail, and the indicators are a smaller loss than knowing what is open.
    if let Some(note) = &opts.status_note {
        spans.push(Span::styled(format!("{note} · "), theme.status));
    }

    // Nothing has been opened yet. The indicators about a file are left out;
    // the rest behave exactly as they do with one, rather than this being a
    // second footer that drifts from the first.
    let has_file = viewer.is_some();

    // The key, what the state currently is, and whether that state is one
    // doing something — lit says "this is in effect right now", so the whole
    // footer can be read at a glance rather than word by word.
    let lit = |on| if on { theme.status_on } else { theme.status };
    // Measured as they are written, because an indicator is also a button and
    // the two must not disagree about where it is. Widths are display columns,
    // not characters: a CJK path in the note ahead of them is twice as wide as
    // it is long.
    let mut x = footer.x + spans.iter().map(|s| s.width() as u16).sum::<u16>();
    // Ordered by what each thing acts on, widest first: the program itself,
    // then the cursor, then the tree, then the file. Not by how often they are
    // used, and not by what looks balanced.
    let mut items: Vec<(String, bool, Option<&mut Rect>)> = vec![
        ("m mouse".into(), modes.mouse, Some(&mut zones.mouse)),
        ("q quit".into(), false, None),
        (
            match focus {
                // What `↑↓` does is the whole difference between the panes, so
                // it is said either way.
                Focus::Viewer => "↑↓ scroll",
                // `→` on a file is the deliberate open, and a deliberate open
                // is what hands the keyboard over.
                Focus::Tree => "↑↓ move · → file",
            }
            .into(),
            false,
            None,
        ),
        (
            match focus {
                Focus::Viewer => "← tree",
                Focus::Tree => "← up",
            }
            .into(),
            false,
            None,
        ),
        // The tree's own switch, next to the tree's own key.
        (
            ". dotfiles".into(),
            modes.dotfiles,
            Some(&mut zones.dotfiles),
        ),
    ];
    if has_file {
        items.push(("c copy".into(), modes.copying, Some(&mut zones.copy)));
        items.push(("# Line#".into(), modes.numbers, Some(&mut zones.numbers)));
        items.push((", ·→".into(), modes.marks, Some(&mut zones.marks)));
        // Lit for the same reason `# Line#` is: the tree is on screen. Which
        // makes the copy arrangement three dim indicators and one lit `c`.
        items.push((
            format!("b {}", if modes.folded { "»" } else { "«" }),
            !modes.folded,
            Some(&mut zones.fold),
        ));
    }

    for (i, (label, on, zone)) in items.into_iter().enumerate() {
        // Nothing to separate from, at the front.
        let lead = Span::styled(if i == 0 { "" } else { " · " }, theme.status);
        let label = Span::styled(label, lit(on));
        x += lead.width() as u16;
        if let Some(zone) = zone {
            *zone = Rect {
                x,
                y: footer.y,
                width: label.width() as u16,
                height: 1,
            }
            .intersection(footer);
        }
        x += label.width() as u16;
        spans.push(lead);
        spans.push(label);
    }

    (ratatui::text::Line::from(spans), zones)
}

/// Where each indicator was drawn. They are buttons as much as readouts: the
/// keys are for hands already on the keyboard, and a pointer should not have
/// to learn them.
#[derive(Debug, Clone, Copy, Default)]
struct ModeZones {
    fold: Rect,
    numbers: Rect,
    marks: Rect,
    dotfiles: Rect,
    mouse: Rect,
    copy: Rect,
}

impl ModeZones {
    /// `show_hidden` because the dotfile switch is a command about the tree
    /// rather than a mode of this view, and it is phrased as where to go next.
    fn at(&self, column: u16, row: u16, show_hidden: bool) -> Option<Action> {
        let at = Position::new(column, row);
        for (zone, action) in [
            (self.fold, Action::Fold),
            (self.numbers, Action::LineNumbers),
            (self.marks, Action::Marks),
            (self.mouse, Action::Mouse),
            (self.copy, Action::Copy),
            (
                self.dotfiles,
                Action::Run(Command::SetHidden { show: !show_hidden }),
            ),
        ] {
            if zone.contains(at) {
                return Some(action);
            }
        }
        None
    }
}

/// What the footer reports, and what `c` sets in one go.
#[derive(Debug, Clone, Copy)]
struct Modes {
    folded: bool,
    numbers: bool,
    marks: bool,
    /// Dotfiles are listed. Not a mode of this view — it is the tree's own
    /// switch, and the browser has the same one — but it reads as one.
    dotfiles: bool,
    /// treex has the mouse; the terminal cannot select while it does.
    mouse: bool,
    /// `c` is engaged, so there is a previous state to go back to.
    copying: bool,
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
#[derive(Debug, Clone, PartialEq)]
enum Action {
    Nothing,
    Quit,
    Run(Command),
    /// Scroll the tree, which moves no cursor and so leaves the open file
    /// alone.
    Scroll(i32),
    ScrollFile(Motion),
    Focus(Focus),
    /// The pointer took hold of the divider at this column.
    GrabDivider(u16),
    /// Drag it to this column.
    Split(u16),
    /// The button came up, wherever it was.
    Release,
    /// Fold the tree away, or bring it back.
    Fold,
    /// Show or hide the file's line numbers.
    LineNumbers,
    /// Show or hide space, tab and line-ending marks.
    Marks,
    /// Take the mouse, or give it back to the terminal.
    Mouse,
    /// All three at once: out of the way for a selection, or back as they were.
    Copy,
}

/// How far the file pane moves. Pages are in screenfuls, which only the event
/// loop knows the size of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Motion {
    By(i64),
    Page(i64),
    Top,
    Bottom,
}

impl Motion {
    /// The line to tell the session about, or `None` when this pane cannot
    /// make the move at all.
    ///
    /// That distinction is the whole of how two views of different heights
    /// share one scroll position: a pane already showing the last line has
    /// nothing to say, so pressing `↓` against the end of a short terminal
    /// does not drag a taller browser back up to meet it.
    fn to_share(self, from: usize, lines: usize, page: usize) -> Option<usize> {
        let line = self.applied_to(from, lines, page);
        (line != from).then_some(line)
    }

    /// The first visible line this motion asks for, clamped to a pane `page`
    /// lines tall showing a file of `lines`.
    fn applied_to(self, from: usize, lines: usize, page: usize) -> usize {
        let last = lines.saturating_sub(page.max(1)) as i64;
        let wanted = match self {
            Motion::By(by) => from as i64 + by,
            Motion::Page(pages) => from as i64 + pages * page.max(1) as i64,
            Motion::Top => 0,
            Motion::Bottom => last,
        };
        wanted.clamp(0, last.max(0)) as usize
    }
}

fn handle_key(
    key: KeyEvent,
    snapshot: &crate::state::Snapshot,
    focus: Focus,
    has_file: bool,
) -> Action {
    // Getting out of the way, so that the terminal's own selection lands on
    // nothing but the file.
    //
    // Held down, a modifier makes a different key entirely: `Ctrl-C` is quit,
    // not copy mode, and matching the character alone swallowed it. Shift is
    // not one of them — `#` is Shift-3 on most layouts, and some terminals say
    // so while others do not.
    const CHORD: KeyModifiers = KeyModifiers::CONTROL
        .union(KeyModifiers::ALT)
        .union(KeyModifiers::SUPER);
    if !key.modifiers.intersects(CHORD) {
        match key.code {
            KeyCode::Char('b') if has_file => return Action::Fold,
            KeyCode::Char('#') if has_file => return Action::LineNumbers,
            // Not `/`: that is search everywhere else, and treex will want it.
            KeyCode::Char(',') if has_file => return Action::Marks,
            KeyCode::Char('m') => return Action::Mouse,
            KeyCode::Char('c') if has_file => return Action::Copy,
            _ => {}
        }
    }
    if key.code == KeyCode::Tab && has_file {
        return Action::Focus(match focus {
            Focus::Tree => Focus::Viewer,
            Focus::Viewer => Focus::Tree,
        });
    }
    // The file pane claims the keys that move through a file and nothing else,
    // so `.`, `r` and the rest still reach the tree while reading.
    if focus == Focus::Viewer {
        if let Some(action) = file_key(key) {
            return action;
        }
    }
    tree_key(key, snapshot)
}

fn file_key(key: KeyEvent) -> Option<Action> {
    let scroll = |motion| Some(Action::ScrollFile(motion));
    match (key.code, key.modifiers) {
        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => scroll(Motion::By(1)),
        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => scroll(Motion::By(-1)),
        (KeyCode::PageDown, _) | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            scroll(Motion::Page(1))
        }
        (KeyCode::PageUp, _) | (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
            scroll(Motion::Page(-1))
        }
        (KeyCode::Home, _) | (KeyCode::Char('g'), _) => scroll(Motion::Top),
        (KeyCode::End, _) | (KeyCode::Char('G'), _) => scroll(Motion::Bottom),
        // Back to the tree, not out of the file. Something is always on
        // display now, so there is nothing here to leave — only a keyboard to
        // put back where it came from.
        (KeyCode::Left, _) | (KeyCode::Char('h'), _) => Some(Action::Focus(Focus::Tree)),
        _ => None,
    }
}

fn tree_key(key: KeyEvent, snapshot: &crate::state::Snapshot) -> Action {
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
    panes: Panes,
    offset: usize,
    opts: &TuiOptions,
    dragging: bool,
) -> Action {
    let at = || hit::hit(panes.rows, offset, &snapshot.rows, mouse.column, mouse.row);
    let over_file = panes.file.contains(Position::new(mouse.column, mouse.row));

    // The footer's indicators are buttons too, and they belong to no pane.
    if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
        if let Some(action) = panes
            .modes
            .at(mouse.column, mouse.row, snapshot.show_hidden)
        {
            return action;
        }
    }

    match mouse.kind {
        // The header's control before the pane it sits in.
        MouseEventKind::Down(MouseButton::Left)
            if panes.fold.contains(Position::new(mouse.column, mouse.row)) =>
        {
            Action::Fold
        }
        // The divider belongs to neither pane, so it is asked about first.
        MouseEventKind::Down(MouseButton::Left) if panes.on_divider(mouse.column) => {
            Action::GrabDivider(mouse.column)
        }
        MouseEventKind::Drag(MouseButton::Left) if dragging => Action::Split(mouse.column),
        MouseEventKind::Up(_) => Action::Release,
        // The wheel scrolls whatever it is pointing at.
        MouseEventKind::ScrollDown if over_file => Action::ScrollFile(Motion::By(3)),
        MouseEventKind::ScrollUp if over_file => Action::ScrollFile(Motion::By(-3)),
        MouseEventKind::ScrollDown => Action::Scroll(3),
        MouseEventKind::ScrollUp => Action::Scroll(-3),
        // Working the pane is what says the file is the subject rather than
        // whatever directory the cursor was left on.
        MouseEventKind::Down(_) if over_file => Action::Focus(Focus::Viewer),

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
                // One click is all it takes: selecting a file is what shows
                // it, so there is no second gesture left to invent.
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
            ignored: false,
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
            view_line: 0,
        }
    }

    /// A keypress with the tree focused, which is where every existing key
    /// rule applies.
    fn press(code: KeyCode, snap: &Snapshot) -> Action {
        handle_key(
            KeyEvent::new(code, KeyModifiers::NONE),
            snap,
            Focus::Tree,
            false,
        )
    }

    /// The same press with a file open and the file pane focused.
    fn press_in_file(code: KeyCode, snap: &Snapshot) -> Action {
        handle_key(
            KeyEvent::new(code, KeyModifiers::NONE),
            snap,
            Focus::Viewer,
            true,
        )
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
    fn left_belongs_to_the_tree_now_that_the_pane_is_not_a_mode() {
        let rows = vec![row("dir", true, true)];
        let collapse = Action::Run(Command::Collapse {
            path: "/root/dir".into(),
        });

        // A file being up is the normal state, so it cannot be what `←`
        // answers: it collapses, exactly as it would with nothing open.
        assert_eq!(
            press(KeyCode::Left, &snapshot(rows.clone(), 0, Some(0))),
            collapse
        );
        assert_eq!(press(KeyCode::Left, &snapshot(rows, 0, None)), collapse);
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

    /// The chord is a different key from the character in it. `c` is copy
    /// mode, `Ctrl-C` is the way out — and matching the character alone took
    /// the way out away.
    #[test]
    fn a_modifier_makes_it_a_different_key() {
        let snap = snapshot(vec![row("a.txt", false, false)], 0, Some(0));
        let chord = |c| {
            handle_key(
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL),
                &snap,
                Focus::Tree,
                true,
            )
        };
        assert_eq!(chord('c'), Action::Quit, "Ctrl-C must still leave");
        assert_eq!(chord('b'), Action::Nothing, "Ctrl-B is not the fold key");
        assert_eq!(chord('m'), Action::Nothing);

        // Shift is not a chord: `#` is Shift-3 wherever it is not its own key.
        assert_eq!(
            handle_key(
                KeyEvent::new(KeyCode::Char('#'), KeyModifiers::SHIFT),
                &snap,
                Focus::Tree,
                true,
            ),
            Action::LineNumbers
        );
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        let snap = snapshot(vec![row("a", false, false)], 0, None);
        assert_eq!(press(KeyCode::Char('q'), &snap), Action::Quit);
        assert_eq!(
            handle_key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &snap,
                Focus::Tree,
                false,
            ),
            Action::Quit
        );
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// A press on a screen row, which with the area starting at y = 0 is also
    /// the row index.
    fn tap_row(snap: &Snapshot, index: u16) -> Action {
        handle_mouse(
            click(6, index),
            snap,
            Panes {
                rows: Rect::new(0, 0, 40, 10),
                file: Rect::default(),
                modes: ModeZones::default(),
                divider: None,
                fold: Rect::default(),
            },
            0,
            &TuiOptions::default(),
            false,
        )
    }

    #[test]
    fn one_press_on_a_file_is_all_it_takes() {
        let snap = snapshot(
            vec![row("dir", true, false), row("a.txt", false, false)],
            0,
            None,
        );
        // Selecting a file is what shows it, so there is nothing for a second
        // press to mean.
        assert_eq!(
            tap_row(&snap, 1),
            Action::Run(Command::Select {
                path: "/root/a.txt".into()
            })
        );
        assert_eq!(
            tap_row(&snap, 1),
            Action::Run(Command::Select {
                path: "/root/a.txt".into()
            }),
            "a second press is the same press, not a different gesture"
        );
    }

    #[test]
    fn pressing_inside_the_file_pane_makes_it_the_subject() {
        let snap = snapshot(vec![row("a.txt", false, false)], 0, Some(0));
        let action = handle_mouse(
            click(25, 2),
            &snap,
            Panes {
                rows: Rect::new(0, 0, 20, 10),
                file: Rect::new(20, 0, 20, 10),
                modes: ModeZones::default(),
                divider: None,
                fold: Rect::default(),
            },
            0,
            &TuiOptions::default(),
            false,
        );
        assert_eq!(
            action,
            Action::Focus(Focus::Viewer),
            "which also puts the cursor back on the file being read"
        );
    }

    #[test]
    fn the_right_button_does_nothing() {
        let snap = snapshot(
            vec![row("dir", true, false), row("a.txt", false, false)],
            0,
            None,
        );
        for index in [0, 1] {
            let event = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 6,
                row: index,
                modifiers: KeyModifiers::NONE,
            };
            assert_eq!(
                handle_mouse(
                    event,
                    &snap,
                    Panes {
                        rows: Rect::new(0, 0, 40, 10),
                        file: Rect::default(),
                        modes: ModeZones::default(),
                        divider: None,
                        fold: Rect::default(),
                    },
                    0,
                    &TuiOptions::default(),
                    false,
                ),
                Action::Nothing,
                "the right button is reserved, so it must not act on row {index}"
            );
        }
    }

    #[test]
    fn offset_follows_the_selection_into_view() {
        assert_eq!(clamp_offset(0, Some(30), 10, 100), 21);
        assert_eq!(clamp_offset(50, Some(3), 10, 100), 3);
        assert_eq!(clamp_offset(0, Some(5), 10, 100), 0);
    }

    /// The wheel's case: with nothing to reveal, an offset is left where it
    /// was put. The tree scrolls away from the cursor and stays there.
    #[test]
    fn an_offset_with_no_cursor_to_reveal_is_left_alone() {
        assert_eq!(clamp_offset(40, None, 10, 100), 40);
        assert_eq!(
            clamp_offset(40, Some(0), 10, 100),
            0,
            "a moved cursor still wins"
        );
    }

    #[test]
    fn offset_never_scrolls_past_the_end() {
        assert_eq!(clamp_offset(99, None, 10, 20), 10);
        assert_eq!(clamp_offset(5, None, 10, 3), 0);
    }

    #[test]
    fn the_file_pane_scrolls_without_moving_the_tree_cursor() {
        let snap = snapshot(vec![row("a.txt", false, false)], 0, Some(0));

        // Moving the cursor is what closes a file, so while the file has the
        // keyboard these must not be cursor commands.
        for (code, want) in [
            (KeyCode::Down, Action::ScrollFile(Motion::By(1))),
            (KeyCode::Up, Action::ScrollFile(Motion::By(-1))),
            (KeyCode::Char('j'), Action::ScrollFile(Motion::By(1))),
            (KeyCode::Char('k'), Action::ScrollFile(Motion::By(-1))),
            (KeyCode::PageDown, Action::ScrollFile(Motion::Page(1))),
            (KeyCode::Home, Action::ScrollFile(Motion::Top)),
            (KeyCode::End, Action::ScrollFile(Motion::Bottom)),
            (KeyCode::Char('G'), Action::ScrollFile(Motion::Bottom)),
        ] {
            assert_eq!(press_in_file(code, &snap), want, "{code:?}");
        }
    }

    #[test]
    fn the_same_keys_still_move_the_cursor_when_the_tree_has_the_keyboard() {
        let snap = snapshot(vec![row("a.txt", false, false)], 0, Some(0));
        assert_eq!(
            press(KeyCode::Down, &snap),
            Action::Run(Command::MoveSelection { delta: 1 }),
            "a file being open must not change what the tree's own keys do"
        );
    }

    #[test]
    fn left_puts_the_keyboard_back_in_the_tree_without_closing_the_file() {
        let snap = snapshot(vec![row("a.txt", false, false)], 0, Some(0));
        assert_eq!(
            press_in_file(KeyCode::Left, &snap),
            Action::Focus(Focus::Tree),
            "a file is always on display now, so there is nothing to leave"
        );
        // From the tree it is a tree key again: up to the parent directory.
        assert_eq!(
            press(KeyCode::Left, &snap),
            Action::Run(Command::Select {
                path: "/root".into()
            })
        );
    }

    #[test]
    fn tab_moves_between_the_panes_and_does_nothing_without_one() {
        let snap = snapshot(vec![row("a.txt", false, false)], 0, Some(0));
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);

        assert_eq!(
            handle_key(tab, &snap, Focus::Tree, true),
            Action::Focus(Focus::Viewer)
        );
        assert_eq!(
            handle_key(tab, &snap, Focus::Viewer, true),
            Action::Focus(Focus::Tree)
        );
        assert_eq!(handle_key(tab, &snap, Focus::Tree, false), Action::Nothing);
    }

    /// The footer's indicators are buttons, and they are measured where they
    /// are written — including past something as wide as a CJK path.
    #[test]
    fn the_mode_indicators_can_be_clicked() {
        let footer = Rect::new(0, 9, 120, 1);
        let modes = Modes {
            folded: false,
            numbers: true,
            marks: true,
            dotfiles: true,
            mouse: true,
            copying: false,
        };
        let opts = TuiOptions {
            status_note: Some("中文路徑".into()),
            ..TuiOptions::default()
        };
        let (line, zones) = status_line(
            &opts,
            &Theme::default(),
            Some(&Viewer::loading(std::sync::Arc::from(Path::new(
                "/root/a.txt",
            )))),
            Focus::Tree,
            modes,
            footer,
        );

        for (zone, want) in [
            (zones.fold, Action::Fold),
            (zones.numbers, Action::LineNumbers),
            (zones.marks, Action::Marks),
            (zones.mouse, Action::Mouse),
            (zones.copy, Action::Copy),
            (
                zones.dotfiles,
                Action::Run(Command::SetHidden { show: false }),
            ),
        ] {
            assert!(zone.width > 0, "{want:?} was never given a place");
            assert_eq!(zones.at(zone.x, footer.y, true), Some(want.clone()));
            assert_eq!(
                zones.at(zone.right() - 1, footer.y, true),
                Some(want),
                "the far end of the label counts too"
            );
        }
        assert_eq!(
            zones.at(0, footer.y, true),
            None,
            "the note is not a button"
        );
        assert_eq!(
            zones.at(zones.fold.x, footer.y - 1, true),
            None,
            "nor is the row above"
        );

        // With nothing open, the indicators about a file are absent and the
        // rest are in the same places and still work — one footer, not two.
        let (_, bare) = status_line(&opts, &Theme::default(), None, Focus::Tree, modes, footer);
        assert_eq!(bare.fold.width, 0, "no file, so nothing to fold away from");
        assert_eq!(bare.numbers.width, 0);
        assert_eq!(bare.marks.width, 0);
        assert_eq!(bare.copy.width, 0);
        assert!(bare.mouse.width > 0, "the mouse is not about a file");
        assert!(bare.dotfiles.width > 0, "nor is the dotfile switch");
        assert_eq!(
            bare.at(bare.dotfiles.x, footer.y, true),
            Some(Action::Run(Command::SetHidden { show: false })),
            "and it is still a button"
        );

        // Written where they were measured. Measured in display columns, not
        // bytes and not characters: the note in front of these is four CJK
        // characters, which is twelve bytes and eight columns.
        let drawn = line.to_string();
        let upto = drawn.find("b «").expect("the indicator was never drawn");
        assert_eq!(
            ratatui::text::Span::from(&drawn[..upto]).width() as u16,
            zones.fold.x,
            "the button is beside the word rather than on it"
        );
    }

    #[test]
    fn the_fold_control_is_a_click_target_and_a_key() {
        let snap = snapshot(vec![row("a.txt", false, false)], 0, Some(0));
        // The file pane starts at column 20; framed, its header sits one in.
        let fold = viewer::fold_zone(Rect::new(20, 0, 20, 10), true);
        assert_eq!((fold.x, fold.y, fold.width), (21, 0, 3));

        let press = |column| {
            handle_mouse(
                click(column, 0),
                &snap,
                Panes {
                    rows: Rect::new(0, 0, 20, 10),
                    file: Rect::new(20, 1, 20, 9),
                    modes: ModeZones::default(),
                    divider: None,
                    fold,
                },
                0,
                &TuiOptions::default(),
                false,
            )
        };
        assert_eq!(press(21), Action::Fold);
        assert_eq!(press(23), Action::Fold, "three columns wide, not one");
        assert_eq!(press(24), Action::Nothing, "and no wider than it looks");

        // Folded, the header is the pane's own first line.
        let folded = viewer::fold_zone(Rect::new(0, 0, 40, 10), false);
        assert_eq!((folded.x, folded.y), (0, 0));
    }

    /// The three that clear the way for the terminal's own selection.
    #[test]
    fn getting_out_of_the_way_is_three_toggles() {
        let snap = snapshot(vec![row("a.txt", false, false)], 0, Some(0));
        let with_file =
            |code, focus| handle_key(KeyEvent::new(code, KeyModifiers::NONE), &snap, focus, true);

        // From either pane, since a reader may be in either.
        for focus in [Focus::Tree, Focus::Viewer] {
            assert_eq!(with_file(KeyCode::Char('b'), focus), Action::Fold);
            assert_eq!(with_file(KeyCode::Char('#'), focus), Action::LineNumbers);
            assert_eq!(with_file(KeyCode::Char(','), focus), Action::Marks);
            assert_eq!(with_file(KeyCode::Char('m'), focus), Action::Mouse);
        }

        assert_eq!(with_file(KeyCode::Char('c'), Focus::Tree), Action::Copy);

        // With no file up there is nothing to get out of the way of, but the
        // mouse is still the terminal's to take back.
        assert_eq!(press(KeyCode::Char('b'), &snap), Action::Nothing);
        assert_eq!(press(KeyCode::Char('c'), &snap), Action::Nothing);
        assert_eq!(press(KeyCode::Char('m'), &snap), Action::Mouse);
    }

    #[test]
    fn keys_the_file_pane_does_not_claim_still_reach_the_tree() {
        let snap = snapshot(vec![row("a.txt", false, false)], 0, Some(0));
        assert_eq!(
            press_in_file(KeyCode::Char('.'), &snap),
            Action::Run(Command::SetHidden { show: false })
        );
        assert_eq!(press_in_file(KeyCode::Char('q'), &snap), Action::Quit);
    }

    #[test]
    fn the_wheel_scrolls_whichever_pane_it_is_over() {
        let snap = snapshot(vec![row("a.txt", false, false)], 0, Some(0));
        let panes = Panes {
            rows: Rect::new(0, 0, 20, 10),
            file: Rect::new(20, 0, 20, 10),
            modes: ModeZones::default(),
            divider: None,
            fold: Rect::default(),
        };
        let wheel = |column| MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        let run = |event| handle_mouse(event, &snap, panes, 0, &TuiOptions::default(), false);
        assert_eq!(run(wheel(5)), Action::Scroll(3));
        assert_eq!(run(wheel(25)), Action::ScrollFile(Motion::By(3)));
    }

    #[test]
    fn a_press_names_the_pane_it_landed_in() {
        let rows = Rect::new(0, 0, 20, 10);
        let file = Rect::new(20, 0, 20, 10);
        assert_eq!(pane_at(rows, file, 5, 2), Some(Focus::Tree));
        assert_eq!(pane_at(rows, file, 25, 2), Some(Focus::Viewer));
        assert_eq!(pane_at(rows, file, 5, 20), None);
        // With no file open there is no pane to land in.
        assert_eq!(pane_at(rows, Rect::default(), 25, 2), None);
    }

    #[test]
    fn the_tree_gives_up_the_screen_when_it_cannot_have_half_of_it() {
        let wide = render::panes(Rect::new(0, 0, 120, 30), true, None, false);
        assert!(wide.tree.is_some() && wide.viewer.is_some());
        assert!(
            wide.tree.unwrap().width >= render::TREE_MIN_WIDTH,
            "the tree never gets less than it can draw a path in"
        );

        let narrow = render::panes(Rect::new(0, 0, 60, 30), true, None, false);
        assert!(narrow.tree.is_none(), "60 columns cannot hold both panes");
        assert_eq!(narrow.viewer.unwrap().width, 60);

        // Nothing open: the tree has it all, as it always did.
        let closed = render::panes(Rect::new(0, 0, 120, 30), false, None, false);
        assert!(closed.tree.is_some() && closed.viewer.is_none());
        assert_eq!(closed.tree.unwrap().width, 120);
    }

    /// A press on the divider, a drag, and the release that ends it.
    fn divider_drag(dragging: bool, event: MouseEvent) -> Action {
        let snap = snapshot(vec![row("a.txt", false, false)], 0, Some(0));
        handle_mouse(
            event,
            &snap,
            Panes {
                rows: Rect::new(1, 1, 18, 10),
                file: Rect::new(21, 1, 18, 10),
                modes: ModeZones::default(),
                divider: Some((19, 20)),
                fold: Rect::default(),
            },
            0,
            &TuiOptions::default(),
            dragging,
        )
    }

    fn at(kind: MouseEventKind, column: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            // The panes above start at y = 1, so this is their first row.
            row: 1,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn the_divider_can_be_taken_hold_of_from_either_of_its_columns() {
        let press = MouseEventKind::Down(MouseButton::Left);
        assert_eq!(divider_drag(false, at(press, 19)), Action::GrabDivider(19));
        assert_eq!(divider_drag(false, at(press, 20)), Action::GrabDivider(20));
        // One column further in is the tree again, not the divider.
        assert!(matches!(
            divider_drag(false, at(press, 18)),
            Action::Run(Command::Select { .. })
        ));
    }

    #[test]
    fn dragging_moves_the_divider_and_the_button_coming_up_ends_it() {
        let drag = MouseEventKind::Drag(MouseButton::Left);
        assert_eq!(divider_drag(true, at(drag, 40)), Action::Split(40));
        assert_eq!(
            divider_drag(false, at(drag, 40)),
            Action::Nothing,
            "a drag that did not start on the divider moves nothing"
        );
        assert_eq!(
            divider_drag(true, at(MouseEventKind::Up(MouseButton::Left), 40)),
            Action::Release
        );
    }

    #[test]
    fn a_dragged_split_is_honored_and_kept_off_both_minimums() {
        let area = Rect::new(0, 0, 120, 30);
        let at = |split| {
            render::panes(area, true, Some(split), false)
                .tree
                .unwrap()
                .width
        };

        assert_eq!(at(70), 70, "a width that fits is taken as asked");
        assert_eq!(at(5), render::TREE_MIN_WIDTH, "the tree keeps its minimum");
        assert_eq!(
            at(119),
            120 - render::VIEWER_MIN_WIDTH,
            "and so does the file"
        );

        // Past the default cap, which only applies when nobody chose.
        assert!(at(90) > 60);
        assert_eq!(
            render::panes(area, true, None, false).tree.unwrap().width,
            30,
            "the default is a sidebar's share, not half the screen"
        );
    }

    #[test]
    fn the_divider_sits_between_the_panes_and_is_gone_with_the_file() {
        let area = Rect::new(0, 0, 120, 30);
        let panes = render::panes(area, true, Some(40), false);
        assert_eq!(panes.divider(), Some((39, 40)));
        assert!(panes.on_divider(39) && panes.on_divider(40));
        assert!(!panes.on_divider(38) && !panes.on_divider(41));

        assert_eq!(
            render::panes(area, false, None, false).divider(),
            None,
            "nothing to drag when the tree has the screen"
        );
        assert_eq!(
            render::panes(Rect::new(0, 0, 60, 30), true, None, false).divider(),
            None,
            "nor when the file has taken it"
        );

        let folded = render::panes(area, true, None, true);
        assert!(
            folded.tree.is_none(),
            "folded away, however wide the window"
        );
        assert_eq!(folded.viewer.unwrap().width, 120);
        assert_eq!(folded.divider(), None, "and nothing left to drag");
    }

    #[test]
    fn a_motion_is_clamped_to_the_pane_it_is_made_in() {
        // 100 lines in a window 10 tall: line 90 is as far as it goes.
        let ten = |motion: Motion, from| motion.applied_to(from, 100, 10);

        assert_eq!(ten(Motion::By(1), 0), 1);
        assert_eq!(ten(Motion::By(-1), 0), 0, "never past the top");
        assert_eq!(ten(Motion::Page(1), 0), 10);
        assert_eq!(ten(Motion::Bottom, 0), 90);
        assert_eq!(ten(Motion::By(1), 90), 90, "nor past the end");
        assert_eq!(ten(Motion::Top, 90), 0);

        // A file that fits has nowhere to go at all.
        assert_eq!(Motion::Bottom.applied_to(0, 5, 40), 0);
    }

    #[test]
    fn a_scroll_this_pane_cannot_make_is_not_shared() {
        // 100 lines, 10 on screen, already at the end.
        assert_eq!(Motion::By(1).to_share(90, 100, 10), None);
        assert_eq!(Motion::Bottom.to_share(90, 100, 10), None);
        assert_eq!(Motion::By(-1).to_share(0, 100, 10), None);
        // A file that fits has no scrolling to share at all.
        assert_eq!(Motion::Page(1).to_share(0, 5, 40), None);

        // A move it can make is shared, clamped to what it can show.
        assert_eq!(Motion::By(1).to_share(0, 100, 10), Some(1));
        assert_eq!(Motion::Page(5).to_share(0, 100, 10), Some(50));
        assert_eq!(
            Motion::Page(20).to_share(0, 100, 10),
            Some(90),
            "clamped, but a move"
        );
    }
}
