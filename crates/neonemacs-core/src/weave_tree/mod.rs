//! Summarized immutable slice B-Tree (`Doc`) and structural edit (`Replace`).

use alloc::vec::Vec;
mod tree;
mod utf8;

use core::ops::{ControlFlow, Range};

use memchr::memchr_iter;
use triomphe::{AllocError, Arc};

use self::tree::{from_slices, join, split_at, try_from_slices, try_insert_in_place};
use self::utf8::{
    ceil_in_slice, chunk_bytes_to_slices, compute_seam_merge, count_line_breaks, floor_in_slice,
    is_boundary_in_slice, next_in_slice, try_append_slices,
};
pub(crate) use self::utf8::{decode_in_slice, decode_unit_raw, prev_in_slice};

/// B-Tree node fanout and slice size constants.
pub const MAX_FANOUT: usize = 32;
pub const MIN_FANOUT: usize = MAX_FANOUT / 2;
pub const CHUNK_CAP: usize = 4096;
/// Maximum size of a single `ByteSlice` (bounds `line_to_offset` scan and buffer retention).
pub const MAX_SLICE_BYTES: usize = 16 * 1024;

/// Logical minimum unit of text (non-UTF-8 bytes are preserved as `Unit::Raw`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Char(char),
    Raw(u8),
}

#[derive(Clone, Debug)]
pub(crate) struct ByteSlice {
    pub(crate) buf: Arc<Vec<u8>>,
    pub(crate) range: Range<usize>,
    pub(crate) line_breaks: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct ChildEntry {
    pub(crate) node: Arc<SliceNode>,
    pub(crate) bytes: usize,
    pub(crate) backing_bytes: usize,
    pub(crate) line_breaks: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum SliceNode {
    Internal { children: Vec<ChildEntry> },
    Leaf { slices: Vec<ByteSlice> },
}

/// Pure value-passing immutable document core (`SliceTree`).
#[derive(Clone, Debug, Default)]
pub struct Doc {
    root: Option<ChildEntry>,
}

/// Structural edit operation for both core input and undo inverse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replace {
    pub offset: usize,
    pub delete_len: usize,
    pub insert: Doc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocError {
    /// Attempted to split or query inside a multi-byte `Unit` (D-01).
    InvalidBoundary { offset: usize },
    /// Byte offset or range exceeds document length.
    OutOfBounds {
        offset: usize,
        len: usize,
        total: usize,
    },
    /// Line index exceeds total lines in document.
    LineOutOfBounds { line: usize, total_lines: usize },
    /// The edit would produce too many bytes to represent all document offsets and counts.
    DocumentTooLarge,
}

enum InsertSource<'a> {
    Bytes(&'a [u8]),
    Tree(Doc),
}

impl InsertSource<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Bytes(b) => b.len(),
            Self::Tree(d) => d.total_bytes(),
        }
    }

    fn head_bytes3(&self) -> ([u8; 3], usize) {
        match self {
            Self::Bytes(b) => {
                let n = b.len().min(3);
                let mut out = [0u8; 3];
                out[..n].copy_from_slice(&b[..n]);
                (out, n)
            }
            Self::Tree(d) => {
                let n = d.total_bytes().min(3);
                (d.read_small_range(0..n), n)
            }
        }
    }

    fn tail_bytes3(&self) -> ([u8; 3], usize) {
        match self {
            Self::Bytes(b) => {
                let n = b.len().min(3);
                let mut out = [0u8; 3];
                out[..n].copy_from_slice(&b[b.len() - n..]);
                (out, n)
            }
            Self::Tree(d) => {
                let total = d.total_bytes();
                let start = total.saturating_sub(3);
                let n = total - start;
                (d.read_small_range(start..total), n)
            }
        }
    }

    fn into_trimmed_root(self, trim_left: usize, trim_right: usize) -> Option<ChildEntry> {
        let total = self.len();
        if trim_left + trim_right >= total {
            return None;
        }
        match self {
            Self::Bytes(b) => {
                let sub = &b[trim_left..total - trim_right];
                from_slices(chunk_bytes_to_slices(sub))
            }
            Self::Tree(d) => {
                let (_, rest) = split_at(d.root, trim_left);
                let (trimmed, _) = split_at(rest, total - trim_left - trim_right);
                trimmed
            }
        }
    }
}

/// Returns the number of trailing bytes (`0..=3`) in `bytes` that form an incomplete
/// RFC 3629 UTF-8 lead/continuation prefix and could complete a `Unit::Char` with following bytes.
fn trailing_utf8_prefix_len(bytes: &[u8]) -> usize {
    let len = bytes.len();
    let mut cont = 0usize;
    while cont < 3 && cont < len && (bytes[len - 1 - cont] & 0xC0) == 0x80 {
        cont += 1;
    }
    if cont == len || cont == 3 {
        return 0;
    }
    let lead_idx = len - 1 - cont;
    let expected = match bytes[lead_idx] {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return 0,
    };
    let prefix_len = cont + 1;
    if prefix_len < expected { prefix_len } else { 0 }
}

/// Incrementally collects 16 KiB `ByteSlice`s and builds a `Doc` bottom-up via `from_slices`.
#[derive(Debug, Default)]
pub struct DocBuilder {
    slices: Vec<ByteSlice>,
    pending: [u8; 3],
    pending_len: usize,
}

impl DocBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `bytes` as 16 KiB `ByteSlice`s without flattening the whole document,
    /// carrying up to 3 trailing incomplete UTF-8 prefix bytes across chunk boundaries (D-01).
    #[must_use]
    pub fn push_bytes(self, bytes: &[u8]) -> Self {
        self.try_push_bytes(bytes)
            .expect("cannot allocate document slices")
    }

    /// Appends bytes, returning an error if slice storage cannot be allocated.
    pub fn try_push_bytes(mut self, bytes: &[u8]) -> Result<Self, AllocError> {
        if bytes.is_empty() {
            return Ok(self);
        }
        let mut rest = bytes;
        if self.pending_len > 0 {
            let cont = rest
                .iter()
                .take(3)
                .take_while(|&&b| (b & 0xC0) == 0x80)
                .count();
            let mut bridge = [0u8; 6];
            bridge[..self.pending_len].copy_from_slice(&self.pending[..self.pending_len]);
            bridge[self.pending_len..self.pending_len + cont].copy_from_slice(&rest[..cont]);
            let bridge_len = self.pending_len + cont;
            rest = &rest[cont..];
            if rest.is_empty() {
                let keep = trailing_utf8_prefix_len(&bridge[..bridge_len]);
                let emit_len = bridge_len - keep;
                self.slices = try_append_slices(self.slices, &bridge[..emit_len])?;
                self.pending_len = keep;
                self.pending[..keep].copy_from_slice(&bridge[emit_len..bridge_len]);
                return Ok(self);
            }
            self.slices = try_append_slices(self.slices, &bridge[..bridge_len])?;
        }
        let keep = trailing_utf8_prefix_len(rest);
        let emit_len = rest.len() - keep;
        self.slices = try_append_slices(self.slices, &rest[..emit_len])?;
        self.pending_len = keep;
        self.pending[..keep].copy_from_slice(&rest[emit_len..]);
        Ok(self)
    }

    /// Builds the balanced B-Tree in $O(N)$ bottom-up, flushing any incomplete trailing prefix.
    #[must_use]
    pub fn finish(self) -> Doc {
        self.try_finish().expect("cannot allocate document tree")
    }

    /// Finalizes the tree, returning an error if slice or node storage cannot be allocated.
    pub fn try_finish(self) -> Result<Doc, AllocError> {
        let slices = try_append_slices(self.slices, &self.pending[..self.pending_len])?;
        Ok(Doc {
            root: try_from_slices(slices)?,
        })
    }
}

impl Doc {
    /// Constructs a `Doc` from a raw byte slice (`root: None` if empty).
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let root = from_slices(chunk_bytes_to_slices(bytes));
        Self { root }
    }

    /// Pure structural transition: consumes `self` and returns `(next_doc, inverse_edit)`.
    pub fn apply(self, edit: Replace) -> Result<(Self, Replace), (Self, DocError)> {
        self.apply_inner(
            edit.offset,
            edit.delete_len,
            InsertSource::Tree(edit.insert),
        )
    }

    /// Byte-slice adapter sharing the same 5-step transition engine as `apply`.
    pub fn replace(
        self,
        offset: usize,
        delete_len: usize,
        insert: &[u8],
    ) -> Result<(Self, Replace), (Self, DocError)> {
        self.apply_inner(offset, delete_len, InsertSource::Bytes(insert))
    }

    fn apply_inner(
        self,
        offset: usize,
        delete_len: usize,
        insert: InsertSource<'_>,
    ) -> Result<(Self, Replace), (Self, DocError)> {
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
        // Leave room for a past-end offset and for one more line than newline bytes.
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

        // Step 3: Pre-mutation UTF-8 seam analysis (D-01).
        let (left_raw, left_raw_len) = self.trailing_raw_bytes3(offset);
        let (right_raw, right_raw_len) = self.leading_raw_bytes3(end);
        let (insert_head, insert_head_len) = insert.head_bytes3();
        let (insert_tail, insert_tail_len) = insert.tail_bytes3();
        let seam = compute_seam_merge(
            &left_raw[..left_raw_len],
            &insert_head[..insert_head_len],
            &insert_tail[..insert_tail_len],
            insert_len,
            &right_raw[..right_raw_len],
        );

        // H1 fast path: in-place append for micro-edits when uniquely owned and seam-free.
        if delete_len == 0
            && seam.k_left == 0
            && seam.k_right == 0
            && seam.abs_insert_left == 0
            && seam.abs_insert_right == 0
            && let InsertSource::Bytes(bytes) = insert
            && !bytes.is_empty()
            && bytes.len() <= CHUNK_CAP.min(1024)
        {
            if let Some(root) = self.root {
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
                    Err(orig_root) => {
                        return Self {
                            root: Some(orig_root),
                        }
                        .apply_cow(offset, delete_len, insert, seam);
                    }
                }
            } else {
                return Ok((
                    Self {
                        root: from_slices(chunk_bytes_to_slices(bytes)),
                    },
                    Replace {
                        offset,
                        delete_len: insert_len,
                        insert: Self::default(),
                    },
                ));
            }
        }

        self.apply_cow(offset, delete_len, insert, seam)
    }

    fn apply_cow(
        self,
        offset: usize,
        delete_len: usize,
        insert: InsertSource<'_>,
        seam: self::utf8::SeamMerge,
    ) -> Result<(Self, Replace), (Self, DocError)> {
        let insert_len = insert.len();

        // Step 4: Structural split without flattening.
        let (left_root, rest) = split_at(self.root, offset - seam.k_left);
        let (k_left_root, rest) = if seam.k_left > 0 {
            split_at(rest, seam.k_left)
        } else {
            (None, rest)
        };
        let (deleted_root, rest) = if delete_len > 0 {
            split_at(rest, delete_len)
        } else {
            (None, rest)
        };
        let (k_right_root, right_root) = if seam.k_right > 0 {
            split_at(rest, seam.k_right)
        } else {
            (None, rest)
        };

        // Step 5: Assemble inserted middle tree with seam-merged UTF-8 pieces.
        let left_seam_root = from_slices(chunk_bytes_to_slices(&seam.left_merged));
        let right_seam_root = from_slices(chunk_bytes_to_slices(&seam.right_merged));
        let trimmed_insert = insert.into_trimmed_root(seam.abs_insert_left, seam.abs_insert_right);
        let mid_root = join(left_seam_root, join(trimmed_insert, right_seam_root));

        // Step 6: Rejoin and rebalance.
        let next_root = join(left_root, join(mid_root, right_root));
        let next_doc = Self { root: next_root };

        // Step 7: Construct inverse Replace (D-04).
        let inv = if seam.k_left == 0 && seam.k_right == 0 {
            Replace {
                offset,
                delete_len: insert_len,
                insert: Self { root: deleted_root },
            }
        } else {
            Replace {
                offset: offset - seam.k_left,
                delete_len: insert_len + seam.k_left + seam.k_right,
                insert: Self {
                    root: join(k_left_root, join(deleted_root, k_right_root)),
                },
            }
        };

        Ok((next_doc, inv))
    }

    fn validate_range(&self, range: &Range<usize>) -> Result<(), DocError> {
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

    /// Extracts a `CoW` subdocument in O(log P) worst-case time for P slices.
    pub fn slice(&self, range: Range<usize>) -> Result<Self, DocError> {
        self.validate_range(&range)?;
        if range.start == range.end {
            return Ok(Self::default());
        }
        let (_, rest) = split_at(self.root.clone(), range.start);
        let (mid, _) = split_at(rest, range.end - range.start);
        Ok(Self { root: mid })
    }

    /// Maps a 0-based line index to its starting byte offset.
    pub fn line_to_offset(&self, line: usize) -> Result<usize, DocError> {
        let total_lines = self.total_lines();
        if line >= total_lines {
            return Err(DocError::LineOutOfBounds { line, total_lines });
        }
        if line == 0 {
            return Ok(0);
        }
        let root = self
            .root
            .as_ref()
            .expect("line >= 1 implies total_lines > 1");
        let mut cur = root;
        let mut rem_lines = line;
        let mut byte_acc = 0usize;
        loop {
            match cur.node.as_ref() {
                SliceNode::Internal { children } => {
                    for child in children {
                        if rem_lines <= child.line_breaks {
                            cur = child;
                            break;
                        }
                        rem_lines -= child.line_breaks;
                        byte_acc += child.bytes;
                    }
                }
                SliceNode::Leaf { slices } => {
                    for slice in slices {
                        if rem_lines <= slice.line_breaks {
                            let bytes = &slice.buf[slice.range.clone()];
                            let mut seen = 0usize;
                            for nl_pos in memchr_iter(b'\n', bytes) {
                                seen += 1;
                                if seen == rem_lines {
                                    return Ok(byte_acc + nl_pos + 1);
                                }
                            }
                            unreachable!("line_breaks invariant guarantees match");
                        }
                        rem_lines -= slice.line_breaks;
                        byte_acc += slice.range.len();
                    }
                    unreachable!("line_breaks invariant guarantees match");
                }
            }
        }
    }

    /// Maps a valid `Unit`-boundary byte offset to its 0-based line index.
    pub fn offset_to_line(&self, offset: usize) -> Result<usize, DocError> {
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
            return Ok(root.line_breaks);
        }
        let mut cur = root;
        let mut rem_bytes = offset;
        let mut line_acc = 0usize;
        loop {
            match cur.node.as_ref() {
                SliceNode::Internal { children } => {
                    for child in children {
                        if rem_bytes < child.bytes {
                            cur = child;
                            break;
                        }
                        rem_bytes -= child.bytes;
                        line_acc += child.line_breaks;
                    }
                }
                SliceNode::Leaf { slices } => {
                    for slice in slices {
                        let len = slice.range.len();
                        if rem_bytes < len {
                            let sub =
                                &slice.buf[slice.range.start..slice.range.start + rem_bytes];
                            return Ok(line_acc + count_line_breaks(sub));
                        }
                        rem_bytes -= len;
                        line_acc += slice.line_breaks;
                    }
                    return Ok(line_acc);
                }
            }
        }
    }

    /// Zero-copy forward streaming over `range` (D-05).
    pub fn try_for_each_slice<B>(
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
        let (flow, _) = Self::visit_fwd(root, 0, &range, f);
        Ok(flow)
    }

    /// Zero-copy reverse streaming over `range` (D-05).
    pub fn try_for_each_slice_rev<B>(
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
        let (flow, _) = Self::visit_rev(root, 0, &range, f);
        Ok(flow)
    }

    fn visit_fwd<B, F: FnMut(&[u8]) -> ControlFlow<B>>(
        entry: &ChildEntry,
        base: usize,
        range: &Range<usize>,
        mut f: F,
    ) -> (ControlFlow<B>, F) {
        match entry.node.as_ref() {
            SliceNode::Internal { children } => {
                let mut cur_base = base;
                for child in children {
                    let child_end = cur_base + child.bytes;
                    if child_end > range.start && cur_base < range.end {
                        let (flow, next_f) = Self::visit_fwd(child, cur_base, range, f);
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
                (ControlFlow::Continue(()), f)
            }
            SliceNode::Leaf { slices } => {
                let mut cur_base = base;
                for slice in slices {
                    let len = slice.range.len();
                    let slice_end = cur_base + len;
                    if slice_end > range.start && cur_base < range.end {
                        let lo = range.start.saturating_sub(cur_base);
                        let hi = (range.end - cur_base).min(len);
                        let sub = &slice.buf[slice.range.start + lo..slice.range.start + hi];
                        if let ControlFlow::Break(b) = f(sub) {
                            return (ControlFlow::Break(b), f);
                        }
                    }
                    cur_base = slice_end;
                    if cur_base >= range.end {
                        break;
                    }
                }
                (ControlFlow::Continue(()), f)
            }
        }
    }

    fn visit_rev<B, F: FnMut(&[u8]) -> ControlFlow<B>>(
        entry: &ChildEntry,
        base: usize,
        range: &Range<usize>,
        mut f: F,
    ) -> (ControlFlow<B>, F) {
        match entry.node.as_ref() {
            SliceNode::Internal { children } => {
                let mut cur_end = base + entry.bytes;
                for child in children.iter().rev() {
                    let child_start = cur_end - child.bytes;
                    if cur_end > range.start && child_start < range.end {
                        let (flow, next_f) = Self::visit_rev(child, child_start, range, f);
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
                (ControlFlow::Continue(()), f)
            }
            SliceNode::Leaf { slices } => {
                let mut cur_end = base + entry.bytes;
                for slice in slices.iter().rev() {
                    let len = slice.range.len();
                    let slice_start = cur_end - len;
                    if cur_end > range.start && slice_start < range.end {
                        let lo = range.start.saturating_sub(slice_start);
                        let hi = (range.end - slice_start).min(len);
                        let sub = &slice.buf[slice.range.start + lo..slice.range.start + hi];
                        if let ControlFlow::Break(b) = f(sub) {
                            return (ControlFlow::Break(b), f);
                        }
                    }
                    cur_end = slice_start;
                    if cur_end <= range.start {
                        break;
                    }
                }
                (ControlFlow::Continue(()), f)
            }
        }
    }

    /// Clamps `offset` to `total_bytes()`, then returns the greatest valid `Unit` boundary <= `offset`.
    #[must_use]
    pub fn floor_boundary(&self, offset: usize) -> usize {
        let total = self.total_bytes();
        let p = offset.min(total);
        if p == 0 || p == total {
            return p;
        }
        let (slice, slice_start) = self.locate_slice(p);
        slice_start + floor_in_slice(&slice.buf[slice.range.clone()], p - slice_start)
    }

    /// Returns `total_bytes()` if `offset >= total_bytes()`, else the smallest valid `Unit` boundary >= `offset`.
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
        slice_start + ceil_in_slice(&slice.buf[slice.range.clone()], offset - slice_start)
    }

    /// Clamps `offset` to `total_bytes()`; returns `0` if `0`, else the greatest valid `Unit` boundary < `offset`.
    #[must_use]
    pub fn prev_boundary(&self, offset: usize) -> usize {
        let total = self.total_bytes();
        let p = offset.min(total);
        if p == 0 {
            return 0;
        }
        let (slice, slice_start) = self.locate_slice(p - 1);
        slice_start + prev_in_slice(&slice.buf[slice.range.clone()], p - slice_start)
    }

    /// Returns `total_bytes()` if `offset >= total_bytes()`, else the smallest valid `Unit` boundary > `offset`.
    #[must_use]
    pub fn next_boundary(&self, offset: usize) -> usize {
        let total = self.total_bytes();
        if offset >= total {
            return total;
        }
        let (slice, slice_start) = self.locate_slice(offset);
        slice_start + next_in_slice(&slice.buf[slice.range.clone()], offset - slice_start)
    }

    /// Decodes the `Unit` at `offset`, returning `None` if `offset >= total_bytes()` or not a boundary.
    #[must_use]
    pub fn decode_at(&self, offset: usize) -> Option<(Unit, usize)> {
        if offset >= self.total_bytes() {
            return None;
        }
        let (slice, slice_start) = self.locate_slice(offset);
        decode_in_slice(&slice.buf[slice.range.clone()], offset - slice_start)
    }

    /// Returns the raw byte at `offset`, or `None` if `offset >= total_bytes()`.
    #[must_use]
    pub fn byte_at(&self, offset: usize) -> Option<u8> {
        if offset >= self.total_bytes() {
            return None;
        }
        let (slice, slice_start) = self.locate_slice(offset);
        Some(slice.buf[slice.range.start + (offset - slice_start)])
    }

    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.root.as_ref().map_or(0, |r| r.bytes)
    }

    /// Conservative byte-storage charge: counts the entire backing allocation per slice,
    /// even when slices share an allocation.
    pub(crate) fn backing_bytes(&self) -> usize {
        self.root.as_ref().map_or(0, |r| r.backing_bytes)
    }

    /// Copy at most one small chunk rather than pinning larger backing allocations.
    pub(crate) fn compact_small(self) -> Self {
        let len = self.total_bytes();
        if len > CHUNK_CAP || self.backing_bytes() <= CHUNK_CAP {
            return self;
        }
        let mut bytes = Vec::with_capacity(len);
        let _ = self.try_for_each_slice(0..len, |chunk| {
            bytes.extend_from_slice(chunk);
            ControlFlow::<()>::Continue(())
        });
        Self::from_bytes(&bytes)
    }

    #[must_use]
    pub fn total_lines(&self) -> usize {
        self.root.as_ref().map_or(1, |r| 1 + r.line_breaks)
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
        is_boundary_in_slice(&slice.buf[slice.range.clone()], offset - slice_start)
    }

    /// Precondition: `offset < self.total_bytes()`.
    fn locate_slice(&self, offset: usize) -> (&ByteSlice, usize) {
        let mut cur = self.root.as_ref().expect("offset < total_bytes");
        let mut rem = offset;
        let mut base = 0usize;
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
                    }
                }
                SliceNode::Leaf { slices } => {
                    for slice in slices {
                        let len = slice.range.len();
                        if rem < len {
                            return (slice, base);
                        }
                        rem -= len;
                        base += len;
                    }
                    unreachable!("offset < total_bytes");
                }
            }
        }
    }

    fn trailing_raw_bytes3(&self, offset: usize) -> ([u8; 3], usize) {
        let mut rev = [0u8; 3];
        let mut len = 0usize;
        let mut cur = offset;
        while len < 3 && cur > 0 {
            let prev = self.prev_boundary(cur);
            if cur - prev != 1 {
                break;
            }
            match self.decode_at(prev) {
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
        let mut bytes = [0u8; 3];
        let mut len = 0usize;
        let mut cur = offset;
        let total = self.total_bytes();
        while len < 3 && cur < total {
            match self.decode_at(cur) {
                Some((Unit::Raw(b), 1)) => {
                    bytes[len] = b;
                    len += 1;
                    cur += 1;
                }
                _ => break,
            }
        }
        (bytes, len)
    }

    fn read_small_range(&self, range: Range<usize>) -> [u8; 3] {
        let mut out = [0u8; 3];
        let mut written = 0usize;
        if range.start < range.end
            && let Some(root) = &self.root
        {
            let _ = Self::visit_fwd(root, 0, &range, |chunk: &[u8]| {
                let take = chunk.len().min(3 - written);
                out[written..written + take].copy_from_slice(&chunk[..take]);
                written += take;
                if written >= 3 {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            });
        }
        out
    }

    #[cfg(any(test, debug_assertions))]
    pub fn assert_invariants(&self) {
        let Some(root) = &self.root else {
            return;
        };
        let depth = Self::check_entry(root, true);
        assert!(depth >= 1);
        // D-01: verify no Unit::Char is split across adjacent ByteSlice boundaries.
        let mut prev_tail = [0u8; 3];
        let mut prev_tail_len = 0usize;
        let _ = Self::visit_fwd(root, 0, &(0..root.bytes), |chunk: &[u8]| {
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

    #[cfg(any(test, debug_assertions))]
    fn check_entry(entry: &ChildEntry, is_root: bool) -> usize {
        match entry.node.as_ref() {
            SliceNode::Leaf { slices } => {
                if is_root {
                    assert!((1..=MAX_FANOUT).contains(&slices.len()));
                } else {
                    assert!((MIN_FANOUT..=MAX_FANOUT).contains(&slices.len()));
                }
                let mut sum_bytes = 0usize;
                let mut sum_backing = 0usize;
                let mut sum_lines = 0usize;
                for slice in slices {
                    assert!(!slice.range.is_empty());
                    assert!(slice.range.len() <= MAX_SLICE_BYTES);
                    let raw = &slice.buf[slice.range.clone()];
                    assert_eq!(slice.line_breaks, count_line_breaks(raw));
                    // D-01: no Unit::Char split across slice boundaries (first byte cannot be
                    // an orphaned UTF-8 continuation that belongs to an earlier char, and
                    // trailing bytes cannot be an incomplete prefix that continues in the next slice).
                    sum_bytes += slice.range.len();
                    sum_backing = sum_backing.saturating_add(slice.buf.len());
                    sum_lines += slice.line_breaks;
                }
                assert_eq!(entry.bytes, sum_bytes);
                assert_eq!(entry.backing_bytes, sum_backing);
                assert_eq!(entry.line_breaks, sum_lines);
                1
            }
            SliceNode::Internal { children } => {
                if is_root {
                    assert!((2..=MAX_FANOUT).contains(&children.len()));
                } else {
                    assert!((MIN_FANOUT..=MAX_FANOUT).contains(&children.len()));
                }
                let mut expected_depth = None;
                let mut sum_bytes = 0usize;
                let mut sum_backing = 0usize;
                let mut sum_lines = 0usize;
                for child in children {
                    let d = Self::check_entry(child, false);
                    match expected_depth {
                        None => expected_depth = Some(d),
                        Some(ed) => assert_eq!(d, ed, "D-02 uniform leaf depth violated"),
                    }
                    sum_bytes += child.bytes;
                    sum_backing = sum_backing.saturating_add(child.backing_bytes);
                    sum_lines += child.line_breaks;
                }
                assert_eq!(entry.bytes, sum_bytes);
                assert_eq!(entry.backing_bytes, sum_backing);
                assert_eq!(entry.line_breaks, sum_lines);
                expected_depth.expect("non-empty internal") + 1
            }
        }
    }
}

impl PartialEq for Doc {
    fn eq(&self, other: &Self) -> bool {
        if self.total_bytes() != other.total_bytes() || self.total_lines() != other.total_lines() {
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

impl Eq for Doc {}

enum CursorFrame<'a> {
    Internal {
        children: &'a [ChildEntry],
        idx: usize,
        base: usize,
    },
    Leaf {
        slices: &'a [ByteSlice],
        idx: usize,
        in_slice: usize,
    },
}

fn push_leftmost<'a>(
    mut stack: Vec<CursorFrame<'a>>,
    entry: &'a ChildEntry,
    base: usize,
) -> Vec<CursorFrame<'a>> {
    let mut cur = entry;
    let cur_base = base;
    loop {
        match cur.node.as_ref() {
            SliceNode::Internal { children } => {
                stack.push(CursorFrame::Internal {
                    children,
                    idx: 0,
                    base: cur_base,
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

fn advance_cursor(mut stack: Vec<CursorFrame<'_>>, bytes_consumed: usize) -> Vec<CursorFrame<'_>> {
    let Some(CursorFrame::Leaf {
        slices,
        idx,
        in_slice,
    }) = stack.last_mut()
    else {
        return stack;
    };
    *in_slice += bytes_consumed;
    if *in_slice < slices[*idx].range.len() {
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

fn advance_internal_past(mut stack: Vec<CursorFrame<'_>>) -> Vec<CursorFrame<'_>> {
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

/// Returns `true` if both cursors are at the exact start of an `Internal` child at the
/// same byte offset with `Arc::ptr_eq`, so the subtree can be skipped in O(1).
fn can_skip_shared_subtree(sa: &[CursorFrame<'_>], sb: &[CursorFrame<'_>]) -> bool {
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

fn entries_eq(a: &ChildEntry, b: &ChildEntry) -> bool {
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
            }),
            Some(CursorFrame::Leaf {
                slices: lb,
                idx: ib,
                in_slice: ob,
            }),
        ) = (sa.last(), sb.last())
        else {
            return false;
        };
        let sa_slice = &la[*ia];
        let sb_slice = &lb[*ib];
        let rem_a = sa_slice.range.len() - *oa;
        let rem_b = sb_slice.range.len() - *ob;
        let take = rem_a.min(rem_b);
        let bytes_a =
            &sa_slice.buf[sa_slice.range.start + *oa..sa_slice.range.start + *oa + take];
        let bytes_b =
            &sb_slice.buf[sb_slice.range.start + *ob..sb_slice.range.start + *ob + take];
        if bytes_a != bytes_b {
            return false;
        }
        sa = advance_cursor(sa, take);
        sb = advance_cursor(sb, take);
    }
}
