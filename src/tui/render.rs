//! Drawing one flattened tree into a ratatui frame.
//!
//! The geometry here is shared with hit-testing in [`super::hit`]: a row's
//! twistie always sits at `INDENT * depth`, which is what makes a click land on
//! the thing the user aimed at.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use super::viewer::Viewer;
use crate::state::Snapshot;
use crate::tree::{Kind, Row};

pub const INDENT: u16 = 2;
pub const TWISTIE_WIDTH: u16 = 2;

pub struct Theme {
    pub dir: Style,
    pub file: Style,
    pub special: Style,
    pub broken: Style,
    /// Layered on top of the kind's style when the entry is a symlink.
    pub symlink: Modifier,
    pub guide: Style,
    /// The cursor, with the keyboard in the tree.
    pub selected: Style,
    /// The cursor with the keyboard in the file, where `↑↓` scrolls what is on
    /// the right rather than moving this.
    pub reading: Style,
    pub status: Style,
    /// A mode indicator that is currently on. The dim `status` is off.
    pub status_on: Style,
    /// Space, tab and line endings, where they are drawn at all.
    pub marks: Style,
    /// What git ignores, and `.git` itself. Replaces the kind's color rather
    /// than tinting it: the point is that the row is not what you came for.
    pub ignored: Style,
    /// File contents where `bat` said nothing about them — and the base every
    /// colored run is layered onto.
    pub code: Style,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            dir: Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
            file: Style::new(),
            special: Style::new().fg(Color::Yellow),
            broken: Style::new()
                .fg(Color::Red)
                .add_modifier(Modifier::CROSSED_OUT),
            symlink: Modifier::ITALIC,
            guide: Style::new().fg(Color::DarkGray),
            // Foregrounds are set explicitly throughout: file rows carry no
            // color of their own, and a light terminal would otherwise put
            // dark text on these dark backgrounds.
            selected: Style::new()
                .bg(Color::Indexed(238))
                .fg(Color::Indexed(253))
                .add_modifier(Modifier::BOLD),
            reading: Style::new()
                .bg(Color::Indexed(91))
                .fg(Color::Indexed(231))
                .add_modifier(Modifier::BOLD),
            status: Style::new().fg(Color::DarkGray),
            status_on: Style::new().fg(Color::Indexed(252)),
            marks: Style::new().fg(Color::Indexed(238)),
            ignored: Style::new().fg(Color::DarkGray),
            code: Style::new(),
        }
    }
}

pub fn twistie(row: &Row) -> &'static str {
    if !row.is_dir() {
        "  "
    } else if row.expanded {
        "▾ "
    } else {
        "▸ "
    }
}

fn line_for(row: &Row, theme: &Theme, cursor: bool, reading: bool) -> Line<'static> {
    let mut spans = Vec::with_capacity(4);

    if row.depth > 0 {
        spans.push(Span::styled("│ ".repeat(row.depth), theme.guide));
    }
    spans.push(Span::styled(twistie(row), theme.guide));

    let mut style = match row.kind {
        _ if row.ignored => theme.ignored,
        Kind::Dir => theme.dir,
        Kind::File => theme.file,
        Kind::Broken => theme.broken,
        _ => theme.special,
    };
    if row.symlink {
        style = style.add_modifier(theme.symlink);
    }
    // `ls -F` suffixes, so a socket or a fifo is not mistaken for a file.
    let name = format!("{}{}", row.name, row.kind.suffix());
    spans.push(Span::styled(name, style));
    if row.omitted > 0 {
        spans.push(Span::styled(
            format!("  … {} more", row.omitted),
            theme.status,
        ));
    }

    let mut line = Line::from(spans);
    // One highlight, and its color says where the keyboard is. Marking the
    // displayed file separately stopped being worth a color when the cursor
    // started displaying whatever it lands on: it is the cursor row almost
    // always, and the file pane's own header names it the rest of the time.
    if cursor {
        line = line.style(if reading {
            theme.reading
        } else {
            theme.selected
        });
    }
    line
}

pub struct View<'a> {
    pub snapshot: &'a Snapshot,
    pub offset: usize,
    pub theme: &'a Theme,
    /// Built by the caller, because each mode indicator is colored by whether
    /// it is currently on.
    pub status: Line<'static>,
    pub viewer: Option<&'a Viewer>,
    /// Where the divider was dragged to, if it was.
    pub split: Option<u16>,
    /// The tree is folded away and the file has the window.
    pub folded: bool,
    pub numbers: bool,
    /// Draw space, tab and line endings.
    pub marks: bool,
    /// The keyboard is in the file pane, so `↑↓` scrolls it.
    pub reading: bool,
}

/// Where the two panes are. The tree keeps the screen to itself until a file
/// is open, and gives it up entirely when the terminal is too narrow to show
/// both — a 40-column phone-sized window reads one thing at a time.
pub struct Panes {
    /// Absent when the file pane has taken the whole body.
    pub tree: Option<Rect>,
    pub viewer: Option<Rect>,
    pub footer: Rect,
}

/// Narrower than this and a pane is not worth drawing.
pub const TREE_MIN_WIDTH: u16 = 26;
pub const VIEWER_MIN_WIDTH: u16 = 46;

impl Panes {
    /// The columns the divider is drawn in: the tree's right border and the
    /// file's left one, which sit next to each other. Both are grabbable —
    /// a one-column target is hard to hit with a mouse and impossible with a
    /// finger.
    pub fn divider(&self) -> Option<(u16, u16)> {
        let tree = self.tree?;
        self.viewer?;
        Some((tree.right().saturating_sub(1), tree.right()))
    }

    pub fn on_divider(&self, column: u16) -> bool {
        matches!(self.divider(), Some((left, right)) if column == left || column == right)
    }
}

/// `split` is a tree width the user dragged the divider to; `None` takes the
/// default share. Either way both panes keep their minimum.
pub fn panes(area: Rect, viewing: bool, split: Option<u16>, folded: bool) -> Panes {
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    let (body, footer) = (chunks[0], chunks[1]);

    if !viewing {
        return Panes {
            tree: Some(body),
            viewer: None,
            footer,
        };
    }
    if folded || body.width < TREE_MIN_WIDTH + VIEWER_MIN_WIDTH {
        return Panes {
            tree: None,
            viewer: Some(body),
            footer,
        };
    }

    // A quarter to the tree: a sidebar, the way an editor lays this out — the
    // file is what is being read. A dragged width is not capped the same way,
    // since past it is a deliberate answer to what the default guessed.
    let width = split
        .unwrap_or((body.width / 4).clamp(TREE_MIN_WIDTH, 44))
        .clamp(TREE_MIN_WIDTH, body.width - VIEWER_MIN_WIDTH);
    let split = Layout::horizontal([Constraint::Length(width), Constraint::Min(1)]).split(body);
    Panes {
        tree: Some(split[0]),
        viewer: Some(split[1]),
        footer,
    }
}

/// The rows' own rect inside a pane's border. Hit-testing needs it to turn a
/// click into a row index.
pub fn inner(area: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(area)
}

pub fn draw(frame: &mut Frame, view: &View) {
    let panes = panes(frame.area(), view.viewer.is_some(), view.split, view.folded);

    if let Some(area) = panes.tree {
        // The count sits with the path rather than at the far end of the
        // border: it is part of what this pane is showing, not a corner label.
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Line::from(vec![
                Span::raw(format!(" {} ", view.snapshot.root.display())),
                Span::styled(
                    format!("· {} rows ", view.snapshot.rows.len()),
                    view.theme.status,
                ),
            ]));
        let rows = block.inner(area);
        frame.render_widget(block, area);

        let height = rows.height as usize;
        let lines: Vec<Line> = view
            .snapshot
            .rows
            .iter()
            .enumerate()
            .skip(view.offset)
            .take(height)
            .map(|(i, row)| {
                line_for(
                    row,
                    view.theme,
                    view.snapshot.selected == Some(i),
                    view.reading,
                )
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), rows);
    }

    if let (Some(area), Some(viewer)) = (panes.viewer, view.viewer) {
        // With no tree beside it there is nothing for a border to separate,
        // and a border is one more thing a dragged selection would pick up.
        let framed = panes.tree.is_some();
        // Read from the row each frame rather than remembered when the file
        // was opened, so editing a `.gitignore` moves the header's color along
        // with the tree's.
        let ignored = view
            .snapshot
            .viewing
            .and_then(|i| view.snapshot.rows.get(i))
            .is_some_and(|row| row.ignored);
        super::viewer::draw(
            frame,
            area,
            viewer,
            view.theme,
            &super::viewer::Look {
                numbers: view.numbers,
                marks: view.marks,
                framed,
                ignored,
            },
        );
    }

    frame.render_widget(Paragraph::new(view.status.clone()), panes.footer);
}
