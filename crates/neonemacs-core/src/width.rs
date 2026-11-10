//! Display width of a unit (§11, R2). Overrides `unicode-width` only for
//! tabs, control characters, C1 controls and zero-width joiners and fillers.

use crate::text::{Text, Unit};
use unicode_width::UnicodeWidthChar;

/// How wide the terminal draws things: the tab stop distance and whether East Asian ambiguous
/// characters take two columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    pub tab: usize,
    pub cjk: bool,
}

impl Metrics {
    /// Columns occupied by `c`.
    #[must_use]
    pub fn char(self, c: char) -> usize {
        match c as u32 {
            0x00..=0x1F | 0x7F => 2,
            0x80..=0x9F => 3,
            0x200B..=0x200D | 0x1160..=0x11FF | 0xFE00..=0xFE0F => 0,
            _ if self.cjk => c.width_cjk().unwrap_or(0),
            _ => c.width().unwrap_or(0),
        }
    }
}

/// Display column of `pos` relative to the start of its line.
#[must_use]
pub fn column(text: &Text, pos: usize, tab: Metrics) -> usize {
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
#[must_use]
pub fn pos_at_column(text: &Text, line: usize, col: usize, tab: Metrics) -> usize {
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
#[must_use]
pub fn unit_width(unit: Unit, col: usize, m: Metrics) -> usize {
    match unit {
        Unit::Char('\t') => m.tab - col % m.tab,
        Unit::Char(c) => m.char(c),
        // Raw bytes are Latin-1 characters for display (§8).
        Unit::Raw(b) => m.char(char::from(b)),
    }
}

/// Display columns occupied by string `s`, starting at column 0.
#[must_use]
pub fn str_width(s: &str, m: Metrics) -> usize {
    let mut col = 0;
    for c in s.chars() {
        col += unit_width(Unit::Char(c), col, m);
    }
    col
}

#[cfg(test)]
mod tests {
    use super::*;

    const M8: Metrics = Metrics { tab: 8, cjk: false };

    #[test]
    fn rules() {
        assert_eq!(unit_width(Unit::Char('\t'), 3, M8), 5);
        assert_eq!(unit_width(Unit::Char('\t'), 8, M8), 8);
        assert_eq!(unit_width(Unit::Char('\u{1}'), 0, M8), 2);
        assert_eq!(unit_width(Unit::Char('\u{85}'), 0, M8), 3);
        assert_eq!(unit_width(Unit::Raw(0x85), 0, M8), 3);
        assert_eq!(unit_width(Unit::Raw(0xE9), 0, M8), 1);
        assert_eq!(unit_width(Unit::Char('가'), 0, M8), 2);
        assert_eq!(unit_width(Unit::Char('e'), 0, M8), 1);
        assert_eq!(unit_width(Unit::Char('\u{301}'), 0, M8), 0);
        // NFD 한: U+1112 U+1161 U+11AB
        assert_eq!(unit_width(Unit::Char('\u{1112}'), 0, M8), 2);
        assert_eq!(unit_width(Unit::Char('\u{1161}'), 0, M8), 0);
        assert_eq!(unit_width(Unit::Char('\u{11AB}'), 0, M8), 0);
        assert_eq!(unit_width(Unit::Char('\u{200D}'), 0, M8), 0);

        assert_eq!(str_width("hello", M8), 5);
        assert_eq!(str_width("가나", M8), 4);
        assert_eq!(str_width("\t", M8), 8);
        assert_eq!(str_width("a\tb", M8), 9);
    }

    #[test]
    fn ambiguous_width_follows_the_metrics() {
        let narrow = Metrics { tab: 8, cjk: false };
        let wide = Metrics { tab: 8, cjk: true };
        assert_eq!(unit_width(Unit::Char('\u{203B}'), 0, narrow), 1);
        assert_eq!(unit_width(Unit::Char('\u{203B}'), 0, wide), 2);
        for m in [narrow, wide] {
            assert_eq!(unit_width(Unit::Char('a'), 0, m), 1);
            assert_eq!(unit_width(Unit::Char('가'), 0, m), 2);
            assert_eq!(unit_width(Unit::Char('\u{1}'), 0, m), 2);
            assert_eq!(unit_width(Unit::Char('\u{301}'), 0, m), 0);
        }
        assert_eq!(str_width("a\u{203B}", wide), 3);
        let t = crate::text::Text::from_bytes("\u{203B}x".as_bytes());
        assert_eq!(column(&t, 3, wide), 2);
        assert_eq!(column(&t, 3, narrow), 1);
        assert_eq!(pos_at_column(&t, 0, 2, wide), 3);
    }

    #[test]
    fn column_and_pos_at_column() {
        let t = crate::text::Text::from_bytes("a\t가b\nnext".as_bytes());
        assert_eq!(column(&t, 0, M8), 0);
        assert_eq!(column(&t, 1, M8), 1);
        assert_eq!(column(&t, 2, M8), 8);
        assert_eq!(column(&t, 5, M8), 10);
        assert_eq!(pos_at_column(&t, 0, 9, M8), 2);
        assert_eq!(pos_at_column(&t, 0, 10, M8), 5);
    }
}
