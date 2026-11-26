//! Summarized immutable slice B-Tree (`WeaveTree`) and structural edit (`Replace`).

#![forbid(unsafe_code)]
#![no_std]

extern crate alloc;

pub mod summary;
mod tree;
mod utf8;

use alloc::vec::Vec;
use core::ops::{ControlFlow, Range};
use triomphe::AllocError;

pub use summary::{LineBreaks, Summary};

use self::tree::{
    ByteSlice, ChildEntry, byte_to_metric, check_entry, entries_eq, join, locate_slice_bytes,
    metric_to_byte, split_at, try_append_slices, try_delete_in_place, try_from_slices,
    try_insert_in_place, visit_slices,
};
use self::utf8::{
    ceil_in_slice, compute_seam_merge, decode_in_slice, decode_unit_raw, extract_leading_raw3,
    extract_trailing_raw3, floor_in_slice, is_boundary_in_slice, next_in_slice, prev_in_slice,
    trailing_utf8_prefix_len,
};

pub const CHUNK_CAP: usize = 4096;
/// Maximum size of a single `ByteSlice` (bounds leaf scan and buffer retention).
pub const MAX_SLICE_BYTES: usize = 16 * 1024;
const _: () = assert!(MAX_SLICE_BYTES * 2 <= u16::MAX as usize);

/// Logical minimum unit of text (non-UTF-8 bytes are preserved as `Unit::Raw`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Char(char),
    Raw(u8),
}

impl Unit {
    /// Decodes the leading RFC 3629 UTF-8 unit or raw byte from `bytes`.
    #[must_use]
    pub fn decode_prefix(bytes: &[u8]) -> Option<(Self, usize)> {
        utf8::decode_unit_raw(bytes, 0)
    }
}

/// Pure value-passing immutable text B-Tree parameterized by [`Summary`].
#[derive(Clone, Debug, Default)]
pub struct WeaveTree<S: Summary> {
    pub(crate) root: Option<ChildEntry<S>>,
}

/// Structural edit operation for both core input and undo inverse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replace<S: Summary> {
    pub offset: usize,
    pub delete_len: usize,
    pub insert: WeaveTree<S>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocError {
    /// Attempted to split or query inside a multi-byte `Unit` (D-01).
    InvalidBoundary { offset: usize },
    /// Byte offset, range, or metric index exceeds document bounds.
    OutOfBounds {
        offset: usize,
        len: usize,
        total: usize,
    },
    /// The edit would produce too many bytes to represent all document offsets and counts.
    DocumentTooLarge,
}

enum InsertSource<'a, S: Summary> {
    Bytes(&'a [u8]),
    Tree(WeaveTree<S>),
}

impl<S: Summary> InsertSource<'_, S> {
    fn len(&self) -> usize {
        match self {
            Self::Bytes(b) => b.len(),
            Self::Tree(d) => d.total_bytes(),
        }
    }

    fn edge_bytes3(&self, from_tail: bool) -> ([u8; 3], usize) {
        match self {
            Self::Bytes(b) => {
                let n = b.len().min(3);
                let mut out = [0u8; 3];
                let src = if from_tail {
                    &b[b.len() - n..]
                } else {
                    &b[..n]
                };
                out[..n].copy_from_slice(src);
                (out, n)
            }
            Self::Tree(d) => {
                let total = d.total_bytes();
                let (start, end) = if from_tail {
                    (total.saturating_sub(3), total)
                } else {
                    (0, total.min(3))
                };
                (d.read_small_range::<3>(start..end), end - start)
            }
        }
    }

    fn into_trimmed_root(self, trim_left: usize, trim_right: usize) -> Option<ChildEntry<S>> {
        let total = self.len();
        if trim_left + trim_right >= total {
            return None;
        }
        match self {
            Self::Bytes(b) => {
                let sub = &b[trim_left..total - trim_right];
                WeaveTree::from_bytes(sub).root
            }
            Self::Tree(d) => {
                let (_, rest) = split_at(d.root, trim_left);
                let (trimmed, _) = split_at(rest, total - trim_left - trim_right);
                trimmed
            }
        }
    }
}

/// Incrementally collects 16 KiB `ByteSlice`s and builds a `WeaveTree` bottom-up.
#[derive(Debug)]
pub struct WeaveTreeBuilder<S: Summary> {
    slices: Vec<ByteSlice<S>>,
    pending: [u8; 3],
    pending_len: usize,
}

impl<S: Summary> Default for WeaveTreeBuilder<S> {
    fn default() -> Self {
        Self {
            slices: Vec::new(),
            pending: [0; 3],
            pending_len: 0,
        }
    }
}

impl<S: Summary> WeaveTreeBuilder<S> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn push_bytes(self, bytes: &[u8]) -> Self {
        self.try_push_bytes(bytes)
            .expect("cannot allocate document slices")
    }

    fn try_emit_and_buffer_tail(mut self, bytes: &[u8]) -> Result<Self, AllocError> {
        let keep = trailing_utf8_prefix_len(bytes);
        let emit_len = bytes.len() - keep;
        self.slices = try_append_slices(self.slices, &bytes[..emit_len])?;
        self.pending_len = keep;
        self.pending[..keep].copy_from_slice(&bytes[emit_len..]);
        Ok(self)
    }

    pub fn try_push_bytes(mut self, bytes: &[u8]) -> Result<Self, AllocError> {
        if bytes.is_empty() {
            return Ok(self);
        }
        let mut rest = bytes;
        if self.pending_len > 0 {
            let cont = rest
                .iter()
                .take(3)
                .take_while(|&&b| utf8::is_utf8_cont(b))
                .count();
            let mut bridge = [0u8; 6];
            bridge[..self.pending_len].copy_from_slice(&self.pending[..self.pending_len]);
            bridge[self.pending_len..self.pending_len + cont].copy_from_slice(&rest[..cont]);
            let bridge_len = self.pending_len + cont;
            rest = &rest[cont..];
            if rest.is_empty() {
                return self.try_emit_and_buffer_tail(&bridge[..bridge_len]);
            }
            self.slices = try_append_slices(self.slices, &bridge[..bridge_len])?;
        }
        self.try_emit_and_buffer_tail(rest)
    }

    #[must_use]
    pub fn finish(self) -> WeaveTree<S> {
        self.try_finish().expect("cannot allocate document tree")
    }

    pub fn try_finish(self) -> Result<WeaveTree<S>, AllocError> {
        let slices = try_append_slices(self.slices, &self.pending[..self.pending_len])?;
        Ok(WeaveTree {
            root: try_from_slices(slices)?,
        })
    }
}

impl<S: Summary> WeaveTree<S> {
    /// Constructs a `WeaveTree` from a raw byte slice (`root: None` if empty).
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let slices = try_append_slices(Vec::new(), bytes).expect("cannot allocate document tree");
        Self {
            root: try_from_slices(slices).expect("cannot allocate document tree"),
        }
    }

    /// Pure structural transition: consumes `self` and returns `(next_tree, inverse_edit)`.
    pub fn apply(self, edit: Replace<S>) -> Result<(Self, Replace<S>), (Self, DocError)> {
        self.apply_inner(
            edit.offset,
            edit.delete_len,
            InsertSource::Tree(edit.insert),
        )
    }

    /// Byte-slice adapter sharing the same transition engine as `apply`.
    pub fn replace(
        self,
        offset: usize,
        delete_len: usize,
        insert: &[u8],
    ) -> Result<(Self, Replace<S>), (Self, DocError)> {
        self.apply_inner(offset, delete_len, InsertSource::Bytes(insert))
    }

    fn apply_inner(
        self,
        offset: usize,
        delete_len: usize,
        insert: InsertSource<'_, S>,
    ) -> Result<(Self, Replace<S>), (Self, DocError)> {
        let total = self.total_bytes();
        let Some(end) = offset.checked_add(delete_len) else {
            return Err((
                self,
                DocError::OutOfBounds {
                    offset,
                    len: delete_len,
                    total,
                },
            ));
        };
        if end > total {
            return Err((
                self,
                DocError::OutOfBounds {
                    offset,
                    len: delete_len,
                    total,
                },
            ));
        }
        if !self.is_boundary(offset) {
            return Err((self, DocError::InvalidBoundary { offset }));
        }
        if !self.is_boundary(end) {
            return Err((self, DocError::InvalidBoundary { offset: end }));
        }

        let insert_len = insert.len();
        if insert_len >= usize::MAX - (total - delete_len) {
            return Err((self, DocError::DocumentTooLarge));
        }
        if delete_len == 0 && insert_len == 0 {
            return Ok((
                self,
                Replace {
                    offset,
                    delete_len: 0,
                    insert: Self::default(),
                },
            ));
        }

        let (left_raw, left_raw_len) = self.trailing_raw_bytes3(offset);
        let (right_raw, right_raw_len) = self.leading_raw_bytes3(end);
        let (insert_head, insert_head_len) = insert.edge_bytes3(false);
        let (insert_tail, insert_tail_len) = insert.edge_bytes3(true);
        let seam = compute_seam_merge(
            &left_raw[..left_raw_len],
            &insert_head[..insert_head_len],
            &insert_tail[..insert_tail_len],
            insert_len,
            &right_raw[..right_raw_len],
        );

        let mut tree = self;
        if delete_len == 0
            && seam.k_left == 0
            && seam.k_right == 0
            && seam.abs_insert_left == 0
            && seam.abs_insert_right == 0
            && let InsertSource::Bytes(bytes) = insert
            && !bytes.is_empty()
            && bytes.len() <= 1024
            && let Some(root) = tree.root.take()
        {
            match try_insert_in_place(root, offset, bytes) {
                Ok(updated_root) => {
                    return Ok((
                        Self {
                            root: Some(updated_root),
                        },
                        Replace {
                            offset,
                            delete_len: insert_len,
                            insert: Self::default(),
                        },
                    ));
                }
                Err(orig_root) => tree.root = Some(orig_root),
            }
        } else if delete_len > 0
            && delete_len <= 1024
            && insert_len == 0
            && seam.k_left == 0
            && seam.k_right == 0
            && let Some(root) = tree.root.take()
        {
            match try_delete_in_place(root, offset, delete_len) {
                Ok((updated_root, deleted)) => {
                    return Ok((
                        Self {
                            root: Some(updated_root),
                        },
                        Replace {
                            offset,
                            delete_len: 0,
                            insert: Self::from_bytes(&deleted),
                        },
                    ));
                }
                Err(orig_root) => tree.root = Some(orig_root),
            }
        }

        Ok(tree.apply_cow(offset, delete_len, insert, seam))
    }

    fn apply_cow(
        self,
        offset: usize,
        delete_len: usize,
        insert: InsertSource<'_, S>,
        seam: self::utf8::SeamMerge,
    ) -> (Self, Replace<S>) {
        let insert_len = insert.len();
        let k_left = usize::from(seam.k_left);
        let k_right = usize::from(seam.k_right);

        let (left_root, rest) = split_at(self.root, offset - k_left);
        let (k_left_root, rest) = if k_left > 0 {
            split_at(rest, k_left)
        } else {
            (None, rest)
        };
        let (deleted_root, rest) = if delete_len > 0 {
            split_at(rest, delete_len)
        } else {
            (None, rest)
        };
        let (k_right_root, right_root) = if k_right > 0 {
            split_at(rest, k_right)
        } else {
            (None, rest)
        };

        let left_seam_root = Self::from_bytes(seam.left_bytes()).root;
        let right_seam_root = Self::from_bytes(seam.right_bytes()).root;
        let trimmed_insert = insert.into_trimmed_root(
            usize::from(seam.abs_insert_left),
            usize::from(seam.abs_insert_right),
        );
        let mid_root = join(left_seam_root, join(trimmed_insert, right_seam_root));

        let next_root = join(left_root, join(mid_root, right_root));
        let next_doc = Self { root: next_root };

        let inv = if k_left == 0 && k_right == 0 {
            Replace {
                offset,
                delete_len: insert_len,
                insert: Self { root: deleted_root },
            }
        } else {
            Replace {
                offset: offset - k_left,
                delete_len: insert_len + k_left + k_right,
                insert: Self {
                    root: join(k_left_root, join(deleted_root, k_right_root)),
                },
            }
        };

        (next_doc, inv)
    }

    pub(crate) fn validate_range(&self, range: &Range<usize>) -> Result<(), DocError> {
        let total = self.total_bytes();
        if range.start > range.end || range.end > total {
            return Err(DocError::OutOfBounds {
                offset: range.start,
                len: range.end.saturating_sub(range.start),
                total,
            });
        }
        if !self.is_boundary(range.start) {
            return Err(DocError::InvalidBoundary {
                offset: range.start,
            });
        }
        if !self.is_boundary(range.end) {
            return Err(DocError::InvalidBoundary { offset: range.end });
        }
        Ok(())
    }

    /// Extracts a `CoW` subdocument in $O(\log P)$ worst-case time for $P$ slices.
    pub fn slice(&self, range: Range<usize>) -> Result<Self, DocError> {
        self.validate_range(&range)?;
        if range.start == range.end {
            return Ok(Self::default());
        }
        let (_, rest) = split_at(self.root.clone(), range.start);
        let (mid, _) = split_at(rest, range.end - range.start);
        Ok(Self { root: mid })
    }

    /// Returns total metric units (`0` when empty).
    #[must_use]
    pub fn total_metric(&self) -> usize {
        self.root.as_ref().map_or(0, ChildEntry::summary_measure)
    }

    /// Maps a metric coordinate `pos` to its starting byte offset.
    pub fn metric_to_byte(&self, pos: usize) -> Result<usize, DocError> {
        let total_m = self.total_metric();
        if pos > total_m {
            return Err(DocError::OutOfBounds {
                offset: pos,
                len: 0,
                total: total_m,
            });
        }
        if pos == 0 {
            return Ok(0);
        }
        let root = self
            .root
            .as_ref()
            .expect("pos >= 1 implies total_metric >= 1");
        Ok(metric_to_byte(root, pos))
    }

    /// Maps a valid `Unit`-boundary byte offset to its metric coordinate.
    pub fn byte_to_metric(&self, offset: usize) -> Result<usize, DocError> {
        let total = self.total_bytes();
        if offset > total {
            return Err(DocError::OutOfBounds {
                offset,
                len: 0,
                total,
            });
        }
        if !self.is_boundary(offset) {
            return Err(DocError::InvalidBoundary { offset });
        }
        let Some(root) = &self.root else {
            return Ok(0);
        };
        if offset == total {
            return Ok(root.summary_measure());
        }
        Ok(byte_to_metric(root, offset))
    }

    /// Zero-copy forward `Unit` streaming over `range` (`D-01` / `D-05` compatible).
    pub fn try_for_each_unit<B>(
        &self,
        range: Range<usize>,
        mut f: impl FnMut(usize, Unit, usize) -> ControlFlow<B>,
    ) -> Result<ControlFlow<B>, DocError> {
        let mut cursor = range.start;
        self.try_for_each_slice(range, |chunk| {
            let mut i = 0;
            while let Some((unit, len)) = decode_unit_raw(chunk, i) {
                if let ControlFlow::Break(b) = f(cursor + i, unit, len) {
                    return ControlFlow::Break(b);
                }
                i += len;
            }
            cursor += chunk.len();
            ControlFlow::Continue(())
        })
    }

    /// Zero-copy reverse `Unit` streaming over `range` (`D-01` / `D-05` compatible).
    pub fn try_for_each_unit_rev<B>(
        &self,
        range: Range<usize>,
        mut f: impl FnMut(usize, Unit, usize) -> ControlFlow<B>,
    ) -> Result<ControlFlow<B>, DocError> {
        let mut cursor = range.end;
        self.try_for_each_slice_rev(range, |chunk| {
            let base = cursor - chunk.len();
            cursor = base;
            let mut i = chunk.len();
            while i > 0 {
                i = prev_in_slice(chunk, i);
                let (unit, len) = decode_unit_raw(chunk, i).expect("D-01 slice boundary");
                if let ControlFlow::Break(b) = f(base + i, unit, len) {
                    return ControlFlow::Break(b);
                }
            }
            ControlFlow::Continue(())
        })
    }

    /// Zero-copy forward streaming over `range` (`ControlFlow`-based, `D-05` compatible).
    pub fn try_for_each_slice<B>(
        &self,
        range: Range<usize>,
        f: impl FnMut(&[u8]) -> ControlFlow<B>,
    ) -> Result<ControlFlow<B>, DocError> {
        self.for_each_slice_dir::<false, B>(range, f)
    }

    /// Zero-copy reverse streaming over `range` (`ControlFlow`-based, `D-05` compatible).
    pub fn try_for_each_slice_rev<B>(
        &self,
        range: Range<usize>,
        f: impl FnMut(&[u8]) -> ControlFlow<B>,
    ) -> Result<ControlFlow<B>, DocError> {
        self.for_each_slice_dir::<true, B>(range, f)
    }

    fn for_each_slice_dir<const REV: bool, B>(
        &self,
        range: Range<usize>,
        f: impl FnMut(&[u8]) -> ControlFlow<B>,
    ) -> Result<ControlFlow<B>, DocError> {
        self.validate_range(&range)?;
        if range.start == range.end {
            return Ok(ControlFlow::Continue(()));
        }
        let Some(root) = &self.root else {
            return Ok(ControlFlow::Continue(()));
        };
        let (flow, _) = visit_slices::<REV, B, S, _>(root, 0, &range, f);
        Ok(flow)
    }

    #[must_use]
    pub fn floor_boundary(&self, offset: usize) -> usize {
        let total = self.total_bytes();
        let p = offset.min(total);
        if p == 0 || p == total {
            return p;
        }
        let (slice, slice_start) = self.locate_slice(p);
        slice_start + floor_in_slice(slice, p - slice_start)
    }

    #[must_use]
    pub fn ceil_boundary(&self, offset: usize) -> usize {
        let total = self.total_bytes();
        if offset >= total {
            return total;
        }
        if offset == 0 {
            return 0;
        }
        let (slice, slice_start) = self.locate_slice(offset);
        slice_start + ceil_in_slice(slice, offset - slice_start)
    }

    #[must_use]
    pub fn prev_boundary(&self, offset: usize) -> usize {
        let total = self.total_bytes();
        let p = offset.min(total);
        if p == 0 {
            return 0;
        }
        let (slice, slice_start) = self.locate_slice(p - 1);
        slice_start + prev_in_slice(slice, p - slice_start)
    }

    #[must_use]
    pub fn next_boundary(&self, offset: usize) -> usize {
        let total = self.total_bytes();
        if offset >= total {
            return total;
        }
        let (slice, slice_start) = self.locate_slice(offset);
        slice_start + next_in_slice(slice, offset - slice_start)
    }

    #[must_use]
    pub fn decode_at(&self, offset: usize) -> Option<(Unit, usize)> {
        if offset >= self.total_bytes() {
            return None;
        }
        let (slice, slice_start) = self.locate_slice(offset);
        decode_in_slice(slice, offset - slice_start)
    }

    #[must_use]
    pub fn byte_at(&self, offset: usize) -> Option<u8> {
        if offset >= self.total_bytes() {
            return None;
        }
        let (slice, slice_start) = self.locate_slice(offset);
        Some(slice[offset - slice_start])
    }

    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.root.as_ref().map_or(0, ChildEntry::bytes)
    }

    /// Conservative sum of backing buffer lengths (`Arc<Vec<u8>>::len`) across all leaves.
    #[must_use]
    pub fn backing_bytes(&self) -> usize {
        self.root.as_ref().map_or(0, ChildEntry::backing_bytes)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total_bytes() == 0
    }

    #[must_use]
    pub fn is_boundary(&self, offset: usize) -> bool {
        let total = self.total_bytes();
        if offset > total {
            return false;
        }
        if offset == 0 || offset == total {
            return true;
        }
        let (slice, slice_start) = self.locate_slice(offset);
        is_boundary_in_slice(slice, offset - slice_start)
    }

    fn locate_slice(&self, offset: usize) -> (&[u8], usize) {
        let root = self.root.as_ref().expect("offset < total_bytes");
        locate_slice_bytes(root, offset)
    }

    fn trailing_raw_bytes3(&self, offset: usize) -> ([u8; 3], usize) {
        let win_start = offset.saturating_sub(6);
        let win_len = offset - win_start;
        let buf = self.read_small_range::<6>(win_start..offset);
        extract_trailing_raw3(&buf[..win_len])
    }

    fn leading_raw_bytes3(&self, offset: usize) -> ([u8; 3], usize) {
        let win_end = (offset + 6).min(self.total_bytes());
        let win_len = win_end.saturating_sub(offset);
        let buf = self.read_small_range::<6>(offset..win_end);
        extract_leading_raw3(&buf[..win_len])
    }

    fn read_small_range<const N: usize>(&self, range: Range<usize>) -> [u8; N] {
        let mut out = [0u8; N];
        let mut written = 0usize;
        if range.start < range.end
            && let Some(root) = &self.root
        {
            let _ = visit_slices::<false, _, S, _>(root, 0, &range, |chunk: &[u8]| {
                let take = chunk.len().min(N - written);
                out[written..written + take].copy_from_slice(&chunk[..take]);
                written += take;
                if written >= N {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            });
        }
        out
    }

    /// Verifies all structural invariants (`D-01`, `D-02`, `D-03`, `S-01`).
    pub fn assert_invariants(&self) {
        let Some(root) = &self.root else {
            return;
        };
        let depth = check_entry(root, true);
        assert!(depth >= 1);
        let mut prev_tail = [0u8; 3];
        let mut prev_tail_len = 0usize;
        let _ = visit_slices::<false, _, S, _>(root, 0, &(0..root.bytes()), |chunk: &[u8]| {
            if prev_tail_len > 0 {
                let mut seam = [0u8; 6];
                let head_len = chunk.len().min(3);
                seam[..prev_tail_len].copy_from_slice(&prev_tail[..prev_tail_len]);
                seam[prev_tail_len..prev_tail_len + head_len].copy_from_slice(&chunk[..head_len]);
                assert!(
                    is_boundary_in_slice(&seam[..prev_tail_len + head_len], prev_tail_len),
                    "D-01 violated: Unit::Char split across ByteSlice boundary"
                );
            }
            if chunk.len() >= 3 {
                prev_tail.copy_from_slice(&chunk[chunk.len() - 3..]);
                prev_tail_len = 3;
            } else {
                let total = prev_tail_len + chunk.len();
                let mut combined = [0u8; 5];
                combined[..prev_tail_len].copy_from_slice(&prev_tail[..prev_tail_len]);
                combined[prev_tail_len..total].copy_from_slice(chunk);
                let keep = total.min(3);
                prev_tail[..keep].copy_from_slice(&combined[total - keep..total]);
                prev_tail_len = keep;
            }
            ControlFlow::<()>::Continue(())
        });
    }
}

impl<S: Summary> PartialEq for WeaveTree<S> {
    fn eq(&self, other: &Self) -> bool {
        if self.total_bytes() != other.total_bytes() {
            return false;
        }
        match (&self.root, &other.root) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                if a.ptr_eq(b) {
                    return true;
                }
                entries_eq(a, b)
            }
            _ => false,
        }
    }
}

impl<S: Summary> Eq for WeaveTree<S> {}
