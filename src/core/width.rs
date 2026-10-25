//! Display width of a unit (§11, R2). Overrides `unicode-width` only for
//! tabs, control characters, C1 controls and zero-width joiners and fillers.

use crate::core::text::{Text, Unit};
use unicode_width::UnicodeWidthChar;

/// Display column of `pos` relative to the start of its line.
pub fn column(text: &Text, pos: usize, tab: usize) -> usize {
    let mut col = 0;
    let mut p = text.line_start(pos);
    while p < pos {
        let Some((unit, n)) = text.unit_at(p) else {
            break;
        };
        col += unit_width(unit, col, tab);
        p += n;
    }
    col
}

/// Position on the line starting at `line` whose column is at most `col`.
pub fn pos_at_column(text: &Text, line: usize, col: usize, tab: usize) -> usize {
    let end = text.line_end(line);
    let (mut p, mut c) = (line, 0);
    while p < end {
        let Some((unit, n)) = text.unit_at(p) else {
            break;
        };
        let w = unit_width(unit, c, tab);
        if c + w > col {
            break;
        }
        c += w;
        p += n;
    }
    p
}

/// Columns occupied by `unit` when it starts at column `col`.
pub fn unit_width(unit: Unit, col: usize, tab_width: usize) -> usize {
    match unit {
        Unit::Char('\t') => tab_width - col % tab_width,
        Unit::Char(c) => char_width(c),
        // Raw bytes are Latin-1 characters for display (§8).
        Unit::Raw(b) => char_width(char::from(b)),
    }
}

pub fn char_width(c: char) -> usize {
    match c as u32 {
        0x00..=0x1F | 0x7F => 2,
        0x80..=0x9F => 3,
        0x200B..=0x200D | 0x1160..=0x11FF | 0xFE00..=0xFE0F => 0,
        _ => c.width().unwrap_or(0),
    }
}

/// Display columns occupied by string `s`, starting at column 0.
pub fn str_width(s: &str, tab_width: usize) -> usize {
    let mut col = 0;
    for c in s.chars() {
        col += unit_width(Unit::Char(c), col, tab_width);
    }
    col
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        assert_eq!(unit_width(Unit::Char('\t'), 3, 8), 5);
        assert_eq!(unit_width(Unit::Char('\t'), 8, 8), 8);
        assert_eq!(unit_width(Unit::Char('\u{1}'), 0, 8), 2);
        assert_eq!(unit_width(Unit::Char('\u{85}'), 0, 8), 3);
        assert_eq!(unit_width(Unit::Raw(0x85), 0, 8), 3);
        assert_eq!(unit_width(Unit::Raw(0xE9), 0, 8), 1);
        assert_eq!(unit_width(Unit::Char('가'), 0, 8), 2);
        assert_eq!(unit_width(Unit::Char('e'), 0, 8), 1);
        assert_eq!(unit_width(Unit::Char('\u{301}'), 0, 8), 0);
        // NFD 한: U+1112 U+1161 U+11AB
        assert_eq!(unit_width(Unit::Char('\u{1112}'), 0, 8), 2);
        assert_eq!(unit_width(Unit::Char('\u{1161}'), 0, 8), 0);
        assert_eq!(unit_width(Unit::Char('\u{11AB}'), 0, 8), 0);
        assert_eq!(unit_width(Unit::Char('\u{200D}'), 0, 8), 0);

        assert_eq!(str_width("hello", 8), 5);
        assert_eq!(str_width("가나", 8), 4);
        assert_eq!(str_width("\t", 8), 8);
        assert_eq!(str_width("a\tb", 8), 9);
    }

    #[test]
    fn column_and_pos_at_column() {
        let t = crate::core::text::Text::from_bytes("a\t가b\nnext".as_bytes());
        assert_eq!(column(&t, 0, 8), 0);
        assert_eq!(column(&t, 1, 8), 1);
        assert_eq!(column(&t, 2, 8), 8);
        assert_eq!(column(&t, 5, 8), 10);
        assert_eq!(pos_at_column(&t, 0, 9, 8), 2);
        assert_eq!(pos_at_column(&t, 0, 10, 8), 5);
    }
}
