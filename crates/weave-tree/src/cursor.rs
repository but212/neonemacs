//! Bidirectional B-Tree spine cursor and standard Rust iterators (`Slices`, `Units`).

use alloc::vec::Vec;
use core::iter::FusedIterator;
use core::ops::Range;

use crate::utf8::{decode_unit_raw, prev_in_slice};
use crate::{ByteSlice, ChildEntry, DocError, Metric, SliceNode, Summary, Unit, WeaveTree};

#[derive(Clone, Debug)]
pub(crate) enum CursorFrame<'a, S: Summary> {
    Internal {
        children: &'a [ChildEntry<S>],
        idx: usize,
        base: usize,
    },
    Leaf {
        slices: &'a [ByteSlice<S>],
        idx: usize,
        in_slice: usize,
        slice_base: usize,
    },
}

/// Stateful bidirectional cursor over a [`WeaveTree`].
///
/// Supports $O(\log P)$ seeking by byte or [`Metric`] offset and $O(1)$ amortized
/// sequential traversal across adjacent slices and `Unit`s.
#[derive(Clone, Debug)]
pub struct Cursor<'a, S: Summary> {
    tree: &'a WeaveTree<S>,
    stack: Vec<CursorFrame<'a, S>>,
    byte_pos: usize,
}

impl<'a, S: Summary> Cursor<'a, S> {
    /// Creates a cursor positioned at byte offset `0`.
    #[must_use]
    pub fn new(tree: &'a WeaveTree<S>) -> Self {
        let mut c = Self {
            tree,
            stack: Vec::new(),
            byte_pos: 0,
        };
        if let Some(root) = tree.root.as_ref() {
            push_leftmost(&mut c.stack, root, 0);
        }
        c
    }

    /// Creates a cursor positioned at `byte_offset`, validating bounds and `Unit` boundary.
    pub fn at_byte(tree: &'a WeaveTree<S>, byte_offset: usize) -> Result<Self, DocError> {
        let mut c = Self {
            tree,
            stack: Vec::new(),
            byte_pos: 0,
        };
        c.seek_byte(byte_offset)?;
        Ok(c)
    }

    /// Creates a cursor positioned at `metric_offset` under `M`.
    pub fn at_metric<M: Metric<S>>(
        tree: &'a WeaveTree<S>,
        metric_offset: usize,
    ) -> Result<Self, DocError> {
        let byte_offset = tree.metric_to_byte::<M>(metric_offset)?;
        Self::at_byte(tree, byte_offset)
    }

    /// Current byte offset of the cursor (`0..=tree.total_bytes()`).
    #[must_use]
    #[inline]
    pub fn byte_offset(&self) -> usize {
        self.byte_pos
    }

    /// Current metric offset of the cursor under `M`.
    #[must_use]
    pub fn metric_offset<M: Metric<S>>(&self) -> usize {
        self.tree
            .byte_to_metric::<M>(self.byte_pos)
            .expect("cursor byte_pos is always a valid Unit boundary")
    }

    /// Moves the cursor to `byte_offset` in $O(\log P)$ time.
    pub fn seek_byte(&mut self, byte_offset: usize) -> Result<(), DocError> {
        let total = self.tree.total_bytes();
        if byte_offset > total {
            return Err(DocError::OutOfBounds {
                offset: byte_offset,
                len: 0,
                total,
            });
        }
        if !self.tree.is_boundary(byte_offset) {
            return Err(DocError::InvalidBoundary {
                offset: byte_offset,
            });
        }
        self.stack.clear();
        self.byte_pos = byte_offset;
        if byte_offset == total {
            return Ok(());
        }
        let Some(root) = self.tree.root.as_ref() else {
            return Ok(());
        };
        descend_to_byte(&mut self.stack, root, 0, byte_offset);
        Ok(())
    }

    /// Moves the cursor to `metric_offset` under `M` in $O(\log P)$ time.
    pub fn seek_metric<M: Metric<S>>(&mut self, metric_offset: usize) -> Result<(), DocError> {
        let byte_offset = self.tree.metric_to_byte::<M>(metric_offset)?;
        self.seek_byte(byte_offset)
    }

    /// Returns `(slice_bytes, offset_within_slice)` at the cursor, or `None` at EOF.
    #[must_use]
    pub fn current_slice(&self) -> Option<(&'a [u8], usize)> {
        let Some(CursorFrame::Leaf {
            slices,
            idx,
            in_slice,
            ..
        }) = self.stack.last()
        else {
            return None;
        };
        let slice = &slices[*idx];
        Some((&slice.buf[slice.range.clone()], *in_slice))
    }

    /// Returns the remaining bytes in the current slice and advances to the start of the next slice.
    pub fn next_slice(&mut self) -> Option<&'a [u8]> {
        let Some(CursorFrame::Leaf {
            slices,
            idx,
            in_slice,
            slice_base,
        }) = self.stack.last_mut()
        else {
            return None;
        };
        let slice = &slices[*idx];
        let rem = &slice.buf[slice.range.start + *in_slice..slice.range.end];
        self.byte_pos = *slice_base + slice.range.len();
        *idx += 1;
        *in_slice = 0;
        if *idx < slices.len() {
            *slice_base = self.byte_pos;
            return Some(rem);
        }
        self.stack.pop();
        advance_internal_past(&mut self.stack);
        Some(rem)
    }

    /// Moves backward to the previous slice boundary (or start of the current slice if mid-slice)
    /// and returns the bytes traversed.
    pub fn prev_slice(&mut self) -> Option<&'a [u8]> {
        if self.byte_pos == 0 {
            return None;
        }
        if self.stack.is_empty() {
            let root = self.tree.root.as_ref()?;
            push_rightmost(&mut self.stack, root, 0);
            if let Some(CursorFrame::Leaf {
                slices,
                idx,
                in_slice,
                slice_base,
            }) = self.stack.last_mut()
            {
                let slice = &slices[*idx];
                *in_slice = 0;
                self.byte_pos = *slice_base;
                return Some(&slice.buf[slice.range.clone()]);
            }
            return None;
        }

        let Some(CursorFrame::Leaf {
            slices,
            idx,
            in_slice,
            slice_base,
        }) = self.stack.last_mut()
        else {
            return None;
        };
        if *in_slice > 0 {
            let slice = &slices[*idx];
            let chunk = &slice.buf[slice.range.start..slice.range.start + *in_slice];
            *in_slice = 0;
            self.byte_pos = *slice_base;
            return Some(chunk);
        }
        if *idx > 0 {
            *idx -= 1;
            let slice = &slices[*idx];
            *slice_base -= slice.range.len();
            *in_slice = 0;
            self.byte_pos = *slice_base;
            return Some(&slice.buf[slice.range.clone()]);
        }
        self.stack.pop();
        if !retreat_internal_past(&mut self.stack) {
            self.byte_pos = 0;
            return None;
        }
        if let Some(CursorFrame::Leaf {
            slices,
            idx,
            in_slice,
            slice_base,
        }) = self.stack.last_mut()
        {
            let slice = &slices[*idx];
            *in_slice = 0;
            self.byte_pos = *slice_base;
            return Some(&slice.buf[slice.range.clone()]);
        }
        None
    }

    /// Decodes and consumes the next [`Unit`] at the cursor, or `None` at EOF.
    pub fn next_unit(&mut self) -> Option<Unit> {
        let Some(CursorFrame::Leaf {
            slices,
            idx,
            in_slice,
            slice_base,
        }) = self.stack.last_mut()
        else {
            return None;
        };
        let slice = &slices[*idx];
        let raw = &slice.buf[slice.range.clone()];
        let (unit, len) = decode_unit_raw(raw, *in_slice)?;
        *in_slice += len;
        self.byte_pos += len;
        if *in_slice < raw.len() {
            return Some(unit);
        }
        *idx += 1;
        *in_slice = 0;
        if *idx < slices.len() {
            *slice_base = self.byte_pos;
            return Some(unit);
        }
        self.stack.pop();
        advance_internal_past(&mut self.stack);
        Some(unit)
    }

    /// Moves backward by one [`Unit`] and returns it, or `None` at byte `0`.
    pub fn prev_unit(&mut self) -> Option<Unit> {
        if self.byte_pos == 0 {
            return None;
        }
        if self.stack.is_empty() {
            let root = self.tree.root.as_ref()?;
            push_rightmost(&mut self.stack, root, 0);
            if let Some(CursorFrame::Leaf {
                slices,
                idx,
                in_slice,
                ..
            }) = self.stack.last_mut()
            {
                *in_slice = slices[*idx].range.len();
            }
        } else if let Some(CursorFrame::Leaf { in_slice, .. }) = self.stack.last()
            && *in_slice == 0
        {
            let prev_len = self.step_to_prev_slice_end()?;
            let _ = prev_len;
        }

        let Some(CursorFrame::Leaf {
            slices,
            idx,
            in_slice,
            ..
        }) = self.stack.last_mut()
        else {
            return None;
        };
        let raw = &slices[*idx].buf[slices[*idx].range.clone()];
        let prev = prev_in_slice(raw, *in_slice);
        let (unit, len) = decode_unit_raw(raw, prev)?;
        *in_slice = prev;
        self.byte_pos -= len;
        Some(unit)
    }

    fn step_to_prev_slice_end(&mut self) -> Option<usize> {
        let Some(CursorFrame::Leaf {
            slices,
            idx,
            in_slice,
            slice_base,
        }) = self.stack.last_mut()
        else {
            return None;
        };
        if *idx > 0 {
            *idx -= 1;
            let len = slices[*idx].range.len();
            *slice_base -= len;
            *in_slice = len;
            return Some(len);
        }
        self.stack.pop();
        if !retreat_internal_past(&mut self.stack) {
            return None;
        }
        if let Some(CursorFrame::Leaf {
            slices,
            idx,
            in_slice,
            ..
        }) = self.stack.last_mut()
        {
            let len = slices[*idx].range.len();
            *in_slice = len;
            return Some(len);
        }
        None
    }
}

pub(crate) fn push_leftmost<'a, S: Summary>(
    stack: &mut Vec<CursorFrame<'a, S>>,
    entry: &'a ChildEntry<S>,
    base: usize,
) {
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
                    slice_base: cur_base,
                });
                return;
            }
        }
    }
}

fn push_rightmost<'a, S: Summary>(
    stack: &mut Vec<CursorFrame<'a, S>>,
    entry: &'a ChildEntry<S>,
    base: usize,
) {
    let mut cur = entry;
    let mut cur_base = base;
    loop {
        match cur.node.as_ref() {
            SliceNode::Internal { children } => {
                let last_idx = children.len() - 1;
                let last_base = cur_base + cur.bytes - children[last_idx].bytes;
                stack.push(CursorFrame::Internal {
                    children,
                    idx: last_idx,
                    base: last_base,
                });
                cur_base = last_base;
                cur = &children[last_idx];
            }
            SliceNode::Leaf { slices } => {
                let last_idx = slices.len() - 1;
                let last_base = cur_base + cur.bytes - slices[last_idx].range.len();
                stack.push(CursorFrame::Leaf {
                    slices,
                    idx: last_idx,
                    in_slice: 0,
                    slice_base: last_base,
                });
                return;
            }
        }
    }
}

fn descend_to_byte<'a, S: Summary>(
    stack: &mut Vec<CursorFrame<'a, S>>,
    entry: &'a ChildEntry<S>,
    base: usize,
    offset: usize,
) {
    let mut cur = entry;
    let mut cur_base = base;
    let mut rem = offset - base;
    loop {
        match cur.node.as_ref() {
            SliceNode::Internal { children } => {
                for (idx, child) in children.iter().enumerate() {
                    if rem < child.bytes {
                        stack.push(CursorFrame::Internal {
                            children,
                            idx,
                            base: cur_base,
                        });
                        cur = child;
                        break;
                    }
                    rem -= child.bytes;
                    cur_base += child.bytes;
                }
            }
            SliceNode::Leaf { slices } => {
                for (idx, slice) in slices.iter().enumerate() {
                    let len = slice.range.len();
                    if rem < len {
                        stack.push(CursorFrame::Leaf {
                            slices,
                            idx,
                            in_slice: rem,
                            slice_base: cur_base,
                        });
                        return;
                    }
                    rem -= len;
                    cur_base += len;
                }
                unreachable!("offset < total_bytes");
            }
        }
    }
}

pub(crate) fn advance_internal_past<S: Summary>(stack: &mut Vec<CursorFrame<'_, S>>) {
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
            push_leftmost(stack, next, nb);
            return;
        }
        stack.pop();
    }
}

fn retreat_internal_past<S: Summary>(stack: &mut Vec<CursorFrame<'_, S>>) -> bool {
    while let Some(CursorFrame::Internal {
        children,
        idx,
        base,
    }) = stack.last_mut()
    {
        if *idx > 0 {
            *idx -= 1;
            let b = children[*idx].bytes;
            *base -= b;
            let prev = &children[*idx];
            let pb = *base;
            push_rightmost(stack, prev, pb);
            return true;
        }
        stack.pop();
    }
    false
}

/// Zero-copy bidirectional iterator over `&'a [u8]` chunks of a [`WeaveTree`].
#[derive(Clone, Debug)]
pub struct Slices<'a, S: Summary> {
    front: Cursor<'a, S>,
    back: Cursor<'a, S>,
}

impl<'a, S: Summary> Slices<'a, S> {
    pub(crate) fn new(tree: &'a WeaveTree<S>, range: Range<usize>) -> Result<Self, DocError> {
        tree.validate_range(&range)?;
        Ok(Self {
            front: Cursor::at_byte(tree, range.start)?,
            back: Cursor::at_byte(tree, range.end)?,
        })
    }
}

impl<'a, S: Summary> Iterator for Slices<'a, S> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        let front_pos = self.front.byte_offset();
        let back_pos = self.back.byte_offset();
        if front_pos >= back_pos {
            return None;
        }
        let chunk = self.front.next_slice()?;
        let avail = back_pos - front_pos;
        if chunk.len() > avail {
            Some(&chunk[..avail])
        } else {
            Some(chunk)
        }
    }
}

impl<S: Summary> DoubleEndedIterator for Slices<'_, S> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let front_pos = self.front.byte_offset();
        let back_pos = self.back.byte_offset();
        if front_pos >= back_pos {
            return None;
        }
        let chunk = self.back.prev_slice()?;
        let avail = back_pos - front_pos;
        if chunk.len() > avail {
            Some(&chunk[chunk.len() - avail..])
        } else {
            Some(chunk)
        }
    }
}

impl<S: Summary> FusedIterator for Slices<'_, S> {}

/// Bidirectional iterator over [`Unit`]s (`Char(char)` or `Raw(u8)`) of a [`WeaveTree`].
#[derive(Clone, Debug)]
pub struct Units<'a, S: Summary> {
    front: Cursor<'a, S>,
    back: Cursor<'a, S>,
}

impl<'a, S: Summary> Units<'a, S> {
    pub(crate) fn new(tree: &'a WeaveTree<S>, range: Range<usize>) -> Result<Self, DocError> {
        tree.validate_range(&range)?;
        Ok(Self {
            front: Cursor::at_byte(tree, range.start)?,
            back: Cursor::at_byte(tree, range.end)?,
        })
    }
}

impl<S: Summary> Iterator for Units<'_, S> {
    type Item = Unit;

    fn next(&mut self) -> Option<Self::Item> {
        if self.front.byte_offset() >= self.back.byte_offset() {
            return None;
        }
        self.front.next_unit()
    }
}

impl<S: Summary> DoubleEndedIterator for Units<'_, S> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.front.byte_offset() >= self.back.byte_offset() {
            return None;
        }
        self.back.prev_unit()
    }
}

impl<S: Summary> FusedIterator for Units<'_, S> {}
