//! The file pane: what `viewing` actually shows.
//!
//! The pane holds its own copy of the file because the session does not: the
//! shared state says *which* file is open, and each view reads it for itself.

use std::borrow::Cow;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthChar;

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
    /// Screen rows of that line already scrolled past, when it wraps. Never
    /// shared: the session's position is a line, and every view can find one.
    pub row: usize,
    wrap: Option<Wrap>,
    /// The session's `edited` this copy was read for.
    pub edited: u64,
}

/// How many screen rows each line takes, for the width it was measured at.
struct Wrap {
    width: usize,
    marks: bool,
    rows: Vec<usize>,
}

impl Wrap {
    fn total(&self) -> usize {
        self.rows.iter().sum()
    }

    fn at(&self, line: usize, row: usize) -> usize {
        self.rows[..line.min(self.rows.len())].iter().sum::<usize>() + row
    }

    /// The line and row that screen row `at` falls in.
    fn locate(&self, mut at: usize) -> (usize, usize) {
        for (line, &rows) in self.rows.iter().enumerate() {
            if at < rows {
                return (line, at);
            }
            at -= rows;
        }
        (self.rows.len().saturating_sub(1), 0)
    }
}

pub enum Body {
    Loading,
    Text(Text),
    Picture(Picture),
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

/// An image, drawn by `chafa` for one size of pane and redrawn when the pane
/// is not that size any more.
pub struct Picture {
    size: u64,
    mime: &'static str,
    /// The cells it was last asked to fit, whether or not that has come back.
    fit: Option<(u16, u16)>,
    art: Art,
    /// Its own width and height in pixels, which is what the zoom limits are
    /// worked out from. Unknown until the first drawing comes back.
    pixels: Option<(u32, u32)>,
    /// Percent of the fitted size.
    zoom: usize,
    /// The cell of the drawing at the pane's top left, once it outgrows the
    /// pane. Kept unclamped while a new zoom is being drawn: clamping it to
    /// the old drawing would lose where the zoom was centered.
    top: usize,
    left: usize,
}

enum Art {
    Waiting,
    Drawn { text: Text, width: usize },
    Failed,
}

/// The zoom steps, as percentages of fitted. Which of them a picture gets is
/// worked out from its size by `Picture::limits`.
const ZOOMS: [usize; 30] = [
    5, 10, 15, 20, 25, 30, 40, 50, 60, 75, 100, 125, 150, 200, 250, 300, 400, 500, 600, 800, 1000,
    1200, 1600, 2000, 2500, 3200, 4000, 5000, 6400, 8000,
];
const FIT: usize = 100;
/// Zoomed out, the picture stays at least this many columns or rows.
const MIN_EXTENT: (f64, f64) = (8.0, 4.0);
/// A drawing of a million cells is ~250 ms of chafa and 8 MB of output; six
/// million was 1.6 s and 47 MB.
const MAX_CELLS: f64 = 1_000_000.0;

impl Picture {
    /// Rows and columns of what is drawn, which is none while waiting.
    fn extent(&self) -> (usize, usize) {
        match &self.art {
            Art::Drawn { text, width } => (text.lines.len(), *width),
            _ => (0, 0),
        }
    }

    /// The smallest and largest zoom for this picture in `view`, both steps
    /// of `ZOOMS`. In: as far as the picture's own pixels, one per half cell —
    /// past that chafa has no more detail to draw — or `MAX_CELLS`. Out: until
    /// it would be smaller than `MIN_EXTENT`.
    pub fn limits(&self, view: Rect) -> (usize, usize) {
        let Some((width, height)) = self.pixels else {
            return (25, 400);
        };
        // A cell is one pixel wide and two tall.
        let (native_cols, native_rows) = (width as f64, height as f64 / 2.0);
        let fit = (view.width as f64 / native_cols).min(view.height as f64 / native_rows);
        let (cols, rows) = (native_cols * fit, native_rows * fit);

        let native = 100.0 / fit;
        let biggest = 100.0 * (MAX_CELLS / (cols * rows).max(1.0)).sqrt();
        let max = ZOOMS
            .iter()
            .copied()
            .find(|&z| z as f64 >= native)
            .unwrap_or(ZOOMS[ZOOMS.len() - 1]);
        let max = ZOOMS
            .iter()
            .copied()
            .rev()
            .find(|&z| z <= max && z as f64 <= biggest)
            .unwrap_or(FIT)
            .max(FIT);

        let smallest = 100.0 * (MIN_EXTENT.0 / cols).max(MIN_EXTENT.1 / rows);
        let min = ZOOMS
            .iter()
            .copied()
            .find(|&z| z as f64 >= smallest)
            .unwrap_or(FIT)
            .min(FIT);
        (min, max)
    }

    /// `step` zooms that many steps in or out; zero goes back to fitted. The
    /// middle of what `view` shows stays in the middle.
    pub fn zoom(&mut self, step: i32, view: Rect) {
        let (min, max) = self.limits(view);
        let mut to = self.zoom;
        if step == 0 {
            to = FIT;
        }
        for _ in 0..step.unsigned_abs() {
            let next = if step > 0 {
                ZOOMS.iter().copied().find(|&z| z > to)
            } else {
                ZOOMS.iter().copied().rev().find(|&z| z < to)
            };
            to = next.unwrap_or(to);
        }
        let to = to.clamp(min, max);
        if to == self.zoom {
            return;
        }
        let (rows, cols) = self.extent();
        let keep = |at: usize, shown: u16, whole: usize| {
            let shown = shown as usize;
            let middle = match whole.checked_sub(shown) {
                Some(last) if last > 0 => at.min(last) + shown / 2,
                _ => whole / 2,
            };
            (middle * to / self.zoom).saturating_sub(shown / 2)
        };
        self.top = keep(self.top, view.height, rows);
        self.left = keep(self.left, view.width, cols);
        self.zoom = to;
    }

    /// Moves what `view` shows, as far as the drawing goes.
    pub fn pan(&mut self, right: i64, down: i64, view: Rect) {
        let (rows, cols) = self.extent();
        let shift = |at: usize, by: i64, shown: u16, whole: usize| {
            let last = whole.saturating_sub(shown as usize);
            (at.min(last) as i64 + by).clamp(0, last as i64) as usize
        };
        self.top = shift(self.top, down, view.height, rows);
        self.left = shift(self.left, right, view.width, cols);
    }

    /// The row at the top, and how many there are, for the motions text
    /// scrolls by.
    pub fn rows(&self, view: Rect) -> (usize, usize) {
        let rows = self.extent().0;
        (
            self.top.min(rows.saturating_sub(view.height as usize)),
            rows,
        )
    }
}

impl Viewer {
    pub fn loading(path: Arc<Path>, edited: u64) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            path,
            name,
            body: Body::Loading,
            offset: 0,
            row: 0,
            wrap: None,
            edited,
        }
    }

    /// Takes the result of a read, which arrived some time after the pane was
    /// opened.
    pub fn fill(&mut self, preview: Preview, runs: Vec<Run>) {
        self.offset = 0;
        self.row = 0;
        self.wrap = None;
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
            Preview::Media { stamp, mime }
                if mime.starts_with("image/") && super::picture::available() =>
            {
                Body::Picture(Picture {
                    size: stamp.size,
                    mime,
                    fit: None,
                    art: Art::Waiting,
                    pixels: None,
                    zoom: FIT,
                    top: 0,
                    left: 0,
                })
            }
            Preview::Media { stamp, mime } if mime.starts_with("image/") => Body::Notice(format!(
                "{mime}, {}\ninstall chafa to see it here",
                bytes(stamp.size)
            )),
            Preview::Media { stamp, mime } => Body::Notice(format!(
                "{mime}, {}\nthe terminal cannot play it; the browser view can try",
                bytes(stamp.size)
            )),
            Preview::Binary { size } => Body::Notice(format!("binary file, {}", bytes(size))),
            Preview::Unreadable { reason } => Body::Notice(reason),
            // Only sent to a caller that said what it already held, which this
            // one never does.
            Preview::Unchanged => Body::Notice("unchanged".into()),
        };
    }

    /// The size the picture needs drawing at, when it has not been asked for
    /// at this size yet. Asking is then the caller's job.
    pub fn refit(&mut self, area: Rect) -> Option<(u16, u16)> {
        let Body::Picture(picture) = &mut self.body else {
            return None;
        };
        // A pane that shrank can leave the zoom past what it now allows.
        let (min, max) = picture.limits(area);
        picture.zoom = picture.zoom.clamp(min, max);
        let scale =
            |cells: u16| (cells as usize * picture.zoom / 100).clamp(1, u16::MAX as usize) as u16;
        let fit = (scale(area.width), scale(area.height));
        if picture.fit == Some(fit) || fit.0 == 0 || fit.1 == 0 {
            return None;
        }
        picture.fit = Some(fit);
        Some(fit)
    }

    pub fn picture_mut(&mut self) -> Option<&mut Picture> {
        match &mut self.body {
            Body::Picture(picture) => Some(picture),
            _ => None,
        }
    }

    /// Takes what `chafa` drew. A drawing for a size the pane has since left
    /// is dropped; the one for the current size is already on its way.
    /// Until then the last drawing stays up, so a zoom does not flash empty.
    pub fn show(
        &mut self,
        fit: (u16, u16),
        pixels: Option<(u32, u32)>,
        drawn: Option<(String, Vec<Run>)>,
    ) {
        let Body::Picture(picture) = &mut self.body else {
            return;
        };
        picture.pixels = pixels.or(picture.pixels);
        if picture.fit != Some(fit) {
            return;
        }
        picture.art = match drawn {
            Some((content, runs)) => {
                let lines = line_ranges(&content);
                let width = lines
                    .iter()
                    .map(|line| content[line.clone()].chars().count())
                    .max()
                    .unwrap_or(0);
                Art::Drawn {
                    text: Text {
                        lines,
                        content,
                        runs,
                        size: picture.size,
                        lossy: false,
                    },
                    width,
                }
            }
            None => Art::Failed,
        };
    }

    pub fn line_count(&self) -> usize {
        match &self.body {
            Body::Text(text) => text.lines.len(),
            _ => 0,
        }
    }

    /// Wraps the text to `width` columns, or stops wrapping it with `None`.
    /// Measuring is a pass over the whole file, so it is redone only when the
    /// width or the marks — which take cells of their own — have changed.
    pub fn wrap_to(&mut self, width: Option<usize>, marks: bool, theme: &Theme) {
        let (Body::Text(text), Some(width)) = (&self.body, width.filter(|w| *w > 0)) else {
            self.wrap = None;
            self.row = 0;
            return;
        };
        if self
            .wrap
            .as_ref()
            .is_some_and(|w| w.width == width && w.marks == marks)
        {
            return;
        }
        let rows = text
            .lines
            .iter()
            .map(|line| breaks(line_spans(text, line, theme, marks), width).len())
            .collect();
        self.wrap = Some(Wrap { width, marks, rows });
        self.row = 0;
    }

    /// Where the top of the pane is and how far it could go, in screen rows,
    /// when the text is wrapped.
    pub fn wrapped(&self) -> Option<(usize, usize)> {
        let wrap = self.wrap.as_ref()?;
        Some((wrap.at(self.offset, self.row), wrap.total()))
    }

    /// Puts screen row `at` at the top of a wrapped pane, returning the line it
    /// lands in if that is a different one — which is all the session hears.
    pub fn scroll_rows(&mut self, at: usize) -> Option<usize> {
        let (line, row) = self.wrap.as_ref()?.locate(at);
        let moved = line != self.offset;
        self.offset = line;
        self.row = row;
        moved.then_some(line)
    }

    /// Puts `line` at the top, as far as a pane `viewport` lines tall can.
    /// A shorter pane than the one that chose the line simply sits at its own
    /// end rather than refusing to follow.
    pub fn scroll_to(&mut self, line: usize, viewport: usize) {
        let viewport = viewport.max(1);
        let Some(wrap) = &self.wrap else {
            let last = self.line_count().saturating_sub(viewport);
            self.offset = line.min(last);
            return;
        };
        if line != self.offset {
            self.offset = line;
            self.row = 0;
        }
        let last = wrap.total().saturating_sub(viewport);
        if wrap.at(self.offset, self.row) > last {
            (self.offset, self.row) = wrap.locate(last);
        }
    }

    /// What the header says about this file beyond its name — which the
    /// header is already showing beside this.
    pub fn detail(&self) -> String {
        match &self.body {
            Body::Loading => " · reading…".into(),
            // Whatever is wrong is written across the pane itself.
            Body::Notice(_) => String::new(),
            Body::Picture(picture) => format!(" · {} · image", bytes(picture.size)),
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
    let color = |color| match color {
        crate::highlight::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        crate::highlight::Color::Indexed(i) => Color::Indexed(i),
    };
    let mut out = theme.code;
    if let Some(fg) = style.fg {
        out = out.fg(color(fg));
    }
    if let Some(bg) = style.bg {
        out = out.bg(color(bg));
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

/// Cuts a line into rows of at most `width` cells. Anywhere, not at words:
/// this is code, and a column of it should stay a column.
fn breaks(line: Line<'_>, width: usize) -> Vec<Line<'_>> {
    let width = width.max(1);
    let mut rows = vec![Line::default()];
    let mut used = 0;
    for span in line.spans {
        let piece = |range: Range<usize>| match &span.content {
            Cow::Borrowed(s) => Cow::Borrowed(&s[range]),
            Cow::Owned(s) => Cow::Owned(s[range].to_string()),
        };
        let mut start = 0;
        for (i, ch) in span.content.char_indices() {
            let cells = ch.width().unwrap_or(0);
            if used + cells > width && used > 0 {
                if i > start {
                    rows.last_mut()
                        .unwrap()
                        .spans
                        .push(Span::styled(piece(start..i), span.style));
                }
                rows.push(Line::default());
                used = 0;
                start = i;
            }
            used += cells;
        }
        if start < span.content.len() {
            rows.last_mut()
                .unwrap()
                .spans
                .push(Span::styled(piece(start..span.content.len()), span.style));
        }
    }
    rows
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
    pub wrap: bool,
    /// There is a tree beside this, so the pane has a border to separate them.
    pub framed: bool,
    /// git would ignore this file, so its name is dimmed like its row.
    pub ignored: bool,
    /// The keyboard is here, so the border is lit and the tree's is not.
    pub focused: bool,
}

pub fn draw(frame: &mut Frame, area: Rect, viewer: &Viewer, theme: &Theme, look: &Look) {
    let Look {
        numbers,
        marks,
        wrap,
        framed,
        ignored,
        focused,
    } = *look;
    if framed {
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(if focused { theme.focused } else { theme.guide }),
            area,
        );
    }

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
    let inner = body(area, framed);

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
        Body::Picture(picture) => {
            draw_picture(frame, inner, picture, theme);
            if let Some(zones) = zoom_zones(area, framed, viewer) {
                draw_zoom(frame, zones, picture, theme);
            }
            return;
        }
        Body::Text(text) => text,
    };

    let height = inner.height as usize;

    // One string for the whole gutter: a 30,000-line file is two widgets
    // rather than 30,000, which is the same reason the browser builds its
    // gutter the way it does.
    //
    // Hidden, it takes no columns at all: an ordinary terminal selection runs
    // from one line into the next, so numbers left on screen would be dragged
    // in along with the code.
    let width = gutter_width(text, numbers);
    let mut column = String::new();
    let mut body: Vec<Line> = Vec::new();
    let mut skip = viewer.row;
    for (i, line) in text.lines.iter().enumerate().skip(viewer.offset) {
        let spans = line_spans(text, line, theme, marks);
        let rows = if wrap {
            breaks(spans, (inner.width as usize).saturating_sub(width))
        } else {
            vec![spans]
        };
        // A wrapped line is numbered once, on the row it starts on.
        for (n, row) in rows.into_iter().enumerate().skip(std::mem::take(&mut skip)) {
            if body.len() == height {
                break;
            }
            if numbers {
                match n {
                    0 => column.push_str(&format!("{:>1$}\n", i + 1, width - 1)),
                    _ => column.push('\n'),
                }
            }
            body.push(row);
        }
        if body.len() == height {
            break;
        }
    }

    let [gutter, code] =
        Layout::horizontal([Constraint::Length(width as u16), Constraint::Min(1)]).areas(inner);

    frame.render_widget(Paragraph::new(column).style(theme.status), gutter);
    frame.render_widget(Paragraph::new(body), code);
}

fn gutter_width(text: &Text, numbers: bool) -> usize {
    if numbers {
        digits(text.lines.len()) + 1
    } else {
        0
    }
}

/// The columns left for the text itself in a pane body `width` wide.
pub fn text_width(viewer: &Viewer, width: u16, numbers: bool) -> usize {
    match &viewer.body {
        Body::Text(text) => (width as usize).saturating_sub(gutter_width(text, numbers)),
        _ => 0,
    }
}

/// Where the file itself goes: inside the frame, or under the header line when
/// there is no frame for the header to sit on.
pub fn body(area: Rect, framed: bool) -> Rect {
    if framed {
        Block::default().borders(Borders::ALL).inner(area)
    } else {
        Rect {
            y: area.y.saturating_add(1),
            height: area.height.saturating_sub(1),
            ..area
        }
    }
}

fn draw_picture(frame: &mut Frame, area: Rect, picture: &Picture, theme: &Theme) {
    let text = match &picture.art {
        Art::Waiting => {
            frame.render_widget(Paragraph::new(Line::styled("drawing…", theme.status)), area);
            return;
        }
        Art::Failed => {
            frame.render_widget(
                Paragraph::new(format!("this chafa cannot draw {}", picture.mime))
                    .style(theme.status),
                area,
            );
            return;
        }
        Art::Drawn { text, .. } => text,
    };
    let (rows, cols) = picture.extent();
    // Smaller than the pane, the picture sits in the middle of it; larger, it
    // fills it and `top`/`left` say which part shows.
    let place = |start: u16, shown: u16, whole: usize, at: usize| {
        let shown = shown as usize;
        if whole <= shown {
            (start + ((shown - whole) / 2) as u16, whole, 0)
        } else {
            (start, shown, at.min(whole - shown))
        }
    };
    let (x, width, left) = place(area.x, area.width, cols, picture.left);
    let (y, height, top) = place(area.y, area.height, rows, picture.top);
    // Only the cells on screen: a zoomed drawing can be a million of them,
    // and this runs on every frame. Every cell is one character wide.
    let lines: Vec<Line> = text.lines[top..top + height]
        .iter()
        .map(|line| {
            let row = &text.content[line.clone()];
            let column =
                |n: usize| line.start + row.char_indices().nth(n).map_or(row.len(), |(i, _)| i);
            line_spans(text, &(column(left)..column(left + width)), theme, false)
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines),
        Rect {
            x,
            y,
            width: width as u16,
            height: height as u16,
        },
    );
}

/// Where the zoom controls go, as the minus button, the percentage — which
/// is a button back to fitted — the plus button, and the whole of them: on
/// the bottom border, or after the header when there is no border. One
/// function for drawing and clicking, as with `fold_zone`.
pub fn zoom_zones(area: Rect, framed: bool, viewer: &Viewer) -> Option<[Rect; 4]> {
    let Body::Picture(picture) = &viewer.body else {
        return None;
    };
    let (x, y) = if framed {
        (area.x.saturating_add(1), area.bottom().saturating_sub(1))
    } else {
        let header = [UNFOLD, &viewer.name, &format!("{} ", viewer.detail())]
            .iter()
            .map(|s| Span::raw(*s).width() as u16)
            .sum::<u16>();
        (area.x.saturating_add(header), area.y)
    };
    let label = zoom_label(picture).len() as u16;
    let at = |dx: u16, width: u16| {
        Rect {
            x: x.saturating_add(dx),
            y,
            width,
            height: 1,
        }
        .intersection(area)
    };
    Some([at(0, 3), at(3, label), at(3 + label, 3), at(0, 6 + label)])
}

fn zoom_label(picture: &Picture) -> String {
    format!("{}%", picture.zoom)
}

fn draw_zoom(frame: &mut Frame, zones: [Rect; 4], picture: &Picture, theme: &Theme) {
    // `reset()` because this is written over the border, like the header.
    let button = Style::reset().patch(theme.status_on);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" - ", button),
            Span::styled(zoom_label(picture), Style::reset().patch(theme.status)),
            Span::styled(" + ", button),
        ])),
        zones[3],
    );
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

    fn wrapped(content: &str, width: usize) -> Viewer {
        let mut viewer = Viewer::loading(Arc::from(Path::new("/root/a.txt")), 0);
        viewer.fill(
            Preview::Ok {
                content: content.into(),
                stamp: Stamp {
                    size: content.len() as u64,
                    mtime: 0,
                },
                lossy: false,
            },
            Vec::new(),
        );
        viewer.wrap_to(Some(width), false, &Theme::default());
        viewer
    }

    fn row_text(row: &Line) -> String {
        row.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_line_breaks_by_cells_and_a_wide_character_is_not_split() {
        let rows = breaks(Line::from(vec![Span::raw("abc"), Span::raw("defg")]), 3);
        let rows: Vec<String> = rows.iter().map(row_text).collect();
        assert_eq!(rows, ["abc", "def", "g"]);

        let rows = breaks(Line::raw("a漢字"), 4);
        let rows: Vec<String> = rows.iter().map(row_text).collect();
        assert_eq!(
            rows,
            ["a漢", "字"],
            "the second one does not fit in one cell"
        );

        assert_eq!(
            breaks(Line::default(), 5).len(),
            1,
            "an empty line is a row"
        );
    }

    #[test]
    fn wrapped_scrolling_moves_by_rows_and_reports_lines() {
        // Rows: 3 for the first line, 1 each after.
        let mut viewer = wrapped("aaaaaaaaa\nb\nc\nd\ne\n", 3);
        assert_eq!(viewer.wrapped(), Some((0, 7)));

        assert_eq!(viewer.scroll_rows(1), None, "still inside the first line");
        assert_eq!((viewer.offset, viewer.row), (0, 1));
        assert_eq!(viewer.scroll_rows(3), Some(1), "into the second");
        assert_eq!((viewer.offset, viewer.row), (1, 0));

        // The session saying the same line again keeps the row within it.
        viewer.scroll_rows(2);
        viewer.scroll_to(0, 2);
        assert_eq!((viewer.offset, viewer.row), (0, 2));

        // Past the last screenful it sits at the end, which is a row count
        // from the bottom rather than a line.
        viewer.scroll_to(4, 3);
        assert_eq!(viewer.wrapped(), Some((4, 7)));
        assert_eq!((viewer.offset, viewer.row), (2, 0));
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
        let mut viewer = Viewer::loading(Arc::from(Path::new("/root/a.txt")), 0);
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
        let mut viewer = Viewer::loading(Arc::from(Path::new("/root/a.txt")), 0);
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

    /// A picture already drawn, `cols` × `rows` cells, at `zoom` percent.
    fn picture(cols: usize, rows: usize, zoom: usize) -> Picture {
        let content: String = (0..rows).map(|_| "▀".repeat(cols) + "\n").collect();
        Picture {
            size: 0,
            fit: None,
            art: Art::Drawn {
                text: text(&content, Vec::new()),
                width: cols,
            },
            pixels: None,
            mime: "image/png",
            zoom,
            top: 0,
            left: 0,
        }
    }

    #[test]
    fn zooming_keeps_the_middle_in_the_middle() {
        let view = Rect::new(0, 0, 40, 10);
        // Fitted and narrower than the pane: its middle is its own.
        let mut p = picture(30, 10, FIT);
        p.zoom(3, view);
        assert_eq!(p.zoom, 200);
        assert_eq!((p.left, p.top), (10, 5), "column 15 of 30 becomes 30 of 60");

        // Zoomed and panned to the right edge: that edge stays in view.
        let mut p = picture(80, 20, 200);
        p.left = 40;
        p.zoom(0, view);
        assert_eq!(p.zoom, 100);
        assert_eq!(p.left, 10, "the middle column 60 of 80 becomes 30 of 40");
    }

    #[test]
    fn the_limits_come_from_the_picture_and_land_on_steps() {
        let view = Rect::new(0, 0, 80, 24);
        let mut p = picture(1, 1, FIT);

        // 160×90 is 160×45 cells at its own size; fitted it is 80 wide, so
        // 200% is every pixel.
        p.pixels = Some((160, 90));
        assert_eq!(p.limits(view), (20, 200));

        // Twice as large and it takes twice the zoom — rounded up to a step,
        // so the last one still shows every pixel.
        p.pixels = Some((330, 180));
        assert_eq!(p.limits(view).1, 500);

        // Huge: capped by how much there is to draw, not by its pixels.
        // Fitted it is 64×24 cells, and its own size would be 12500%.
        p.pixels = Some((8000, 6000));
        assert_eq!(p.limits(view).1, 2500);

        // An icon already fills the pane past its own size.
        p.pixels = Some((16, 16));
        assert_eq!(p.limits(view).1, FIT);
    }

    #[test]
    fn zoom_stops_at_the_limits_and_zero_fits() {
        let view = Rect::new(0, 0, 80, 24);
        let mut p = picture(80, 22, FIT);
        p.pixels = Some((160, 90));
        p.zoom(99, view);
        assert_eq!(p.zoom, 200);
        p.zoom(-99, view);
        assert_eq!(p.zoom, 20);
        p.zoom(0, view);
        assert_eq!(p.zoom, 100);
    }

    #[test]
    fn panning_stops_at_the_edges_of_the_drawing() {
        let view = Rect::new(0, 0, 40, 10);
        let mut p = picture(100, 30, 200);
        p.pan(1000, 1000, view);
        assert_eq!((p.left, p.top), (60, 20));
        p.pan(-1000, -1000, view);
        assert_eq!((p.left, p.top), (0, 0));

        let mut small = picture(20, 5, FIT);
        small.pan(10, 10, view);
        assert_eq!((small.left, small.top), (0, 0), "nothing to pan to");
    }

    #[test]
    fn the_zoom_buttons_sit_on_the_bottom_border_or_after_the_header() {
        let mut viewer = Viewer::loading(Arc::from(Path::new("/root/a.png")), 0);
        assert!(zoom_zones(Rect::new(20, 0, 40, 10), true, &viewer).is_none());
        viewer.body = Body::Picture(picture(10, 5, FIT));

        let [minus, fit, plus, _] = zoom_zones(Rect::new(20, 0, 40, 10), true, &viewer).unwrap();
        assert_eq!((minus.x, minus.y, minus.width), (21, 9, 3));
        assert_eq!((fit.x, fit.width), (24, 4), "\"100%\"");
        assert_eq!((plus.x, plus.y, plus.width), (28, 9, 3));

        // " » " + "a.png" + " · 0 B · image " is 3 + 5 + 15 columns.
        let [minus, ..] = zoom_zones(Rect::new(0, 0, 60, 10), false, &viewer).unwrap();
        assert_eq!((minus.x, minus.y), (23, 0));
    }

    #[test]
    fn gutter_width_follows_the_line_count() {
        assert_eq!(digits(0), 1);
        assert_eq!(digits(9), 1);
        assert_eq!(digits(10), 2);
        assert_eq!(digits(1000), 4);
    }
}
