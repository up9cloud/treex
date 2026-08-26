//! Drawing one flattened tree into a ratatui frame.
//!
//! The geometry here is shared with hit-testing in [`super::hit`]: a row's
//! twistie always sits at `INDENT * depth`, which is what makes a click land on
//! the thing the user aimed at.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

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
    /// The cursor: this row is picked out, nothing more.
    pub selected: Style,
    /// The cursor on a file that is open for reading — the second step.
    pub viewing: Style,
    pub status: Style,
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
            viewing: Style::new()
                .bg(Color::Indexed(91))
                .fg(Color::Indexed(231))
                .add_modifier(Modifier::BOLD),
            status: Style::new().fg(Color::DarkGray),
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

fn line_for(row: &Row, theme: &Theme, selected: bool, viewing: bool) -> Line<'static> {
    let mut spans = Vec::with_capacity(4);

    if row.depth > 0 {
        spans.push(Span::styled("│ ".repeat(row.depth), theme.guide));
    }
    spans.push(Span::styled(twistie(row), theme.guide));

    let mut style = match row.kind {
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
    if viewing {
        line = line.style(theme.viewing);
    } else if selected {
        line = line.style(theme.selected);
    }
    line
}

pub struct View<'a> {
    pub snapshot: &'a Snapshot,
    pub offset: usize,
    pub theme: &'a Theme,
    pub status: &'a str,
}

pub fn draw(frame: &mut Frame, view: &View) {
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(frame.area());
    let (body, footer) = (chunks[0], chunks[1]);

    let title = format!(" {} ", view.snapshot.root.display());
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(body);
    frame.render_widget(block, body);

    let height = inner.height as usize;
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
                view.snapshot.viewing == Some(i),
            )
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
    frame.render_widget(
        Paragraph::new(Line::styled(view.status.to_string(), view.theme.status)),
        footer,
    );
}

/// The body area's inner rect, i.e. where rows are actually drawn. Hit-testing
/// needs this to convert a click into a row index.
pub fn body_inner(area: Rect) -> Rect {
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    Block::default().borders(Borders::ALL).inner(chunks[0])
}
