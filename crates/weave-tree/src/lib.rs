//! Summarized immutable slice B-Tree (`WeaveTree`) and structural edit (`Replace`).

#![forbid(unsafe_code)]
#![no_std]

extern crate alloc;

pub mod summary;
mod tree;
pub mod utf8;

use alloc::vec::Vec;
use core::ops::{ControlFlow, Range};
use triomphe::{AllocError, Arc};

pub use summary::{LineBreaks, Summary};

use self::tree::{join, split_at, try_delete_in_place, try_from_slices, try_insert_in_place};
use self::utf8::{
    ceil_in_slice, compute_seam_merge, decode_in_slice, floor_in_slice, is_boundary_in_slice,
    next_in_slice, prev_in_slice, try_append_slices,
};

/// B-Tree node fanout and slice size constants.
pub(crate) const MAX_FANOUT: usize = 32;
pub(crate) const MIN_FANOUT: usize = MAX_FANOUT / 2;
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

#[derive(Clone, Debug)]
pub(crate) struct ByteSlice<S: Summary> {
    pub(crate) buf: Arc<Vec<u8>>,
    pub(crate) summary: S,
    pub(crate) start: u16,
    pub(crate) end: u16,
}

const _: () = assert!(core::mem::size_of::<ByteSlice<LineBreaks>>() <= 24);

impl<S: Summary> ByteSlice<S> {
    pub(crate) fn new(buf: Arc<Vec<u8>>, range: Range<usize>, summary: S) -> Self {
        Self {
            buf,
            summary,
            start: u16::try_from(range.start).expect("slice start fits in u16"),
            end: u16::try_from(range.end).expect("slice end fits in u16"),
        }
    }

    pub(crate) fn range(&self) -> Range<usize> {
        usize::from(self.start)..usize::from(self.end)
    }

    pub(crate) fn len(&self) -> usize {
        usize::from(self.end - self.start)
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.buf[self.range()]
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ChildEntry<S: Summary> {
    pub(crate) node: Arc<SliceNode<S>>,
    pub(crate) bytes: usize,
    pub(crate) backing_bytes: usize,
    pub(crate) summary: S,
}

#[derive(Clone, Debug)]
pub(crate) enum SliceNode<S: Summary> {
    Internal { children: Vec<ChildEntry<S>> },
    Leaf { slices: Vec<ByteSlice<S>> },
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

fn trailing_utf8_prefix_len(bytes: &[u8]) -> usize {
    let len = bytes.len();
    let mut cont = 0usize;
    while cont < 3 && cont < len && utf8::is_utf8_cont(bytes[len - 1 - cont]) {
        cont += 1;
    }
    if cont == len || cont == 3 {
        return 0;
    }
    let expected = match bytes[len - 1 - cont] {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return 0,
    };
    let prefix_len = cont + 1;
    if prefix_len < expected { prefix_len } else { 0 }
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
        self.root.as_ref().map_or(0, |r| r.summary.measure())
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
        let mut cur = root;
        let mut rem = pos;
        let mut byte_acc = 0usize;
        loop {
            match cur.node.as_ref() {
                SliceNode::Internal { children } => {
                    for child in children {
                        let cm = child.summary.measure();
                        if rem <= cm {
                            cur = child;
                            break;
                        }
                        rem -= cm;
                        byte_acc += child.bytes;
                    }
                }
                SliceNode::Leaf { slices } => {
                    for slice in slices {
                        let sm = slice.summary.measure();
                        if rem <= sm {
                            return Ok(byte_acc + S::to_byte_offset(slice.bytes(), rem));
                        }
                        rem -= sm;
                        byte_acc += slice.len();
                    }
                    unreachable!("Summary invariant guarantees match");
                }
            }
        }
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
            return Ok(root.summary.measure());
        }
        let (slice, slice_start, metric_acc) = self.descend_to_byte(offset);
        Ok(metric_acc + S::from_byte_offset(slice.bytes(), offset - slice_start))
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
        let (flow, _) = Self::visit_slices::<REV, B, _>(root, 0, &range, f);
        Ok(flow)
    }

    fn visit_slices<const REV: bool, B, F: FnMut(&[u8]) -> ControlFlow<B>>(
        entry: &ChildEntry<S>,
        base: usize,
        range: &Range<usize>,
        mut f: F,
    ) -> (ControlFlow<B>, F) {
        match entry.node.as_ref() {
            SliceNode::Internal { children } => {
                if REV {
                    let mut cur_end = base + entry.bytes;
                    for child in children.iter().rev() {
                        let child_start = cur_end - child.bytes;
                        if cur_end > range.start && child_start < range.end {
                            let (flow, next_f) =
                                Self::visit_slices::<true, B, F>(child, child_start, range, f);
                            f = next_f;
                            if let ControlFlow::Break(b) = flow {
                                return (ControlFlow::Break(b), f);
                            }
                        }
                        cur_end = child_start;
                        if cur_end <= range.start {
                            break;
                        }
                    }
                } else {
                    let mut cur_base = base;
                    for child in children {
                        let child_end = cur_base + child.bytes;
                        if child_end > range.start && cur_base < range.end {
                            let (flow, next_f) =
                                Self::visit_slices::<false, B, F>(child, cur_base, range, f);
                            f = next_f;
                            if let ControlFlow::Break(b) = flow {
                                return (ControlFlow::Break(b), f);
                            }
                        }
                        cur_base = child_end;
                        if cur_base >= range.end {
                            break;
                        }
                    }
                }
                (ControlFlow::Continue(()), f)
            }
            SliceNode::Leaf { slices } => {
                if REV {
                    let mut cur_end = base + entry.bytes;
                    for slice in slices.iter().rev() {
                        let len = slice.len();
                        let slice_start = cur_end - len;
                        if cur_end > range.start && slice_start < range.end {
                            let lo = range.start.saturating_sub(slice_start);
                            let hi = (range.end - slice_start).min(len);
                            let sub = &slice.bytes()[lo..hi];
                            if let ControlFlow::Break(b) = f(sub) {
                                return (ControlFlow::Break(b), f);
                            }
                        }
                        cur_end = slice_start;
                        if cur_end <= range.start {
                            break;
                        }
                    }
                } else {
                    let mut cur_base = base;
                    for slice in slices {
                        let len = slice.len();
                        let slice_end = cur_base + len;
                        if slice_end > range.start && cur_base < range.end {
                            let lo = range.start.saturating_sub(cur_base);
                            let hi = (range.end - cur_base).min(len);
                            let sub = &slice.bytes()[lo..hi];
                            if let ControlFlow::Break(b) = f(sub) {
                                return (ControlFlow::Break(b), f);
                            }
                        }
                        cur_base = slice_end;
                        if cur_base >= range.end {
                            break;
                        }
                    }
                }
                (ControlFlow::Continue(()), f)
            }
        }
    }

    #[must_use]
    pub fn floor_boundary(&self, offset: usize) -> usize {
        let total = self.total_bytes();
        let p = offset.min(total);
        if p == 0 || p == total {
            return p;
        }
        let (slice, slice_start) = self.locate_slice(p);
        slice_start + floor_in_slice(slice.bytes(), p - slice_start)
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
        slice_start + ceil_in_slice(slice.bytes(), offset - slice_start)
    }

    #[must_use]
    pub fn prev_boundary(&self, offset: usize) -> usize {
        let total = self.total_bytes();
        let p = offset.min(total);
        if p == 0 {
            return 0;
        }
        let (slice, slice_start) = self.locate_slice(p - 1);
        slice_start + prev_in_slice(slice.bytes(), p - slice_start)
    }

    #[must_use]
    pub fn next_boundary(&self, offset: usize) -> usize {
        let total = self.total_bytes();
        if offset >= total {
            return total;
        }
        let (slice, slice_start) = self.locate_slice(offset);
        slice_start + next_in_slice(slice.bytes(), offset - slice_start)
    }

    #[must_use]
    pub fn decode_at(&self, offset: usize) -> Option<(Unit, usize)> {
        if offset >= self.total_bytes() {
            return None;
        }
        let (slice, slice_start) = self.locate_slice(offset);
        decode_in_slice(slice.bytes(), offset - slice_start)
    }

    #[must_use]
    pub fn byte_at(&self, offset: usize) -> Option<u8> {
        if offset >= self.total_bytes() {
            return None;
        }
        let (slice, slice_start) = self.locate_slice(offset);
        Some(slice.bytes()[offset - slice_start])
    }

    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.root.as_ref().map_or(0, |r| r.bytes)
    }

    /// Conservative sum of backing buffer lengths (`Arc<Vec<u8>>::len`) across all leaves.
    #[must_use]
    pub fn backing_bytes(&self) -> usize {
        self.root.as_ref().map_or(0, |r| r.backing_bytes)
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
        is_boundary_in_slice(slice.bytes(), offset - slice_start)
    }

    fn locate_slice(&self, offset: usize) -> (&ByteSlice<S>, usize) {
        let (slice, base, _) = self.descend_to_byte(offset);
        (slice, base)
    }

    fn descend_to_byte(&self, offset: usize) -> (&ByteSlice<S>, usize, usize) {
        let mut cur = self.root.as_ref().expect("offset < total_bytes");
        let mut rem = offset;
        let mut base = 0usize;
        let mut metric_acc = 0usize;
        loop {
            match cur.node.as_ref() {
                SliceNode::Internal { children } => {
                    for child in children {
                        if rem < child.bytes {
                            cur = child;
                            break;
                        }
                        rem -= child.bytes;
                        base += child.bytes;
                        metric_acc += child.summary.measure();
                    }
                }
                SliceNode::Leaf { slices } => {
                    for slice in slices {
                        let len = slice.len();
                        if rem < len {
                            return (slice, base, metric_acc);
                        }
                        rem -= len;
                        base += len;
                        metric_acc += slice.summary.measure();
                    }
                    unreachable!("offset < total_bytes");
                }
            }
        }
    }

    fn trailing_raw_bytes3(&self, offset: usize) -> ([u8; 3], usize) {
        let win_start = offset.saturating_sub(6);
        let win_len = offset - win_start;
        let buf = self.read_small_range::<6>(win_start..offset);
        let win = &buf[..win_len];
        let mut rev = [0u8; 3];
        let mut len = 0usize;
        let mut cur = win_len;
        while len < 3 && cur > 0 {
            let prev = prev_in_slice(win, cur);
            if cur - prev != 1 {
                break;
            }
            match utf8::decode_unit_raw(win, prev) {
                Some((Unit::Raw(b), 1)) => {
                    rev[len] = b;
                    len += 1;
                    cur = prev;
                }
                _ => break,
            }
        }
        rev[..len].reverse();
        (rev, len)
    }

    fn leading_raw_bytes3(&self, offset: usize) -> ([u8; 3], usize) {
        let win_end = (offset + 6).min(self.total_bytes());
        let win_len = win_end.saturating_sub(offset);
        let buf = self.read_small_range::<6>(offset..win_end);
        let win = &buf[..win_len];
        let mut bytes = [0u8; 3];
        let mut len = 0usize;
        while len < 3 && len < win_len {
            match utf8::decode_unit_raw(win, len) {
                Some((Unit::Raw(b), 1)) => {
                    bytes[len] = b;
                    len += 1;
                }
                _ => break,
            }
        }
        (bytes, len)
    }

    fn read_small_range<const N: usize>(&self, range: Range<usize>) -> [u8; N] {
        let mut out = [0u8; N];
        let mut written = 0usize;
        if range.start < range.end
            && let Some(root) = &self.root
        {
            let _ = Self::visit_slices::<false, _, _>(root, 0, &range, |chunk: &[u8]| {
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
        let depth = Self::check_entry(root, true);
        assert!(depth >= 1);
        let mut prev_tail = [0u8; 3];
        let mut prev_tail_len = 0usize;
        let _ = Self::visit_slices::<false, _, _>(root, 0, &(0..root.bytes), |chunk: &[u8]| {
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

    fn assert_entry_totals(entry: &ChildEntry<S>, bytes: usize, backing: usize, summary: S) {
        assert_eq!(entry.bytes, bytes);
        assert_eq!(entry.backing_bytes, backing);
        assert!(entry.summary == summary);
    }

    fn check_entry(entry: &ChildEntry<S>, is_root: bool) -> usize {
        let min_fanout = if is_root { 1 } else { MIN_FANOUT };
        match entry.node.as_ref() {
            SliceNode::Leaf { slices } => {
                assert!((min_fanout..=MAX_FANOUT).contains(&slices.len()));
                let mut sum_bytes = 0usize;
                let mut sum_backing = 0usize;
                let mut sum_summary = S::default();
                for slice in slices {
                    let len = slice.len();
                    assert!(len > 0);
                    assert!(len <= MAX_SLICE_BYTES);
                    let raw = slice.bytes();
                    assert!(slice.summary == S::from_slice(raw));
                    sum_bytes += len;
                    sum_backing = sum_backing.saturating_add(slice.buf.len());
                    sum_summary = sum_summary.combine(&slice.summary);
                }
                Self::assert_entry_totals(entry, sum_bytes, sum_backing, sum_summary);
                1
            }
            SliceNode::Internal { children } => {
                let min_internal = if is_root { 2 } else { MIN_FANOUT };
                assert!((min_internal..=MAX_FANOUT).contains(&children.len()));
                let mut expected_depth = None;
                let mut sum_bytes = 0usize;
                let mut sum_backing = 0usize;
                let mut sum_summary = S::default();
                for child in children {
                    let d = Self::check_entry(child, false);
                    match expected_depth {
                        None => expected_depth = Some(d),
                        Some(ed) => assert_eq!(d, ed, "D-02 uniform leaf depth violated"),
                    }
                    sum_bytes += child.bytes;
                    sum_backing = sum_backing.saturating_add(child.backing_bytes);
                    sum_summary = sum_summary.combine(&child.summary);
                }
                Self::assert_entry_totals(entry, sum_bytes, sum_backing, sum_summary);
                expected_depth.expect("non-empty internal") + 1
            }
        }
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
                if Arc::ptr_eq(&a.node, &b.node) {
                    return true;
                }
                entries_eq(a, b)
            }
            _ => false,
        }
    }
}

impl<S: Summary> Eq for WeaveTree<S> {}

#[derive(Clone, Debug)]
enum CursorFrame<'a, S: Summary> {
    Internal {
        children: &'a [ChildEntry<S>],
        idx: usize,
        base: usize,
    },
    Leaf {
        slices: &'a [ByteSlice<S>],
        idx: usize,
        in_slice: usize,
    },
}

fn push_leftmost<'a, S: Summary>(
    mut stack: Vec<CursorFrame<'a, S>>,
    entry: &'a ChildEntry<S>,
    base: usize,
) -> Vec<CursorFrame<'a, S>> {
    let mut cur = entry;
    loop {
        match cur.node.as_ref() {
            SliceNode::Internal { children } => {
                stack.push(CursorFrame::Internal {
                    children,
                    idx: 0,
                    base,
                });
                cur = &children[0];
            }
            SliceNode::Leaf { slices } => {
                stack.push(CursorFrame::Leaf {
                    slices,
                    idx: 0,
                    in_slice: 0,
                });
                return stack;
            }
        }
    }
}

fn advance_internal_past<S: Summary>(
    mut stack: Vec<CursorFrame<'_, S>>,
) -> Vec<CursorFrame<'_, S>> {
    while let Some(CursorFrame::Internal {
        children,
        idx,
        base,
    }) = stack.last_mut()
    {
        let b = children[*idx].bytes;
        *idx += 1;
        *base += b;
        if *idx < children.len() {
            let next = &children[*idx];
            let nb = *base;
            return push_leftmost(stack, next, nb);
        }
        stack.pop();
    }
    stack
}

fn advance_eq_cursor<S: Summary>(
    mut stack: Vec<CursorFrame<'_, S>>,
    bytes_consumed: usize,
) -> Vec<CursorFrame<'_, S>> {
    let Some(CursorFrame::Leaf {
        slices,
        idx,
        in_slice,
        ..
    }) = stack.last_mut()
    else {
        return stack;
    };
    *in_slice += bytes_consumed;
    if *in_slice < slices[*idx].len() {
        return stack;
    }
    *idx += 1;
    *in_slice = 0;
    if *idx < slices.len() {
        return stack;
    }
    stack.pop();
    advance_internal_past(stack)
}

fn can_skip_shared_subtree<S: Summary>(
    sa: &[CursorFrame<'_, S>],
    sb: &[CursorFrame<'_, S>],
) -> bool {
    let (Some(top_a), Some(top_b)) = (sa.len().checked_sub(2), sb.len().checked_sub(2)) else {
        return false;
    };
    let (
        CursorFrame::Internal {
            children: ca,
            idx: ia,
            base: ba,
        },
        CursorFrame::Internal {
            children: cb,
            idx: ib,
            base: bb,
        },
        CursorFrame::Leaf {
            idx: la_idx,
            in_slice: la_off,
            ..
        },
        CursorFrame::Leaf {
            idx: lb_idx,
            in_slice: lb_off,
            ..
        },
    ) = (&sa[top_a], &sb[top_b], &sa[top_a + 1], &sb[top_b + 1])
    else {
        return false;
    };
    *ba == *bb
        && *la_idx == 0
        && *la_off == 0
        && *lb_idx == 0
        && *lb_off == 0
        && Arc::ptr_eq(&ca[*ia].node, &cb[*ib].node)
}

fn entries_eq<S: Summary>(a: &ChildEntry<S>, b: &ChildEntry<S>) -> bool {
    let mut sa = push_leftmost(Vec::new(), a, 0);
    let mut sb = push_leftmost(Vec::new(), b, 0);

    loop {
        if sa.is_empty() && sb.is_empty() {
            return true;
        }
        if sa.is_empty() || sb.is_empty() {
            return false;
        }
        if can_skip_shared_subtree(&sa, &sb) {
            sa.pop();
            sb.pop();
            sa = advance_internal_past(sa);
            sb = advance_internal_past(sb);
            continue;
        }
        let (
            Some(CursorFrame::Leaf {
                slices: la,
                idx: ia,
                in_slice: oa,
                ..
            }),
            Some(CursorFrame::Leaf {
                slices: lb,
                idx: ib,
                in_slice: ob,
                ..
            }),
        ) = (sa.last(), sb.last())
        else {
            return false;
        };
        let sa_slice = &la[*ia];
        let sb_slice = &lb[*ib];
        let rem_a = sa_slice.len() - *oa;
        let rem_b = sb_slice.len() - *ob;
        let take = rem_a.min(rem_b);
        let bytes_a = &sa_slice.bytes()[*oa..*oa + take];
        let bytes_b = &sb_slice.bytes()[*ob..*ob + take];
        if bytes_a != bytes_b {
            return false;
        }
        sa = advance_eq_cursor(sa, take);
        sb = advance_eq_cursor(sb, take);
    }
}
