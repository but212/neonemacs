//! Monoidal summary metadata and coordinate projection trait (`Summary`).

use memchr::memchr_iter;

/// Slice-local additive summary metadata and 1-D coordinate projection for `WeaveTree<S>`.
///
/// Implementations must form a commutative monoid under `Default::default()` (identity)
/// and `combine` (associative), and `split_pair(l, r)` must match
/// `(Self::from_slice(l), Self::from_slice(r))`.
pub trait Summary: Clone + Default + Eq {
    /// Computes summary metadata from a valid `Unit`-bounded byte slice.
    fn from_slice(bytes: &[u8]) -> Self;

    /// Associative monoid combination.
    fn combine(&self, other: &Self) -> Self;

    /// Computes summaries for `(left, right)` after splitting a slice with summary `self`.
    ///
    /// Subtractable metrics may override this to scan only the shorter half.
    fn split_pair(&self, left: &[u8], right: &[u8]) -> (Self, Self) {
        (Self::from_slice(left), Self::from_slice(right))
    }

    /// Total metric units in `self`.
    fn measure(&self) -> usize;

    /// Converts a valid `Unit`-boundary byte offset within `slice` to a metric offset.
    fn from_byte_offset(slice: &[u8], byte_offset: usize) -> usize;

    /// Converts a metric offset within `slice` to a valid `Unit`-boundary byte offset.
    fn to_byte_offset(slice: &[u8], metric_offset: usize) -> usize;
}

fn count_line_breaks(bytes: &[u8]) -> usize {
    memchr_iter(b'\n', bytes).count()
}

/// Built-in summary tracking `'\n'` line-break counts (0-based lines).
///
/// `measure` returns the number of `'\n'` breaks (i.e. `total_lines - 1`).
/// `to_byte_offset(slice, k)` returns the byte offset immediately after the `k`-th `'\n'`
/// when `k >= 1` (0 when `k == 0`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineBreaks(pub usize);

impl Summary for LineBreaks {
    fn from_slice(bytes: &[u8]) -> Self {
        Self(count_line_breaks(bytes))
    }

    fn combine(&self, other: &Self) -> Self {
        Self(self.0 + other.0)
    }

    fn split_pair(&self, left: &[u8], right: &[u8]) -> (Self, Self) {
        if left.len() <= right.len() {
            let l = count_line_breaks(left);
            (Self(l), Self(self.0 - l))
        } else {
            let r = count_line_breaks(right);
            (Self(self.0 - r), Self(r))
        }
    }

    fn measure(&self) -> usize {
        self.0
    }

    fn from_byte_offset(slice: &[u8], byte_offset: usize) -> usize {
        count_line_breaks(&slice[..byte_offset])
    }

    fn to_byte_offset(slice: &[u8], metric_offset: usize) -> usize {
        if metric_offset == 0 {
            return 0;
        }
        memchr_iter(b'\n', slice)
            .nth(metric_offset - 1)
            .map_or(slice.len(), |pos| pos + 1)
    }
}
