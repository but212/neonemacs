//! Display width of a unit (§11, R2). Overrides `unicode-width` only for
//! tabs, control characters, C1 controls and zero-width joiners and fillers.

use core::ops::ControlFlow;

use crate::{Doc, Unit, line_end, line_start};
use unicode_width::UnicodeWidthChar;

/// How wide the terminal draws things: the tab stop distance and whether East Asian ambiguous
/// characters take two columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    pub tab: u8,
    pub cjk: bool,
}

const _: () = assert!(core::mem::size_of::<Metrics>() <= 2);

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

/// Counts leading printable ASCII bytes (`0x20..=0x7E`), each occupying 1 byte and 1 column.
fn ascii_printable_run(bytes: &[u8]) -> usize {
    const HIGH: u64 = 0x8080_8080_8080_8080;
    const LOW_BIAS: u64 = 0x6060_6060_6060_6060;
    const DEL_BIAS: u64 = 0x0101_0101_0101_0101;

    let mut i = 0;
    let (chunks, _) = bytes.as_chunks::<8>();
    for chunk in chunks {
        let word = u64::from_le_bytes(*chunk);
        // Byte b is in 0x20..=0x7E iff (b & 0x80) == 0, ((b + 0x60) & 0x80) != 0, ((b + 0x01) & 0x80) == 0.
        let bad = (word & HIGH)
            | ((word.wrapping_add(LOW_BIAS) ^ HIGH) & HIGH)
            | (word.wrapping_add(DEL_BIAS) & HIGH);
        if bad != 0 {
            return i + (bad.trailing_zeros() as usize / 8);
        }
        i += 8;
    }
    while i < bytes.len() && (0x20..=0x7E).contains(&bytes[i]) {
        i += 1;
    }
    i
}

fn advance_width(bytes: &[u8], mut col: usize, m: Metrics) -> usize {
    let mut i = 0;
    while i < bytes.len() {
        let run = ascii_printable_run(&bytes[i..]);
        if run > 0 {
            col += run;
            i += run;
            continue;
        }
        let Some((unit, n)) = Unit::decode_prefix(&bytes[i..]) else {
            break;
        };
        col += unit_width(unit, col, m);
        i += n;
    }
    col
}

/// Display column of `pos` relative to the start of its line.
#[must_use]
pub fn column(text: &Doc, pos: usize, tab: Metrics) -> usize {
    let pos = text.floor_boundary(pos);
    let start = line_start(text, pos);
    let mut col = 0;
    let _ = text.try_for_each_slice(start..pos, |chunk| {
        col = advance_width(chunk, col, tab);
        ControlFlow::<()>::Continue(())
    });
    col
}

/// Position and exact display column on the line starting at `line` whose column is at most `col`.
#[must_use]
pub(crate) fn pos_and_column(text: &Doc, line: usize, col: usize, tab: Metrics) -> (usize, usize) {
    let line = text.floor_boundary(line);
    let end = line_end(text, line);
    let (mut p, mut c) = (line, 0);
    let _ = text.try_for_each_slice(line..end, |chunk| {
        let mut i = 0;
        while i < chunk.len() {
            if c < col {
                let max_ascii = (col - c).min(chunk.len() - i);
                let run = ascii_printable_run(&chunk[i..i + max_ascii]);
                if run > 0 {
                    p += run;
                    c += run;
                    i += run;
                    continue;
                }
            }
            let Some((unit, n)) = Unit::decode_prefix(&chunk[i..]) else {
                return ControlFlow::Break(());
            };
            let w = unit_width(unit, c, tab);
            if c + w > col {
                return ControlFlow::Break(());
            }
            c += w;
            p += n;
            i += n;
        }
        ControlFlow::Continue(())
    });
    (p, c)
}

/// Columns occupied by `unit` when it starts at column `col`.
#[must_use]
pub fn unit_width(unit: Unit, col: usize, m: Metrics) -> usize {
    match unit {
        Unit::Char('\t') => {
            let tab = usize::from(m.tab);
            tab - col % tab
        }
        _ => m.char(crate::unit_char(unit)),
    }
}

/// Display columns occupied by string `s`, starting at column 0.
#[must_use]
pub fn str_width(s: &str, m: Metrics) -> usize {
    let ascii = ascii_printable_run(s.as_bytes());
    let mut col = ascii;
    for ch in s[ascii..].chars() {
        col += unit_width(Unit::Char(ch), col, m);
    }
    col
}

#[cfg(test)]
mod tests {
    use alloc::vec;

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
        let t = Doc::from_bytes("\u{203B}x".as_bytes());
        assert_eq!(column(&t, 3, wide), 2);
        assert_eq!(column(&t, 3, narrow), 1);
        assert_eq!(pos_and_column(&t, 0, 2, wide).0, 3);
    }

    #[test]
    fn column_and_pos_at_column() {
        let t = Doc::from_bytes("a\t가b\nnext".as_bytes());
        assert_eq!(column(&t, 0, M8), 0);
        assert_eq!(column(&t, 1, M8), 1);
        assert_eq!(column(&t, 2, M8), 8);
        assert_eq!(column(&t, 5, M8), 10);
        assert_eq!(pos_and_column(&t, 0, 9, M8).0, 2);
        assert_eq!(pos_and_column(&t, 0, 10, M8).0, 5);
    }

    #[test]
    fn ascii_printable_run_matches_scalar_for_all_bytes_and_lanes() {
        for offset in 0..16 {
            for b in 0..=u8::MAX {
                let mut buf = vec![b'a'; offset + 9];
                buf[offset] = b;
                let expected = if (0x20..=0x7E).contains(&b) {
                    buf.len()
                } else {
                    offset
                };
                assert_eq!(
                    ascii_printable_run(&buf),
                    expected,
                    "offset={offset}, byte={b:#04x}"
                );
            }
        }
    }
}
