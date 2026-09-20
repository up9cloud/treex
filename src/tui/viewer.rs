//! The file pane: what `viewing` actually shows.
//!
//! The pane holds its own copy of the file because the session does not: the
//! shared state says *which* file is open, and each view reads it for itself.

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use super::render::Theme;
use crate::highlight::Run;
use crate::preview::Preview;

/// How far apart tab stops are drawn. The bytes are left alone — this is only
/// what the terminal shows, since a raw tab in a cell draws as nothing.
const TAB: usize = 4;

/// What the invisible characters are drawn as. `bat`'s vocabulary, since that
/// is what the rest of this pane already speaks.
const SPACE: &str = "·";
const TAB_MARK: &str = "→";
const CR: &str = "␍";
const LF: &str = "␊";

pub struct Viewer {
    pub path: Arc<Path>,
    pub name: String,
    pub body: Body,
    /// First visible line.
    pub offset: usize,
}

pub enum Body {
    Loading,
    Text(Text),
    /// Too large, binary, or unreadable — the reason, in the reader's words.
    Notice(String),
}

pub struct Text {
    content: String,
    /// Byte range of each line, without its newline or a CRLF's carriage
    /// return.
    lines: Vec<Range<usize>>,
    runs: Vec<Run>,
    size: u64,
    lossy: bool,
}

impl Viewer {
    pub fn loading(path: Arc<Path>) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            path,
            name,
            body: Body::Loading,
            offset: 0,
        }
    }

    /// Takes the result of a read, which arrived some time after the pane was
    /// opened.
    pub fn fill(&mut self, preview: Preview, runs: Vec<Run>) {
        self.offset = 0;
        self.body = match preview {
            Preview::Ok {
                content,
                stamp,
                lossy,
            } => {
                let lines = line_ranges(&content);
                Body::Text(Text {
                    content,
                    lines,
                    runs,
                    size: stamp.size,
                    lossy,
                })
            }
            Preview::TooLarge { size, limit } => Body::Notice(format!(
                "{} is over the {} preview limit\nraise it with --max-preview-size",
                bytes(size),
                bytes(limit)
            )),
            Preview::Binary { size } => Body::Notice(format!("binary file, {}", bytes(size))),
            Preview::Unreadable { reason } => Body::Notice(reason),
            // Only sent to a caller that said what it already held, which this
            // one never does.
            Preview::Unchanged => Body::Notice("unchanged".into()),
        };
    }

    pub fn line_count(&self) -> usize {
        match &self.body {
            Body::Text(text) => text.lines.len(),
            _ => 0,
        }
    }

    /// Puts `line` at the top, as far as a pane `viewport` lines tall can.
    /// A shorter pane than the one that chose the line simply sits at its own
    /// end rather than refusing to follow.
    pub fn scroll_to(&mut self, line: usize, viewport: usize) {
        let last = self.line_count().saturating_sub(viewport.max(1));
        self.offset = line.min(last);
    }

    /// What the header says about this file beyond its name — which the
    /// header is already showing beside this.
    pub fn detail(&self) -> String {
        match &self.body {
            Body::Loading => " · reading…".into(),
            // Whatever is wrong is written across the pane itself.
            Body::Notice(_) => String::new(),
            Body::Text(text) => {
                let lines = text.lines.len();
                let mut s = format!(" · {} · {lines} lines", bytes(text.size));
                if text.lossy {
                    s.push_str(" · not UTF-8");
                }
                s
            }
        }
    }
}

/// Where each line starts and ends. A file ending in a newline does not have
/// one more line than it looks — the same rule the browser's gutter follows.
fn line_ranges(content: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            let mut end = i;
            if end > start && content.as_bytes()[end - 1] == b'\r' {
                end -= 1;
            }
            ranges.push(start..end);
            start = i + 1;
        }
    }
    if start < content.len() {
        ranges.push(start..content.len());
    }
    ranges
}

fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn style_of(style: &crate::highlight::Style, theme: &Theme) -> Style {
    let mut out = theme.code;
    if let Some(color) = style.fg {
        out = out.fg(match color {
            crate::highlight::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
            crate::highlight::Color::Indexed(i) => Color::Indexed(i),
        });
    }
    for (on, modifier) in [
        (style.bold, Modifier::BOLD),
        (style.dim, Modifier::DIM),
        (style.italic, Modifier::ITALIC),
        (style.underline, Modifier::UNDERLINED),
    ] {
        if on {
            out = out.add_modifier(modifier);
        }
    }
    out
}

/// One line as styled spans, with tabs expanded to the next stop.
fn line_spans<'a>(text: &'a Text, line: &Range<usize>, theme: &Theme, marks: bool) -> Line<'a> {
    let mut spans: Vec<Span<'a>> = Vec::new();
    let mut column = 0;

    // The runs tile the file in order, so the first one that reaches into this
    // line is where to start.
    let first = text.runs.partition_point(|run| run.end <= line.start);
    let mut at = line.start;

    let mut push = |slice: &'a str, style: Style, column: &mut usize| {
        for piece in slice.split_inclusive('\t') {
            let (body, tab) = match piece.strip_suffix('\t') {
                Some(body) => (body, true),
                None => (piece, false),
            };
            if !body.is_empty() {
                *column += body.chars().count();
                push_body(&mut spans, body, style, theme, marks);
            }
            if tab {
                let pad = TAB - *column % TAB;
                *column += pad;
                // The arrow takes the first cell of the stop; the rest is the
                // blank the tab was always drawn as.
                if marks {
                    spans.push(Span::styled(TAB_MARK, theme.marks));
                    spans.push(Span::styled(" ".repeat(pad - 1), style));
                } else {
                    spans.push(Span::styled(" ".repeat(pad), style));
                }
            }
        }
    };

    for run in &text.runs[first.min(text.runs.len())..] {
        if run.start >= line.end {
            break;
        }
        let from = run.start.max(line.start);
        let to = run.end.min(line.end);
        if let Some(slice) = text.content.get(from..to) {
            push(slice, style_of(&run.style, theme), &mut column);
            at = to;
        }
    }

    // Anything the runs did not cover, which is the whole line when there is
    // no coloring at all.
    if at < line.end {
        if let Some(slice) = text.content.get(at..line.end) {
            push(slice, theme.code, &mut column);
        }
    }

    // The ending, read back off the content: `line_ranges` stops before it.
    if marks {
        let rest = text.content.as_bytes().get(line.end..).unwrap_or_default();
        if rest.first() == Some(&b'\r') {
            spans.push(Span::styled(CR, theme.marks));
        }
        if !rest.is_empty() {
            spans.push(Span::styled(LF, theme.marks));
        }
    }

    Line::from(spans)
}

/// Splits ordinary text away from the spaces in it, so the spaces can be
/// drawn as something you can see.
fn push_body<'a>(
    spans: &mut Vec<Span<'a>>,
    body: &'a str,
    style: Style,
    theme: &Theme,
    marks: bool,
) {
    if !marks || !body.contains(' ') {
        spans.push(Span::styled(body, style));
        return;
    }
    // One span per run of spaces, not per space: an indented line is a couple
    // of spans rather than one per column.
    let mut rest = body;
    while !rest.is_empty() {
        let spaces = rest.starts_with(' ');
        let end = rest
            .find(|c: char| (c == ' ') != spaces)
            .unwrap_or(rest.len());
        let (piece, tail) = rest.split_at(end);
        if spaces {
            spans.push(Span::styled(SPACE.repeat(piece.len()), theme.marks));
        } else {
            spans.push(Span::styled(piece, style));
        }
        rest = tail;
    }
}

/// The cells the fold control occupies, which are also the cells a press on it
/// lands in.
///
/// One function for both, so the marker cannot drift from what clicking it
/// does — the same reason `render.rs` and `hit.rs` share their indents.
pub fn fold_zone(area: Rect, framed: bool) -> Rect {
    // Framed, the header is written over the top border, one column in.
    let x = if framed {
        area.x.saturating_add(1)
    } else {
        area.x
    };
    Rect {
        x,
        y: area.y,
        width: FOLD.chars().count() as u16,
        height: 1,
    }
    .intersection(area)
}

/// Wide enough to hit without aiming. The arrow points where the tree is, or
/// where it would come back to.
const FOLD: &str = " « ";
const UNFOLD: &str = " » ";

/// How the pane is drawn right now — the parts that are a reader's choice,
/// plus the two the layout decides.
pub struct Look {
    pub numbers: bool,
    pub marks: bool,
    /// There is a tree beside this, so the pane has a border to separate them.
    pub framed: bool,
    /// git would ignore this file, so its name is dimmed like its row.
    pub ignored: bool,
}

pub fn draw(frame: &mut Frame, area: Rect, viewer: &Viewer, theme: &Theme, look: &Look) {
    let Look {
        numbers,
        marks,
        framed,
        ignored,
    } = *look;
    let inner = if framed {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.guide);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    } else {
        area
    };

    // The header: the fold control and the file's name. Framed, it is written
    // over the top border where a title would go; folded, it is the one line
    // above the file — and the only thing between the reader and a selection
    // that starts at column one.
    let zone = fold_zone(area, framed);
    let header = Rect {
        x: zone.x,
        y: zone.y,
        width: area
            .width
            .saturating_sub(zone.x - area.x + u16::from(framed)),
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(if framed { FOLD } else { UNFOLD }, theme.status),
            // Whatever this file's name is wearing in the tree, dimmed
            // included. `reset()` first because this is written over the
            // border: ratatui patches cell styles rather than replacing them,
            // so a style that sets no foreground would keep the border's.
            Span::styled(
                viewer.name.clone(),
                Style::reset().patch(if ignored { theme.ignored } else { theme.file }),
            ),
            // Size and line count live here rather than in the footer: the
            // file is always on display, so its measurements belong with it.
            Span::styled(format!("{} ", viewer.detail()), theme.status),
        ])),
        header,
    );
    let inner = if framed {
        inner
    } else {
        Rect {
            y: inner.y.saturating_add(1),
            height: inner.height.saturating_sub(1),
            ..inner
        }
    };

    let text = match &viewer.body {
        Body::Loading => {
            frame.render_widget(
                Paragraph::new(Line::styled("reading…", theme.status)),
                inner,
            );
            return;
        }
        Body::Notice(reason) => {
            frame.render_widget(Paragraph::new(reason.as_str()).style(theme.status), inner);
            return;
        }
        Body::Text(text) => text,
    };

    let height = inner.height as usize;
    let visible = text
        .lines
        .iter()
        .enumerate()
        .skip(viewer.offset)
        .take(height);

    // One string for the whole gutter: a 30,000-line file is two widgets
    // rather than 30,000, which is the same reason the browser builds its
    // gutter the way it does.
    //
    // Hidden, it takes no columns at all: an ordinary terminal selection runs
    // from one line into the next, so numbers left on screen would be dragged
    // in along with the code.
    let width = if numbers {
        digits(text.lines.len()) + 1
    } else {
        0
    };
    let mut column = String::new();
    let mut body: Vec<Line> = Vec::new();
    for (i, line) in visible {
        if numbers {
            column.push_str(&format!("{:>1$}\n", i + 1, width - 1));
        }
        body.push(line_spans(text, line, theme, marks));
    }

    let [gutter, code] =
        Layout::horizontal([Constraint::Length(width as u16), Constraint::Min(1)]).areas(inner);

    frame.render_widget(Paragraph::new(column).style(theme.status), gutter);
    frame.render_widget(Paragraph::new(body), code);
}

fn digits(n: usize) -> usize {
    n.max(1).ilog10() as usize + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::{Color as HColor, Style as HStyle};
    use crate::preview::Stamp;

    fn text(content: &str, runs: Vec<Run>) -> Text {
        Text {
            lines: line_ranges(content),
            content: content.into(),
            runs,
            size: content.len() as u64,
            lossy: false,
        }
    }

    #[test]
    fn a_trailing_newline_is_not_another_line() {
        assert_eq!(line_ranges("a\nb\n").len(), 2);
        assert_eq!(line_ranges("a\nb").len(), 2);
        assert_eq!(line_ranges("").len(), 0);
        assert_eq!(line_ranges("\n").len(), 1);
    }

    #[test]
    fn crlf_does_not_leave_a_carriage_return_in_the_line() {
        let ranges = line_ranges("a\r\nb\r\n");
        assert_eq!(ranges, vec![0..1, 3..4]);
    }

    #[test]
    fn coloring_is_sliced_per_line() {
        let content = "fn a\nfn b\n";
        let blue = HStyle {
            fg: Some(HColor::Rgb(0, 0, 255)),
            ..HStyle::default()
        };
        // One run spanning both lines, as a multi-line string literal would.
        let text = text(
            content,
            vec![Run {
                start: 0,
                end: 10,
                style: blue,
            }],
        );
        let theme = Theme::default();

        let second = line_spans(&text, &text.lines[1], &theme, false);
        assert_eq!(second.spans.len(), 1);
        assert_eq!(second.spans[0].content, "fn b");
        assert_eq!(second.spans[0].style.fg, Some(Color::Rgb(0, 0, 255)));
    }

    #[test]
    fn text_the_runs_do_not_cover_is_still_drawn() {
        // No coloring at all: bat was missing, or the file was too large.
        let text = text("hello\n", Vec::new());
        let line = line_spans(&text, &text.lines[0], &Theme::default(), false);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "hello");
    }

    #[test]
    fn tabs_are_drawn_to_the_next_stop() {
        let text = text("\tx\ty\n", Vec::new());
        let line = line_spans(&text, &text.lines[0], &Theme::default(), false);
        let drawn: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(drawn, "    x   y", "a raw tab draws as nothing in a cell");
    }

    /// What the marks are for: a tab and a space look identical without them,
    /// and a stray carriage return looks like nothing at all.
    #[test]
    fn the_invisible_characters_can_be_made_visible() {
        let text = text("\ta b\r\n  c\n", Vec::new());
        let drawn = |i: usize| -> String {
            line_spans(&text, &text.lines[i], &Theme::default(), true)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };
        // The arrow takes the first cell of the stop and the rest stays blank,
        // so nothing moves when the marks are turned on.
        assert_eq!(drawn(0), "→   a·b␍␊");
        assert_eq!(drawn(1), "··c␊");

        let plain: String = line_spans(&text, &text.lines[0], &Theme::default(), false)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(plain.chars().count(), drawn(0).chars().count() - 2);
    }

    #[test]
    fn a_last_line_with_no_newline_is_not_given_one() {
        let text = text("a\nb", Vec::new());
        let last: String = line_spans(&text, &text.lines[1], &Theme::default(), true)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            last, "b",
            "the file does not end in a newline, so neither does this"
        );
    }

    #[test]
    fn scrolling_stops_at_the_last_screenful() {
        let mut viewer = Viewer::loading(Arc::from(Path::new("/root/a.txt")));
        viewer.fill(
            Preview::Ok {
                stamp: Stamp { size: 6, mtime: 0 },
                content: (1..=100).map(|i| format!("{i}\n")).collect(),
                lossy: false,
            },
            Vec::new(),
        );
        assert_eq!(viewer.line_count(), 100);

        viewer.scroll_to(1000, 10);
        assert_eq!(viewer.offset, 90, "the last line stays on screen");
        viewer.scroll_to(0, 10);
        assert_eq!(viewer.offset, 0);
    }

    #[test]
    fn a_file_shorter_than_the_pane_does_not_scroll() {
        let mut viewer = Viewer::loading(Arc::from(Path::new("/root/a.txt")));
        viewer.fill(
            Preview::Ok {
                stamp: Stamp { size: 2, mtime: 0 },
                content: "a\n".into(),
                lossy: false,
            },
            Vec::new(),
        );
        viewer.scroll_to(5, 20);
        assert_eq!(viewer.offset, 0);
    }

    #[test]
    fn gutter_width_follows_the_line_count() {
        assert_eq!(digits(0), 1);
        assert_eq!(digits(9), 1);
        assert_eq!(digits(10), 2);
        assert_eq!(digits(1000), 4);
    }
}
