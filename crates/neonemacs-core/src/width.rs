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

fn contiguous_slice(text: &Text, pos: usize, end: usize) -> &[u8] {
    let (left, right) = text.slices_in(pos..end);
    if left.is_empty() { right } else { left }
}

/// Counts leading printable ASCII bytes (`0x20..=0x7E`), each occupying 1 byte and 1 column.
fn ascii_printable_run(bytes: &[u8]) -> usize {
    const HIGH: u64 = 0x8080_8080_8080_8080;
    const LOW_BIAS: u64 = 0x6060_6060_6060_6060;
    const DEL_BIAS: u64 = 0x0101_0101_0101_0101;

    let mut i = 0;
    while i + 8 <= bytes.len() {
        let word = u64::from_ne_bytes([
            bytes[i],
            bytes[i + 1],
            bytes[i + 2],
            bytes[i + 3],
            bytes[i + 4],
            bytes[i + 5],
            bytes[i + 6],
            bytes[i + 7],
        ]);
        if (word & HIGH) == 0
            && (word.wrapping_add(LOW_BIAS) & HIGH) == HIGH
            && (word.wrapping_add(DEL_BIAS) & HIGH) == 0
        {
            i += 8;
        } else {
            break;
        }
    }
    while i < bytes.len() && (0x20..=0x7E).contains(&bytes[i]) {
        i += 1;
    }
    i
}

/// Display column of `pos` relative to the start of its line.
#[must_use]
pub fn column(text: &Text, pos: usize, tab: Metrics) -> usize {
    let pos = pos.min(text.len());
    let mut col = 0;
    let mut p = text.line_start(pos);
    while p < pos {
        let run = ascii_printable_run(contiguous_slice(text, p, pos));
        if run > 0 {
            p += run;
            col += run;
            continue;
        }
        let Some((unit, n)) = text.unit_at(p) else {
            break;
        };
        col += unit_width(unit, col, tab);
        p += n;
    }
    col
}

/// Position and exact display column on the line starting at `line` whose column is at most `col`.
#[must_use]
pub(crate) fn pos_and_column(text: &Text, line: usize, col: usize, tab: Metrics) -> (usize, usize) {
    let end = text.line_end(line);
    let (mut p, mut c) = (line, 0);
    while p < end {
        if c < col {
            let max_ascii = (col - c).min(end - p);
            let run = ascii_printable_run(contiguous_slice(text, p, p + max_ascii));
            if run > 0 {
                p += run;
                c += run;
                continue;
            }
        }
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
    (p, c)
}

/// Position on the line starting at `line` whose column is at most `col`.
#[must_use]
pub fn pos_at_column(text: &Text, line: usize, col: usize, tab: Metrics) -> usize {
    pos_and_column(text, line, col, tab).0
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
