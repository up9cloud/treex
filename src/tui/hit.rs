//! Mapping a mouse position onto the tree.

use ratatui::prelude::Rect;

use super::render::{INDENT, TWISTIE_WIDTH};
use crate::tree::Row;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// The ▸/▾ marker — always toggles, never just selects.
    Twistie(usize),
    Name(usize),
    /// Inside the tree pane but past the end of a row, or outside it entirely.
    Nothing,
}

pub fn hit(inner: Rect, offset: usize, rows: &[Row], col: u16, row: u16) -> Hit {
    if col < inner.x || row < inner.y || col >= inner.right() || row >= inner.bottom() {
        return Hit::Nothing;
    }
    let index = offset + (row - inner.y) as usize;
    let Some(entry) = rows.get(index) else {
        return Hit::Nothing;
    };

    let x = col - inner.x;
    let twistie_start = entry.depth as u16 * INDENT;
    if entry.is_dir() && x >= twistie_start && x < twistie_start + TWISTIE_WIDTH {
        return Hit::Twistie(index);
    }

    // Anywhere else on the line — the name, or the empty space past it — picks
    // the row, which is what every GUI file tree does and what a finger on a
    // phone will mostly produce. There is deliberately no width test: it would
    // have to measure display columns rather than characters, since a CJK
    // filename is twice as wide as its `chars().count()`.
    Hit::Name(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Kind;
    use std::path::PathBuf;

    fn row(depth: usize, name: &str, is_dir: bool) -> Row {
        Row {
            id: 0,
            depth,
            name: name.into(),
            path: std::sync::Arc::from(PathBuf::from(name)),
            kind: if is_dir { Kind::Dir } else { Kind::File },
            symlink: false,
            expanded: false,
            size: 0,
            last: true,
            omitted: 0,
        }
    }

    fn area() -> Rect {
        Rect::new(1, 1, 40, 10)
    }

    #[test]
    fn clicking_the_twistie_of_a_nested_dir() {
        let rows = vec![row(0, "root", true), row(1, "sub", true)];
        // depth 1 -> twistie occupies inner x 2..4, i.e. screen col 3..5
        assert_eq!(hit(area(), 0, &rows, 3, 2), Hit::Twistie(1));
        assert_eq!(hit(area(), 0, &rows, 4, 2), Hit::Twistie(1));
        assert_eq!(hit(area(), 0, &rows, 5, 2), Hit::Name(1));
    }

    #[test]
    fn files_have_no_twistie_to_click() {
        let rows = vec![row(0, "root", true), row(1, "a.txt", false)];
        assert_eq!(hit(area(), 0, &rows, 3, 2), Hit::Name(1));
    }

    #[test]
    fn scroll_offset_shifts_the_mapping() {
        let rows = vec![row(0, "root", true), row(1, "a", true), row(1, "b", true)];
        assert_eq!(hit(area(), 1, &rows, 3, 1), Hit::Twistie(1));
        assert_eq!(hit(area(), 1, &rows, 3, 2), Hit::Twistie(2));
    }

    #[test]
    fn a_wide_name_is_clickable_along_its_whole_width() {
        // Four CJK characters occupy eight columns, not four.
        let rows = vec![row(0, "root", true), row(1, "中文檔名", false)];
        for col in 5..13 {
            assert_eq!(hit(area(), 0, &rows, col, 2), Hit::Name(1), "column {col}");
        }
    }

    #[test]
    fn clicks_past_the_last_row_hit_nothing() {
        let rows = vec![row(0, "root", true)];
        assert_eq!(hit(area(), 0, &rows, 3, 5), Hit::Nothing);
        assert_eq!(hit(area(), 0, &rows, 0, 1), Hit::Nothing);
    }
}
