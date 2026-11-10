//! Gap buffer over raw bytes. Boundaries are local (I-02).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Char(char),
    Raw(u8),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Text {
    buf: Vec<u8>,
    gap_start: usize,
    gap_end: usize,
}

/// Moves an anchor that landed inside a unit back to that unit's start.
#[must_use]
pub fn snap_point(text: &Text, a: usize) -> usize {
    let a = a.min(text.len());
    if text.is_boundary(a) {
        a
    } else {
        text.prev_boundary((a + 1).min(text.len()))
    }
}

impl Text {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self::from_vec(bytes.to_vec())
    }

    #[must_use]
    pub const fn from_vec(buf: Vec<u8>) -> Self {
        let n = buf.len();
        Self {
            buf,
            gap_start: n,
            gap_end: n,
        }
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.buf.len() - (self.gap_end - self.gap_start)
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn byte_at(&self, pos: usize) -> u8 {
        if pos < self.gap_start {
            self.buf[pos]
        } else {
            self.buf[pos + self.gap_end - self.gap_start]
        }
    }

    #[must_use]
    pub fn bytes(&self, range: std::ops::Range<usize>) -> Vec<u8> {
        let len = self.len();
        let start = range.start.min(len);
        let end = range.end.min(len).max(start);
        let (left, right) = self.slices();
        let left_len = left.len();
        let mut out = Vec::with_capacity(end - start);
        if start < left_len {
            out.extend_from_slice(&left[start..end.min(left_len)]);
        }
        if end > left_len {
            out.extend_from_slice(&right[start.saturating_sub(left_len)..end - left_len]);
        }
        out
    }

    /// Returns the two contiguous slices before and after the gap.
    #[must_use]
    pub fn slices(&self) -> (&[u8], &[u8]) {
        (&self.buf[..self.gap_start], &self.buf[self.gap_end..])
    }

    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let (left, right) = self.slices();
        let mut out = Vec::with_capacity(left.len() + right.len());
        out.extend_from_slice(left);
        out.extend_from_slice(right);
        out
    }

    #[must_use]
    pub fn insert(self, pos: usize, bytes: &[u8]) -> Self {
        let pos = pos.min(self.len());
        let mut t = self.move_gap(pos).reserve_gap(bytes.len());
        t.buf[t.gap_start..t.gap_start + bytes.len()].copy_from_slice(bytes);
        t.gap_start += bytes.len();
        t
    }

    /// Removes `range`. Read the bytes first with `bytes` if they are needed.
    #[must_use]
    pub fn delete(self, range: std::ops::Range<usize>) -> Self {
        let len = self.len();
        let start = range.start.min(len);
        let end = range.end.min(len).max(start);
        let mut t = self.move_gap(start);
        t.gap_end += end - start;
        t
    }

    fn move_gap(mut self, pos: usize) -> Self {
        if pos < self.gap_start {
            let n = self.gap_start - pos;
            self.buf.copy_within(pos..self.gap_start, self.gap_end - n);
            self.gap_start = pos;
            self.gap_end -= n;
        } else if pos > self.gap_start {
            let n = pos - self.gap_start;
            self.buf
                .copy_within(self.gap_end..self.gap_end + n, self.gap_start);
            self.gap_start += n;
            self.gap_end += n;
        }
        self
    }

    fn reserve_gap(mut self, need: usize) -> Self {
        if self.gap_end - self.gap_start >= need {
            return self;
        }
        let grow = need + self.len().max(64) / 2;
        let tail = self.buf.split_off(self.gap_end);
        self.buf.truncate(self.gap_start);
        self.buf.resize(self.gap_start + grow, 0);
        self.buf.extend_from_slice(&tail);
        self.gap_end = self.gap_start + grow;
        self
    }

    fn decode_at(&self, pos: usize) -> (Unit, usize) {
        let lead = self.byte_at(pos);
        let (extra, bits) = match lead {
            0x00..=0x7F => return (Unit::Char(lead as char), 1),
            0xC0..=0xDF => (1, 0x1F),
            0xE0..=0xEF => (2, 0x0F),
            0xF0..=0xF7 => (3, 0x07),
            _ => return (Unit::Raw(lead), 1),
        };
        if pos + extra >= self.len() {
            return (Unit::Raw(lead), 1);
        }
        let mut cp = u32::from(lead & bits);
        for i in 1..=extra {
            let b = self.byte_at(pos + i);
            if b & 0xC0 != 0x80 {
                return (Unit::Raw(lead), 1);
            }
            cp = cp << 6 | u32::from(b & 0x3F);
        }
        char::from_u32(cp).map_or((Unit::Raw(lead), 1), |c| (Unit::Char(c), extra + 1))
    }

    /// Unit starting at `pos`, with its byte length. `None` at end.
    #[must_use]
    pub fn unit_at(&self, pos: usize) -> Option<(Unit, usize)> {
        (pos < self.len()).then(|| self.decode_at(pos))
    }

    /// True when `pos` is the start of a unit or the end of the text.
    #[must_use]
    pub fn is_boundary(&self, pos: usize) -> bool {
        let len = self.len();
        if pos > len {
            return false;
        }
        if pos == 0 || pos == len || self.byte_at(pos) & 0xC0 != 0x80 {
            return true;
        }
        let lo = pos.saturating_sub(3);
        !(lo..pos).any(|q| q + self.decode_at(q).1 > pos)
    }

    /// Largest boundary strictly before `pos`. `pos` must be in `1..=len`.
    #[must_use]
    pub fn prev_boundary(&self, pos: usize) -> usize {
        let mut q = pos - 1;
        while !self.is_boundary(q) {
            q -= 1;
        }
        q
    }

    /// Smallest boundary strictly after `pos`.
    #[must_use]
    pub fn next_boundary(&self, pos: usize) -> usize {
        let len = self.len();
        let mut q = pos + 1;
        while q < len && !self.is_boundary(q) {
            q += 1;
        }
        q.min(len)
    }

    /// Start of line `n` (1-based). Past the last line, the start of the last line.
    #[must_use]
    pub fn nth_line_start(&self, n: usize) -> usize {
        if n <= 1 {
            return 0;
        }
        let mut remaining = n - 1;
        let mut last_pos = 0;
        let (left, right) = self.slices();

        for offset in memchr::memchr_iter(b'\n', left) {
            remaining -= 1;
            last_pos = offset + 1;
            if remaining == 0 {
                return last_pos;
            }
        }

        for offset in memchr::memchr_iter(b'\n', right) {
            remaining -= 1;
            last_pos = left.len() + offset + 1;
            if remaining == 0 {
                return last_pos;
            }
        }

        last_pos
    }

    /// Counts newline characters strictly within `start..end`.
    #[must_use]
    pub fn count_lines(&self, start: usize, end: usize) -> usize {
        let len = self.len();
        let start = start.min(len);
        let end = end.min(len);
        if start >= end {
            return 0;
        }

        let (left, right) = self.slices();
        let left_len = left.len();

        let left_count = if start < left_len {
            let s = start;
            let e = end.min(left_len);
            memchr::memchr_iter(b'\n', &left[s..e]).count()
        } else {
            0
        };

        let right_count = if end > left_len {
            let s = start.saturating_sub(left_len);
            let e = end - left_len;
            memchr::memchr_iter(b'\n', &right[s..e]).count()
        } else {
            0
        };

        left_count + right_count
    }

    /// Start of the line containing pos.
    #[must_use]
    pub fn line_start(&self, pos: usize) -> usize {
        let pos = pos.min(self.len());
        let (left, right) = self.slices();
        let left_len = left.len();

        if pos <= left_len {
            memchr::memrchr(b'\n', &left[..pos]).map_or(0, |i| i + 1)
        } else {
            let right_offset = pos - left_len;
            memchr::memrchr(b'\n', &right[..right_offset]).map_or_else(
                || memchr::memrchr(b'\n', left).map_or(0, |i| i + 1),
                |i| left_len + i + 1,
            )
        }
    }

    /// Position of the \n ending the line containing pos, or the end of text.
    #[must_use]
    pub fn line_end(&self, pos: usize) -> usize {
        let len = self.len();
        let pos = pos.min(len);
        let (left, right) = self.slices();
        let left_len = left.len();

        if pos < left_len {
            memchr::memchr(b'\n', &left[pos..]).map_or_else(
                || memchr::memchr(b'\n', right).map_or(len, |i| left_len + i),
                |i| pos + i,
            )
        } else {
            let right_offset = pos - left_len;
            memchr::memchr(b'\n', &right[right_offset..]).map_or(len, |i| pos + i)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn starts(t: &Text) -> Vec<usize> {
        let mut v = Vec::new();
        let mut p = 0;
        while let Some((_, n)) = t.unit_at(p) {
            v.push(p);
            p += n;
        }
        v.push(t.len());
        v
    }

    #[test]
    fn decode_units() {
        let t = Text::from_bytes("가".as_bytes());
        assert_eq!(t.unit_at(0), Some((Unit::Char('가'), 3)));

        let t = Text::from_bytes(&[0xC0, 0xAF]);
        assert_eq!(t.unit_at(0), Some((Unit::Char('/'), 2)));

        let t = Text::from_bytes(&[0xE9]);
        assert_eq!(t.unit_at(0), Some((Unit::Raw(0xE9), 1)));

        let t = Text::from_bytes(&[0xE4, 0xB8]);
        assert_eq!(t.unit_at(0), Some((Unit::Raw(0xE4), 1)));
        assert_eq!(t.unit_at(1), Some((Unit::Raw(0xB8), 1)));
    }

    #[test]
    fn boundary_symmetry() {
        // a | 가 | E4 | B8 | b | C0 AF
        let t = Text::from_bytes(&[b'a', 0xEA, 0xB0, 0x80, 0xE4, 0xB8, b'b', 0xC0, 0xAF]);
        let s = starts(&t);
        assert_eq!(s, vec![0, 1, 4, 5, 6, 7, 9]);
        for p in 0..=t.len() {
            assert_eq!(t.is_boundary(p), s.contains(&p), "is_boundary({p})");
            if p > 0 {
                let prev = *s.iter().rev().find(|&&b| b < p).unwrap();
                assert_eq!(t.prev_boundary(p), prev, "prev_boundary({p})");
            }
            if p < t.len() {
                let next = *s.iter().find(|&&b| b > p).unwrap();
                assert_eq!(t.next_boundary(p), next, "next_boundary({p})");
            }
        }
    }

    #[test]
    fn gap_edits_round_trip() {
        let t = Text::from_bytes(b"hello")
            .insert(5, b" world")
            .insert(0, b">> ");
        assert_eq!(t.to_bytes(), b">> hello world");
        assert_eq!(t.bytes(3..9), b"hello ");
        let t = t.delete(3..9);
        assert_eq!(t.to_bytes(), b">> world");
        assert_eq!(t.byte_at(3), b'w');
    }

    #[test]
    fn line_helpers() {
        let t = Text::from_bytes(b"ab\ncd\n");
        assert_eq!(t.line_start(4), 3);
        assert_eq!(t.line_end(4), 5);
        assert_eq!(t.line_start(0), 0);
    }
}
