//! Monoidal summary metadata (`Summary`) and coordinate projection (`Metric`) traits.

use memchr::memchr_iter;

/// Slice-local additive summary metadata for `WeaveTree<S>` (S-01).
///
/// Implementations must form a commutative monoid under `Default::default()` (identity)
/// and `combine` (associative), and `split_pair(l, r)` must match
/// `(Self::from_slice(l), Self::from_slice(r))`.
pub trait Summary: Clone + Default + Eq {
    /// Computes summary metadata from a valid `Unit`-bounded byte slice.
    fn from_slice(bytes: &[u8]) -> Self;

    /// Attaches or updates the backing allocation length (`Arc<Vec<u8>>::len`) for a leaf
    /// slice. Pure content summaries leave this as a no-op.
    #[must_use]
    fn with_backing_len(self, _backing_len: usize) -> Self {
        self
    }

    /// Computes summary metadata for a leaf `ByteSlice` with its backing allocation length.
    fn from_leaf_slice(bytes: &[u8], backing_len: usize) -> Self {
        Self::from_slice(bytes).with_backing_len(backing_len)
    }

    /// Associative monoid combination.
    fn combine(&self, other: &Self) -> Self;

    /// Computes summaries for `(left, right)` after splitting a slice with summary `self`.
    ///
    /// Subtractable metrics may override this to scan only the shorter half.
    fn split_pair(&self, left: &[u8], right: &[u8]) -> (Self, Self) {
        (Self::from_slice(left), Self::from_slice(right))
    }
}

/// Projects a 1-D coordinate axis onto a `Summary` for `O(log P)` tree descent (S-02).
pub trait Metric<S: Summary> {
    /// Total metric units in `summary`.
    fn measure(summary: &S) -> usize;

    /// Converts a valid `Unit`-boundary byte offset within `slice` to a metric offset.
    fn from_byte_offset(slice: &[u8], byte_offset: usize) -> usize;

    /// Converts a metric offset within `slice` to a valid `Unit`-boundary byte offset.
    fn to_byte_offset(slice: &[u8], metric_offset: usize) -> usize;
}

impl Summary for () {
    #[inline]
    fn from_slice(_bytes: &[u8]) -> Self {}

    #[inline]
    fn combine(&self, _other: &Self) -> Self {}

    #[inline]
    fn split_pair(&self, _left: &[u8], _right: &[u8]) -> (Self, Self) {
        ((), ())
    }
}

/// Counts `'\n'` bytes in `bytes`.
#[must_use]
#[inline]
pub fn count_line_breaks(bytes: &[u8]) -> usize {
    memchr_iter(b'\n', bytes).count()
}

/// Built-in summary tracking `'\n'` line-break counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineBreaks(pub usize);

impl Summary for LineBreaks {
    #[inline]
    fn from_slice(bytes: &[u8]) -> Self {
        Self(count_line_breaks(bytes))
    }

    #[inline]
    fn combine(&self, other: &Self) -> Self {
        Self(self.0 + other.0)
    }

    #[inline]
    fn split_pair(&self, left: &[u8], right: &[u8]) -> (Self, Self) {
        if left.len() <= right.len() {
            let l = count_line_breaks(left);
            (Self(l), Self(self.0 - l))
        } else {
            let r = count_line_breaks(right);
            (Self(self.0 - r), Self(r))
        }
    }
}

/// `Metric` indexing 0-based lines delimited by `'\n'`.
///
/// `measure` returns the number of `'\n'` breaks (i.e. `total_lines - 1`).
/// `to_byte_offset(slice, k)` returns the byte offset immediately after the `k`-th `'\n'`
/// when `k >= 1` (0 when `k == 0`).
#[derive(Clone, Copy, Debug, Default)]
pub struct LineBreakMetric;

impl Metric<LineBreaks> for LineBreakMetric {
    #[inline]
    fn measure(summary: &LineBreaks) -> usize {
        summary.0
    }

    #[inline]
    fn from_byte_offset(slice: &[u8], byte_offset: usize) -> usize {
        count_line_breaks(&slice[..byte_offset])
    }

    #[inline]
    fn to_byte_offset(slice: &[u8], metric_offset: usize) -> usize {
        if metric_offset == 0 {
            return 0;
        }
        let mut seen = 0usize;
        for pos in memchr_iter(b'\n', slice) {
            seen += 1;
            if seen == metric_offset {
                return pos + 1;
            }
        }
        slice.len()
    }
}

/// Counts UTF-16 code units in a `Unit`-bounded byte slice.
///
/// Valid UTF-8 `Unit::Char(c)` contributes `c.len_utf16()` (1 or 2);
/// `Unit::Raw(_)` contributes 1 code unit (replacement unit).
#[must_use]
pub fn count_utf16_units(bytes: &[u8]) -> usize {
    let mut count = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] < 0x80 {
            count += 1;
            i += 1;
            continue;
        }
        match crate::utf8::decode_unit_raw(bytes, i) {
            Some((crate::Unit::Char(ch), len)) => {
                count += ch.len_utf16();
                i += len;
            }
            Some((crate::Unit::Raw(_), len)) => {
                count += 1;
                i += len;
            }
            None => break,
        }
    }
    count
}

/// Built-in summary tracking UTF-16 code units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Utf16CodeUnits(pub usize);

impl Summary for Utf16CodeUnits {
    #[inline]
    fn from_slice(bytes: &[u8]) -> Self {
        Self(count_utf16_units(bytes))
    }

    #[inline]
    fn combine(&self, other: &Self) -> Self {
        Self(self.0 + other.0)
    }

    #[inline]
    fn split_pair(&self, left: &[u8], right: &[u8]) -> (Self, Self) {
        if left.len() <= right.len() {
            let l = count_utf16_units(left);
            (Self(l), Self(self.0 - l))
        } else {
            let r = count_utf16_units(right);
            (Self(self.0 - r), Self(r))
        }
    }
}

/// `Metric` indexing UTF-16 code unit offsets.
#[derive(Clone, Copy, Debug, Default)]
pub struct Utf16Metric;

impl Metric<Utf16CodeUnits> for Utf16Metric {
    #[inline]
    fn measure(summary: &Utf16CodeUnits) -> usize {
        summary.0
    }

    #[inline]
    fn from_byte_offset(slice: &[u8], byte_offset: usize) -> usize {
        count_utf16_units(&slice[..byte_offset])
    }

    #[inline]
    fn to_byte_offset(slice: &[u8], metric_offset: usize) -> usize {
        let mut units = 0usize;
        let mut i = 0usize;
        while i < slice.len() && units < metric_offset {
            let Some((unit, len)) = crate::utf8::decode_unit_raw(slice, i) else {
                break;
            };
            let w = match unit {
                crate::Unit::Char(ch) => ch.len_utf16(),
                crate::Unit::Raw(_) => 1,
            };
            if units + w > metric_offset {
                break;
            }
            units += w;
            i += len;
        }
        i
    }
}
