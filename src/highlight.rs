//! Syntax coloring, by asking `bat` for it.
//!
//! treex links no highlighting engine. It runs whatever `bat` is already on
//! the machine, feeds the file on stdin and reads the colors back out of the
//! ANSI it prints — so the user's own `bat` theme, syntax mappings and config
//! are what both views show, and a build without `bat` simply shows plain text.
//!
//! The output is checked against the text treex already read: `paint` returns
//! `None` unless what `bat` printed is the same bytes. Anything else —
//! a `bat` that expands tabs, a config that turns on line numbers, a different
//! program called `bat` — falls back to no coloring rather than coloring the
//! wrong characters.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::LazyLock;

/// A foreground color as the terminal expresses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    Rgb(u8, u8, u8),
    /// One of the terminal's own 256 colors.
    Indexed(u8),
}

impl Color {
    /// The color as 24-bit RGB, resolving an indexed color through the xterm
    /// palette. The browser has no notion of "color 12", so something has to.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(i) => indexed_rgb(i),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Option<Color>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

impl Style {
    pub fn is_plain(&self) -> bool {
        *self == Style::default()
    }
}

/// A styled stretch of the file, as a byte range into the content painted.
///
/// Ranges rather than copies: the caller already holds the text, and a second
/// copy of a megabyte to say what color it is would be the largest allocation
/// in the program.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

/// Files above this are shown without coloring.
///
/// Measured with bat 0.24 on this machine: 64 KiB takes 61 ms, 256 KiB 203 ms,
/// 1 MiB 804 ms — and the browser has to build a DOM node per run, of which a
/// megabyte of Rust produces about 240,000. The preview limit is 1 MiB, so a
/// file between the two is read but not colored.
pub const MAX_BYTES: usize = 256 * 1024;

/// The `bat` to use, looked for once.
///
/// Debian ships it as `batcat` because another package owns the name `bat`,
/// which is also why the version output is checked rather than trusted: the
/// `bat` first on someone's `PATH` may be a different program entirely.
static BAT: LazyLock<Option<&'static str>> = LazyLock::new(|| {
    ["bat", "batcat"].into_iter().find(|name| {
        Command::new(name)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .map(|out| out.status.success() && out.stdout.starts_with(b"bat "))
            .unwrap_or(false)
    })
});

/// Whether coloring is possible at all here, i.e. whether `bat` was found.
pub fn available() -> bool {
    BAT.is_some()
}

/// Colors `content`, which is the contents of a file called `name`.
///
/// `name` is what `bat` is told the file is called, so its own extension
/// rules, `--map-syntax` entries and first-line detection all apply. Returns
/// `None` when there is no `bat`, when the file is too large to be worth it,
/// or when `bat` printed anything but the text it was given.
pub fn paint(name: &str, content: &str) -> Option<Vec<Run>> {
    let bat = (*BAT)?;
    if content.is_empty() || content.len() > MAX_BYTES {
        return None;
    }
    parse(&run_bat(bat, name, content).ok()?, content)
}

fn run_bat(bat: &str, name: &str, content: &str) -> std::io::Result<Vec<u8>> {
    let mut child = Command::new(bat)
        .args([
            "--color=always",
            "--style=plain",
            "--paging=never",
            "--wrap=never",
            // Tabs must reach us as tabs: expanding them would make bat's
            // output a different string from the file, and every run would
            // then be checked against the wrong offset.
            "--tabs=0",
            "--file-name",
        ])
        .arg(name)
        // Ask for 24-bit color so the browser gets exact values rather than
        // palette indexes it would have to guess at.
        .env("COLORTERM", "truecolor")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let mut stdin = child.stdin.take().expect("stdin was piped");
    let mut stdout = child.stdout.take().expect("stdout was piped");

    // Written and read at the same time: a pipe holds 64 KiB, so writing a
    // megabyte before starting to read deadlocks both ends.
    let mut out = Vec::new();
    let read = std::thread::scope(|scope| {
        scope.spawn(move || {
            let _ = stdin.write_all(content.as_bytes());
        });
        stdout.read_to_end(&mut out)
    });
    read?;

    match child.wait()?.success() {
        true => Ok(out),
        false => Err(std::io::Error::other(format!("{bat} failed"))),
    }
}

const ESC: u8 = 0x1b;

/// Turns `bat`'s output into runs over `content`, or `None` if the two are not
/// the same text.
fn parse(out: &[u8], content: &str) -> Option<Vec<Run>> {
    let body = content.as_bytes();
    let mut runs: Vec<Run> = Vec::new();
    let mut style = Style::default();
    let mut pos = 0;
    let mut i = 0;

    while i < out.len() {
        if out[i] == ESC {
            i = escape(out, i, &mut style)?;
            continue;
        }
        let start = i;
        while i < out.len() && out[i] != ESC {
            i += 1;
        }
        let chunk = &out[start..i];
        if !body.get(pos..)?.starts_with(chunk) {
            return None;
        }
        push(&mut runs, pos, pos + chunk.len(), style);
        pos += chunk.len();
    }

    (pos == body.len()).then_some(runs)
}

/// Appends a run, extending the last one when the style has not changed.
/// `bat` resets and re-sets the color around every token, so without this a
/// line of ordinary punctuation is a dozen runs saying the same thing.
fn push(runs: &mut Vec<Run>, start: usize, end: usize, style: Style) {
    if start == end {
        return;
    }
    match runs.last_mut() {
        Some(last) if last.end == start && last.style == style => last.end = end,
        _ => runs.push(Run { start, end, style }),
    }
}

/// Consumes the escape sequence at `i`, applying it if it is an SGR one.
/// Returns the index just past it.
fn escape(out: &[u8], i: usize, style: &mut Style) -> Option<usize> {
    match out.get(i + 1)? {
        b'[' => {
            let mut j = i + 2;
            while j < out.len() && !(0x40..=0x7e).contains(&out[j]) {
                j += 1;
            }
            if *out.get(j)? == b'm' {
                sgr(&out[i + 2..j], style);
            }
            Some(j + 1)
        }
        // A hyperlink, which bat emits when its config asks for one. Runs to
        // BEL or to ESC \.
        b']' => {
            let mut j = i + 2;
            while j < out.len() {
                if out[j] == 0x07 {
                    return Some(j + 1);
                }
                if out[j] == ESC && out.get(j + 1) == Some(&b'\\') {
                    return Some(j + 2);
                }
                j += 1;
            }
            None
        }
        _ => Some(i + 2),
    }
}

fn sgr(params: &[u8], style: &mut Style) {
    let mut it = params.split(|&b| b == b';').map(|p| {
        std::str::from_utf8(p).ok().and_then(|s| {
            if s.is_empty() {
                Some(0)
            } else {
                s.parse::<u16>().ok()
            }
        })
    });

    while let Some(param) = it.next() {
        // An unparsable parameter means the rest cannot be read positionally
        // either, since 38 and 48 take their arguments from this same list.
        let Some(n) = param else { return };
        match n {
            0 => *style = Style::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            24 => style.underline = false,
            30..=37 => style.fg = Some(Color::Indexed((n - 30) as u8)),
            90..=97 => style.fg = Some(Color::Indexed((n - 90 + 8) as u8)),
            38 => style.fg = color(&mut it),
            39 => style.fg = None,
            48 => {
                // A background treex does not use, but whose parameters still
                // have to be taken off the list.
                let _ = color(&mut it);
            }
            _ => {}
        }
    }
}

fn color(it: &mut impl Iterator<Item = Option<u16>>) -> Option<Color> {
    match it.next()?? {
        5 => Some(Color::Indexed(it.next()?? as u8)),
        2 => Some(Color::Rgb(
            it.next()?? as u8,
            it.next()?? as u8,
            it.next()?? as u8,
        )),
        _ => None,
    }
}

/// The xterm 256-color palette. The first sixteen are a terminal's own and
/// only ever an approximation; the rest are defined exactly.
fn indexed_rgb(i: u8) -> (u8, u8, u8) {
    const BASIC: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    match i {
        0..=15 => BASIC[i as usize],
        16..=231 => {
            let i = i - 16;
            let level = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
            (level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Style {
        Style {
            fg: Some(Color::Rgb(r, g, b)),
            ..Style::default()
        }
    }

    /// What bat actually emits: a color, the token, a reset, repeatedly.
    #[test]
    fn parses_what_bat_prints() {
        let out = b"\x1b[38;2;102;217;239mfn\x1b[0m\x1b[38;2;248;248;242m main\x1b[0m\n";
        let runs = parse(out, "fn main\n").unwrap();
        assert_eq!(
            runs,
            vec![
                Run {
                    start: 0,
                    end: 2,
                    style: rgb(102, 217, 239)
                },
                Run {
                    start: 2,
                    end: 7,
                    style: rgb(248, 248, 242)
                },
                Run {
                    start: 7,
                    end: 8,
                    style: Style::default()
                },
            ]
        );
    }

    #[test]
    fn adjacent_runs_of_one_style_are_merged() {
        // Four separately colored tokens, all the same color.
        let out = b"\x1b[31ma\x1b[0m\x1b[31mb\x1b[0m\x1b[31mc\x1b[0m";
        let runs = parse(out, "abc").unwrap();
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert_eq!(runs[0].end, 3);
    }

    #[test]
    fn text_that_is_not_the_file_is_refused() {
        // A bat that expanded tabs, which would put every later run on the
        // wrong characters.
        assert_eq!(parse(b"\x1b[31m    x\x1b[0m", "\tx"), None);
        // Truncated output is not a coloring of the whole file either.
        assert_eq!(parse(b"\x1b[31mab\x1b[0m", "abc"), None);
        // Nor is output with something extra on the end.
        assert_eq!(parse(b"\x1b[31mabc\x1b[0m\n", "abc"), None);
    }

    #[test]
    fn attributes_accumulate_and_reset() {
        let runs = parse(b"\x1b[1m\x1b[3ma\x1b[23mb\x1b[0mc", "abc").unwrap();
        assert_eq!(
            runs.iter().map(|r| r.style).collect::<Vec<_>>(),
            vec![
                Style {
                    bold: true,
                    italic: true,
                    ..Style::default()
                },
                Style {
                    bold: true,
                    ..Style::default()
                },
                Style::default(),
            ]
        );
    }

    #[test]
    fn color_forms_all_parse() {
        let cases: &[(&[u8], Option<Color>)] = &[
            (b"\x1b[31ma", Some(Color::Indexed(1))),
            (b"\x1b[91ma", Some(Color::Indexed(9))),
            (b"\x1b[38;5;208ma", Some(Color::Indexed(208))),
            (b"\x1b[38;2;1;2;3ma", Some(Color::Rgb(1, 2, 3))),
            (b"\x1b[39ma", None),
            // A background is consumed without being mistaken for a color.
            (b"\x1b[48;2;9;9;9ma", None),
        ];
        for (out, want) in cases {
            let runs = parse(out, "a").unwrap();
            assert_eq!(
                runs[0].style.fg,
                *want,
                "{:?}",
                String::from_utf8_lossy(out)
            );
        }
    }

    #[test]
    fn sequences_that_are_not_colors_are_stepped_over() {
        // A cursor move, a hyperlink and a bare ESC \ terminator.
        let out = b"\x1b[2Ka\x1b]8;;http://x\x07b\x1b]8;;\x1b\\c";
        let runs = parse(out, "abc").unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].start, runs[0].end), (0, 3));
    }

    #[test]
    fn multibyte_text_keeps_its_own_boundaries() {
        let content = "中文 ok\n";
        let out = "\u{1b}[31m中文\u{1b}[0m ok\n".as_bytes();
        let runs = parse(out, content).unwrap();
        assert_eq!(runs[0].end, 6, "two CJK characters are six bytes");
        // Every boundary has to be sliceable, or the views panic on it.
        for run in &runs {
            assert!(content.get(run.start..run.end).is_some(), "{run:?}");
        }
    }

    #[test]
    fn an_empty_file_is_never_sent_to_bat() {
        assert_eq!(paint("a.rs", ""), None);
    }

    #[test]
    fn the_xterm_cube_and_ramp_are_right() {
        assert_eq!(indexed_rgb(16), (0, 0, 0));
        assert_eq!(indexed_rgb(231), (255, 255, 255));
        assert_eq!(indexed_rgb(196), (255, 0, 0));
        assert_eq!(indexed_rgb(232), (8, 8, 8));
        assert_eq!(indexed_rgb(255), (238, 238, 238));
    }

    /// Against the real thing, when the machine has one. CI without `bat`
    /// skips this; the parsing above is what has the real coverage.
    #[test]
    fn a_real_bat_colors_rust() {
        if !available() {
            return;
        }
        let content = "fn main() {\n    let x = 1;\n}\n";
        let runs = paint("main.rs", content).expect("bat is present and should color Rust");

        assert_eq!(runs.first().map(|r| r.start), Some(0));
        assert_eq!(runs.last().map(|r| r.end), Some(content.len()));
        assert!(
            runs.windows(2).all(|w| w[0].end == w[1].start),
            "runs must tile the file with no gaps"
        );
        assert!(
            runs.iter().any(|r| !r.style.is_plain()),
            "nothing was colored at all"
        );
    }
}
