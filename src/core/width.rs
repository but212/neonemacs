//! Display width of a unit (§11, R2). Overrides `unicode-width` only for
//! tabs, control characters, C1 controls and zero-width joiners and fillers.

use crate::core::text::Unit;
use unicode_width::UnicodeWidthChar;

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
    }
}
