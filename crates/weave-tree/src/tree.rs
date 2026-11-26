//! Structural B-Tree operations and encapsulated node storage preserving D-02 and D-03.

use alloc::vec;
use alloc::vec::Vec;
use core::ops::{ControlFlow, Range};
use triomphe::{AllocError, Arc};

use crate::utf8::floor_in_slice;
use crate::{CHUNK_CAP, MAX_SLICE_BYTES, Summary};

/// B-Tree node fanout bounds.
const MAX_FANOUT: usize = 32;
const MIN_FANOUT: usize = MAX_FANOUT / 2;

#[derive(Clone, Debug)]
pub(crate) struct ByteSlice<S: Summary> {
    buf: Arc<Vec<u8>>,
    summary: S,
    start: u16,
    end: u16,
}

const _: () = assert!(core::mem::size_of::<ByteSlice<crate::LineBreaks>>() <= 24);

impl<S: Summary> ByteSlice<S> {
    fn new(buf: Arc<Vec<u8>>, range: Range<usize>, summary: S) -> Self {
        Self {
            buf,
            summary,
            start: u16::try_from(range.start).expect("slice start fits in u16"),
            end: u16::try_from(range.end).expect("slice end fits in u16"),
        }
    }

    fn range(&self) -> Range<usize> {
        usize::from(self.start)..usize::from(self.end)
    }

    fn len(&self) -> usize {
        usize::from(self.end - self.start)
    }

    fn bytes(&self) -> &[u8] {
        &self.buf[self.range()]
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ChildEntry<S: Summary> {
    node: Arc<SliceNode<S>>,
    bytes: usize,
    backing_bytes: usize,
    summary: S,
}

impl<S: Summary> ChildEntry<S> {
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn backing_bytes(&self) -> usize {
        self.backing_bytes
    }

    pub(crate) fn summary_measure(&self) -> usize {
        self.summary.measure()
    }

    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.node, &other.node)
    }
}

#[derive(Clone, Debug)]
enum SliceNode<S: Summary> {
    Internal { children: Vec<ChildEntry<S>> },
    Leaf { slices: Vec<ByteSlice<S>> },
}

pub(crate) fn try_append_slices<S: Summary>(
    mut slices: Vec<ByteSlice<S>>,
    bytes: &[u8],
) -> Result<Vec<ByteSlice<S>>, AllocError> {
    let mut start = 0;
    while start < bytes.len() {
        let max_end = (start + MAX_SLICE_BYTES).min(bytes.len());
        let end = if max_end == bytes.len() {
            max_end
        } else {
            let local_end = floor_in_slice(&bytes[start..], max_end - start);
            // A single Unit is at most 4 bytes and MAX_SLICE_BYTES >= 4, so local_end > 0.
            start + local_end.max(1)
        };
        let chunk = &bytes[start..end];
        slices.try_reserve(1).map_err(|_| AllocError)?;
        let mut v = Vec::new();
        let cap = if chunk.len() < CHUNK_CAP {
            (chunk.len() + 128).min(MAX_SLICE_BYTES)
        } else {
            chunk.len()
        };
        v.try_reserve_exact(cap).map_err(|_| AllocError)?;
        v.extend_from_slice(chunk);
        let buf = Arc::try_new(v)?;
        let summary = S::from_slice(chunk);
        slices.push(ByteSlice::new(buf, 0..chunk.len(), summary));
        start = end;
    }
    Ok(slices)
}

fn try_summarize_node<T, S: Summary>(
    items: Vec<T>,
    metrics: impl Fn(&T) -> (usize, usize, &S),
    wrap: impl FnOnce(Vec<T>) -> SliceNode<S>,
) -> Result<ChildEntry<S>, AllocError> {
    let (bytes, backing_bytes, summary) = recompute_summary(&items, metrics);
    Ok(ChildEntry {
        node: Arc::try_new(wrap(items))?,
        bytes,
        backing_bytes,
        summary,
    })
}

fn try_summarize_leaf<S: Summary>(slices: Vec<ByteSlice<S>>) -> Result<ChildEntry<S>, AllocError> {
    try_summarize_node(
        slices,
        |s| (s.len(), s.buf.len(), &s.summary),
        |slices| SliceNode::Leaf { slices },
    )
}

fn try_summarize_internal<S: Summary>(
    children: Vec<ChildEntry<S>>,
) -> Result<ChildEntry<S>, AllocError> {
    try_summarize_node(
        children,
        |c| (c.bytes, c.backing_bytes, &c.summary),
        |children| SliceNode::Internal { children },
    )
}

#[must_use]
pub(crate) fn summarize_leaf<S: Summary>(slices: Vec<ByteSlice<S>>) -> ChildEntry<S> {
    try_summarize_leaf(slices).expect("cannot allocate document leaf")
}

#[must_use]
pub(crate) fn summarize_internal<S: Summary>(children: Vec<ChildEntry<S>>) -> ChildEntry<S> {
    try_summarize_internal(children).expect("cannot allocate document node")
}

#[must_use]
pub(crate) fn entry_height<S: Summary>(entry: &ChildEntry<S>) -> usize {
    let mut cur = entry;
    let mut h = 1;
    while let SliceNode::Internal { children } = cur.node.as_ref() {
        h += 1;
        cur = &children[0];
    }
    h
}

fn try_pack_level<T, S: Summary>(
    items: Vec<T>,
    summarize: impl Fn(Vec<T>) -> Result<ChildEntry<S>, AllocError>,
) -> Result<Vec<ChildEntry<S>>, AllocError> {
    let mut items = items.into_iter();
    let mut level = Vec::new();
    level
        .try_reserve_exact(items.len().div_ceil(MAX_FANOUT))
        .map_err(|_| AllocError)?;
    while items.len() > 0 {
        let remaining = items.len();
        let size = if (MAX_FANOUT + 1..MAX_FANOUT + MIN_FANOUT).contains(&remaining) {
            remaining / 2
        } else {
            remaining.min(MAX_FANOUT)
        };
        let mut group = Vec::new();
        group.try_reserve_exact(size).map_err(|_| AllocError)?;
        group.extend(items.by_ref().take(size));
        level.push(summarize(group)?);
    }
    Ok(level)
}

pub(crate) fn try_from_slices<S: Summary>(
    slices: Vec<ByteSlice<S>>,
) -> Result<Option<ChildEntry<S>>, AllocError> {
    let mut level = try_pack_level(slices, try_summarize_leaf)?;
    while level.len() > 1 {
        level = try_pack_level(level, try_summarize_internal)?;
    }
    Ok(level.pop())
}

fn from_children_list<S: Summary>(mut children: Vec<ChildEntry<S>>) -> Option<ChildEntry<S>> {
    match children.len() {
        0 => None,
        1 => children.pop(),
        _ => Some(summarize_internal(children)),
    }
}

fn non_empty_leaf<S: Summary>(slices: Vec<ByteSlice<S>>) -> Option<ChildEntry<S>> {
    if slices.is_empty() {
        None
    } else {
        Some(summarize_leaf(slices))
    }
}

fn can_merge_slices<S: Summary>(a: &ByteSlice<S>, b: &ByteSlice<S>) -> bool {
    let combined = a.len() + b.len();
    (Arc::ptr_eq(&a.buf, &b.buf) && a.end == b.start && combined <= MAX_SLICE_BYTES)
        || combined <= CHUNK_CAP
}

fn merge_two_slices<S: Summary>(a: ByteSlice<S>, b: ByteSlice<S>) -> ByteSlice<S> {
    let combined = a.len() + b.len();
    if Arc::ptr_eq(&a.buf, &b.buf) && a.end == b.start && combined <= MAX_SLICE_BYTES {
        return ByteSlice {
            buf: a.buf,
            summary: a.summary.combine(&b.summary),
            start: a.start,
            end: b.end,
        };
    }
    let mut buf = Vec::with_capacity(combined);
    buf.extend_from_slice(a.bytes());
    buf.extend_from_slice(b.bytes());
    ByteSlice::new(Arc::new(buf), 0..combined, a.summary.combine(&b.summary))
}

fn edge_slice<S: Summary>(entry: &ChildEntry<S>, from_right: bool) -> &ByteSlice<S> {
    let mut cur = entry;
    loop {
        match cur.node.as_ref() {
            SliceNode::Leaf { slices } => {
                return if from_right {
                    slices.last().expect("non-empty leaf")
                } else {
                    &slices[0]
                };
            }
            SliceNode::Internal { children } => {
                cur = if from_right {
                    children.last().expect("non-empty internal")
                } else {
                    &children[0]
                };
            }
        }
    }
}

fn child_len<S: Summary>(entry: &ChildEntry<S>) -> usize {
    match entry.node.as_ref() {
        SliceNode::Leaf { slices } => slices.len(),
        SliceNode::Internal { children } => children.len(),
    }
}

fn rebalance_pair<T: Clone, S: Summary>(
    left: &[T],
    right: &[T],
    steal_from_left: bool,
    summarize: fn(Vec<T>) -> ChildEntry<S>,
    mut out: Vec<ChildEntry<S>>,
) -> Vec<ChildEntry<S>> {
    if steal_from_left && left.len() > MIN_FANOUT {
        let mut new_l = left.to_vec();
        let mut new_r = Vec::with_capacity(right.len() + 1);
        new_r.push(new_l.pop().expect("left > MIN_FANOUT"));
        new_r.extend_from_slice(right);
        out.push(summarize(new_l));
        out.push(summarize(new_r));
    } else if !steal_from_left && right.len() > MIN_FANOUT {
        let mut new_l = left.to_vec();
        let mut new_r = right.to_vec();
        new_l.push(new_r.remove(0));
        out.push(summarize(new_l));
        out.push(summarize(new_r));
    } else {
        let mut merged = left.to_vec();
        merged.extend_from_slice(right);
        out.push(summarize(merged));
    }
    out
}

fn rebalance_adjacent<S: Summary>(
    left: &ChildEntry<S>,
    right: &ChildEntry<S>,
    steal_from_left: bool,
    out: Vec<ChildEntry<S>>,
) -> Vec<ChildEntry<S>> {
    match (left.node.as_ref(), right.node.as_ref()) {
        (SliceNode::Leaf { slices: l }, SliceNode::Leaf { slices: r }) => {
            rebalance_pair(l, r, steal_from_left, summarize_leaf, out)
        }
        (SliceNode::Internal { children: l }, SliceNode::Internal { children: r }) => {
            rebalance_pair(l, r, steal_from_left, summarize_internal, out)
        }
        _ => unreachable!("D-02 uniform leaf depth"),
    }
}

fn rebalance_edge_child<S: Summary>(
    mut children: Vec<ChildEntry<S>>,
    edge: Option<ChildEntry<S>>,
    from_right: bool,
) -> Vec<ChildEntry<S>> {
    let Some(edge_entry) = edge else {
        return children;
    };
    if child_len(&edge_entry) >= MIN_FANOUT || children.is_empty() {
        if from_right {
            children.push(edge_entry);
            return children;
        }
        let mut out = Vec::with_capacity(children.len() + 1);
        out.push(edge_entry);
        out.extend(children);
        return out;
    }
    if from_right {
        let prev = children.pop().expect("non-empty children");
        rebalance_adjacent(&prev, &edge_entry, true, children)
    } else {
        let next = children.remove(0);
        let out = Vec::with_capacity(children.len() + 2);
        let mut out = rebalance_adjacent(&edge_entry, &next, false, out);
        out.extend(children);
        out
    }
}

fn pop_edge_slice<S: Summary>(
    entry: &ChildEntry<S>,
    from_right: bool,
) -> (Option<ChildEntry<S>>, ByteSlice<S>) {
    match entry.node.as_ref() {
        SliceNode::Leaf { slices } => {
            let mut rest = slices.clone();
            let slice = if from_right {
                rest.pop().expect("non-empty leaf")
            } else {
                rest.remove(0)
            };
            (non_empty_leaf(rest), slice)
        }
        SliceNode::Internal { children } => {
            let mut rest = children.clone();
            let child = if from_right {
                rest.pop().expect("non-empty internal")
            } else {
                rest.remove(0)
            };
            let (new_edge, popped) = pop_edge_slice(&child, from_right);
            let balanced = rebalance_edge_child(rest, new_edge, from_right);
            (from_children_list(balanced), popped)
        }
    }
}

fn split_or_single<T, S: Summary>(
    mut items: Vec<T>,
    summarize: impl Fn(Vec<T>) -> ChildEntry<S>,
) -> Vec<ChildEntry<S>> {
    if items.len() <= MAX_FANOUT {
        vec![summarize(items)]
    } else {
        let mid = items.len() / 2;
        let right = items.split_off(mid);
        vec![summarize(items), summarize(right)]
    }
}

fn join_at_height<S: Summary>(
    l: ChildEntry<S>,
    r: ChildEntry<S>,
    hl: usize,
    hr: usize,
) -> Vec<ChildEntry<S>> {
    if hl == hr {
        match (l.node.as_ref(), r.node.as_ref()) {
            (SliceNode::Leaf { slices: ls }, SliceNode::Leaf { slices: rs }) => {
                let mut all = Vec::with_capacity(ls.len() + rs.len());
                all.extend_from_slice(ls);
                all.extend_from_slice(rs);
                split_or_single(all, summarize_leaf)
            }
            (SliceNode::Internal { children: lc }, SliceNode::Internal { children: rc }) => {
                let mut all = Vec::with_capacity(lc.len() + rc.len());
                all.extend_from_slice(lc);
                all.extend_from_slice(rc);
                split_or_single(all, summarize_internal)
            }
            _ => unreachable!("hl == hr guarantees matching node variants"),
        }
    } else if hl > hr {
        let SliceNode::Internal { children } = l.node.as_ref() else {
            unreachable!("hl > hr guarantees internal node");
        };
        let mut all = children.clone();
        let last = all.pop().expect("non-empty internal");
        all.extend(join_at_height(last, r, hl - 1, hr));
        split_or_single(all, summarize_internal)
    } else {
        let SliceNode::Internal { children } = r.node.as_ref() else {
            unreachable!("hl < hr guarantees internal node");
        };
        let mut all = children.clone();
        let first = all.remove(0);
        let mid = join_at_height(l, first, hl, hr - 1);
        let mut combined = Vec::with_capacity(mid.len() + all.len());
        combined.extend(mid);
        combined.extend(all);
        split_or_single(combined, summarize_internal)
    }
}

fn join_trees_raw<S: Summary>(
    left: Option<ChildEntry<S>>,
    right: Option<ChildEntry<S>>,
) -> Option<ChildEntry<S>> {
    match (left, right) {
        (None, r) => r,
        (l, None) => l,
        (Some(l), Some(r)) => {
            let hl = entry_height(&l);
            let hr = entry_height(&r);
            let mut res = join_at_height(l, r, hl, hr);
            if res.len() == 1 {
                res.pop()
            } else {
                Some(summarize_internal(res))
            }
        }
    }
}

#[must_use]
pub(crate) fn join<S: Summary>(
    left: Option<ChildEntry<S>>,
    right: Option<ChildEntry<S>>,
) -> Option<ChildEntry<S>> {
    let (l, r) = match (left, right) {
        (Some(l), Some(r)) => (l, r),
        (l, None) => return l,
        (None, r) => return r,
    };
    if can_merge_slices(edge_slice(&l, true), edge_slice(&r, false)) {
        let (l_rest, a) = pop_edge_slice(&l, true);
        let (r_rest, b) = pop_edge_slice(&r, false);
        let merged = summarize_leaf(vec![merge_two_slices(a, b)]);
        return join_trees_raw(l_rest, join_trees_raw(Some(merged), r_rest));
    }
    join_trees_raw(Some(l), Some(r))
}

fn split_byte_slice<S: Summary>(
    slice: &ByteSlice<S>,
    local: usize,
) -> (ByteSlice<S>, ByteSlice<S>) {
    let start = usize::from(slice.start);
    let end = usize::from(slice.end);
    let split_pt = start + local;
    let left_range = start..split_pt;
    let right_range = split_pt..end;
    let (ls, rs) = slice.summary.split_pair(
        &slice.buf[left_range.clone()],
        &slice.buf[right_range.clone()],
    );
    (
        ByteSlice::new(Arc::clone(&slice.buf), left_range, ls),
        ByteSlice::new(Arc::clone(&slice.buf), right_range, rs),
    )
}

fn assemble_split_side<S: Summary>(
    mut levels: Vec<Vec<ChildEntry<S>>>,
    leaf: Option<ChildEntry<S>>,
    start_child_h: usize,
    is_left: bool,
) -> Option<ChildEntry<S>> {
    let mut carry: Option<(ChildEntry<S>, usize)> = leaf.map(|e| (e, 1));
    let mut child_h = start_child_h;
    while let Some(mut level_children) = levels.pop() {
        if !level_children.is_empty() {
            carry = match carry {
                None => {
                    let len = level_children.len();
                    from_children_list(level_children).map(|e| {
                        let h = if len == 1 { child_h } else { child_h + 1 };
                        (e, h)
                    })
                }
                Some((c, ch)) => {
                    let combined = if is_left {
                        let last = level_children.pop().expect("non-empty level_children");
                        let joined = join_at_height(last, c, child_h, ch);
                        level_children.extend(joined);
                        level_children
                    } else {
                        let first = level_children.remove(0);
                        let mut joined = join_at_height(c, first, ch, child_h);
                        joined.extend(level_children);
                        joined
                    };
                    let len = combined.len();
                    let e = from_children_list(combined).expect("non-empty combined");
                    let h = if len == 1 { child_h } else { child_h + 1 };
                    Some((e, h))
                }
            };
        }
        child_h += 1;
    }
    carry.map(|(e, _)| e)
}

#[must_use]
pub(crate) fn split_at<S: Summary>(
    root: Option<ChildEntry<S>>,
    pos: usize,
) -> (Option<ChildEntry<S>>, Option<ChildEntry<S>>) {
    let Some(entry) = root else {
        return (None, None);
    };
    if pos == 0 {
        return (None, Some(entry));
    }
    if pos >= entry.bytes {
        return (Some(entry), None);
    }

    let mut cur = entry;
    let mut rem = pos;
    let mut left_levels: Vec<Vec<ChildEntry<S>>> = Vec::new();
    let mut right_levels: Vec<Vec<ChildEntry<S>>> = Vec::new();
    let mut left_leaf: Option<ChildEntry<S>> = None;
    let mut right_leaf: Option<ChildEntry<S>> = None;
    let mut start_child_h = 1usize;

    loop {
        match cur.node.as_ref() {
            SliceNode::Leaf { slices } => {
                let mut acc = 0usize;
                for (idx, slice) in slices.iter().enumerate() {
                    let len = slice.len();
                    if rem == acc {
                        left_leaf = non_empty_leaf(slices[..idx].to_vec());
                        right_leaf = non_empty_leaf(slices[idx..].to_vec());
                        break;
                    }
                    if rem < acc + len {
                        let (ls, rs) = split_byte_slice(slice, rem - acc);
                        let mut left = slices[..idx].to_vec();
                        left.push(ls);
                        let mut right = Vec::with_capacity(slices.len() - idx);
                        right.push(rs);
                        right.extend_from_slice(&slices[idx + 1..]);
                        left_leaf = non_empty_leaf(left);
                        right_leaf = non_empty_leaf(right);
                        break;
                    }
                    acc += len;
                }
                break;
            }
            SliceNode::Internal { children } => {
                let mut acc = 0usize;
                let mut next_child = None;
                for (idx, child) in children.iter().enumerate() {
                    if rem == acc {
                        left_levels.push(children[..idx].to_vec());
                        right_levels.push(children[idx..].to_vec());
                        start_child_h = entry_height(child);
                        break;
                    }
                    if rem < acc + child.bytes {
                        left_levels.push(children[..idx].to_vec());
                        right_levels.push(children[idx + 1..].to_vec());
                        rem -= acc;
                        next_child = Some(child.clone());
                        break;
                    }
                    acc += child.bytes;
                }
                match next_child {
                    Some(c) => cur = c,
                    None => break,
                }
            }
        }
    }

    (
        assemble_split_side(left_levels, left_leaf, start_child_h, true),
        assemble_split_side(right_levels, right_leaf, start_child_h, false),
    )
}

fn recompute_summary<T, S: Summary>(
    items: &[T],
    metrics: impl Fn(&T) -> (usize, usize, &S),
) -> (usize, usize, S) {
    let mut bytes = 0usize;
    let mut backing_bytes = 0usize;
    let mut summary = S::default();
    for item in items {
        let (b, bb, s) = metrics(item);
        bytes += b;
        backing_bytes = backing_bytes.saturating_add(bb);
        summary = summary.combine(s);
    }
    (bytes, backing_bytes, summary)
}

fn mutate_vec_item<T, R>(
    mut items: Vec<T>,
    offset: usize,
    inclusive_end: bool,
    item_len: impl Fn(&T) -> usize,
    f: impl FnOnce(T, usize) -> Result<(T, R), T>,
) -> Result<(Vec<T>, R), Vec<T>> {
    let mut acc = 0usize;
    let Some(target_idx) = items.iter().position(|item| {
        let len = item_len(item);
        let hit = if inclusive_end {
            offset <= acc + len
        } else {
            offset < acc + len
        };
        if !hit {
            acc += len;
        }
        hit
    }) else {
        return Err(items);
    };
    let item = items.remove(target_idx);
    match f(item, offset - acc) {
        Ok((updated, out)) => {
            items.insert(target_idx, updated);
            Ok((items, out))
        }
        Err(orig) => {
            items.insert(target_idx, orig);
            Err(items)
        }
    }
}

fn mutate_internal_child<S: Summary, R>(
    mut entry: ChildEntry<S>,
    offset: usize,
    inclusive_end: bool,
    f: impl FnOnce(ChildEntry<S>, usize) -> Result<(ChildEntry<S>, R), ChildEntry<S>>,
) -> Result<(ChildEntry<S>, R), ChildEntry<S>> {
    let Some(SliceNode::Internal { children }) = Arc::get_mut(&mut entry.node) else {
        return Err(entry);
    };
    let taken = core::mem::take(children);
    match mutate_vec_item(taken, offset, inclusive_end, |c| c.bytes, f) {
        Ok((updated, out)) => {
            *children = updated;
            let (total_bytes, total_backing, total_summary) =
                recompute_summary(children, |c| (c.bytes, c.backing_bytes, &c.summary));
            entry.bytes = total_bytes;
            entry.backing_bytes = total_backing;
            entry.summary = total_summary;
            Ok((entry, out))
        }
        Err(orig) => {
            *children = orig;
            Err(entry)
        }
    }
}

fn with_refreshed_leaf_entry<S: Summary>(mut entry: ChildEntry<S>) -> ChildEntry<S> {
    if let SliceNode::Leaf { slices } = entry.node.as_ref() {
        let (total_bytes, total_backing, total_summary) =
            recompute_summary(slices, |s| (s.len(), s.buf.len(), &s.summary));
        entry.bytes = total_bytes;
        entry.backing_bytes = total_backing;
        entry.summary = total_summary;
    }
    entry
}

fn mutate_leaf_slice<S: Summary, R>(
    mut entry: ChildEntry<S>,
    offset: usize,
    inclusive_end: bool,
    f: impl FnOnce(ByteSlice<S>, usize) -> Result<(ByteSlice<S>, R), ByteSlice<S>>,
) -> Result<(ChildEntry<S>, R), ChildEntry<S>> {
    let Some(SliceNode::Leaf { slices }) = Arc::get_mut(&mut entry.node) else {
        return Err(entry);
    };
    let taken = core::mem::take(slices);
    match mutate_vec_item(taken, offset, inclusive_end, ByteSlice::len, f) {
        Ok((updated, out)) => {
            *slices = updated;
            Ok((with_refreshed_leaf_entry(entry), out))
        }
        Err(orig) => {
            *slices = orig;
            Err(entry)
        }
    }
}

pub(crate) fn try_insert_in_place<S: Summary>(
    entry: ChildEntry<S>,
    offset: usize,
    bytes: &[u8],
) -> Result<ChildEntry<S>, ChildEntry<S>> {
    if matches!(entry.node.as_ref(), SliceNode::Internal { .. }) {
        return mutate_internal_child(entry, offset, true, |child, local_off| {
            try_insert_in_place(child, local_off, bytes).map(|updated| (updated, ()))
        })
        .map(|(updated, ())| updated);
    }
    mutate_leaf_slice(entry, offset, true, |mut slice, local| {
        if local == slice.len()
            && usize::from(slice.end) + bytes.len() <= MAX_SLICE_BYTES
            && let Some(buf) = Arc::get_mut(&mut slice.buf)
        {
            buf.truncate(usize::from(slice.end));
            buf.extend_from_slice(bytes);
            slice.end += u16::try_from(bytes.len()).expect("slice end fits in u16");
            slice.summary = slice.summary.combine(&S::from_slice(bytes));
            Ok((slice, ()))
        } else {
            Err(slice)
        }
    })
    .map(|(updated, ())| updated)
}

pub(crate) fn try_delete_in_place<S: Summary>(
    entry: ChildEntry<S>,
    offset: usize,
    delete_len: usize,
) -> Result<(ChildEntry<S>, Vec<u8>), ChildEntry<S>> {
    if matches!(entry.node.as_ref(), SliceNode::Internal { .. }) {
        return mutate_internal_child(entry, offset, false, |child, local_off| {
            try_delete_in_place(child, local_off, delete_len)
        });
    }
    mutate_leaf_slice(entry, offset, false, |mut slice, local_start| {
        let len = slice.len();
        let local_end = local_start + delete_len;
        if local_end <= len && delete_len < len && (local_end == len || local_start == 0) {
            let start = usize::from(slice.start);
            let end = usize::from(slice.end);
            let abs_start = start + local_start;
            let abs_end = start + local_end;
            let deleted = slice.buf[abs_start..abs_end].to_vec();
            let delta = u16::try_from(delete_len).expect("delete_len fits in u16");
            if local_end == len {
                let kept_range = start..abs_start;
                let (kept_s, _) = slice.summary.split_pair(&slice.buf[kept_range], &deleted);
                slice.end -= delta;
                if let Some(buf) = Arc::get_mut(&mut slice.buf) {
                    buf.truncate(usize::from(slice.end));
                }
                slice.summary = kept_s;
            } else {
                let kept_range = abs_end..end;
                let (_, kept_s) = slice.summary.split_pair(&deleted, &slice.buf[kept_range]);
                slice.start += delta;
                slice.summary = kept_s;
            }
            Ok((slice, deleted))
        } else {
            Err(slice)
        }
    })
}

fn descend_to_byte<S: Summary>(
    root: &ChildEntry<S>,
    offset: usize,
) -> (&ByteSlice<S>, usize, usize) {
    let mut cur = root;
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

pub(crate) fn locate_slice_bytes<S: Summary>(
    root: &ChildEntry<S>,
    offset: usize,
) -> (&[u8], usize) {
    let (slice, base, _) = descend_to_byte(root, offset);
    (slice.bytes(), base)
}

pub(crate) fn metric_to_byte<S: Summary>(root: &ChildEntry<S>, pos: usize) -> usize {
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
                        return byte_acc + S::to_byte_offset(slice.bytes(), rem);
                    }
                    rem -= sm;
                    byte_acc += slice.len();
                }
                unreachable!("Summary invariant guarantees match");
            }
        }
    }
}

pub(crate) fn byte_to_metric<S: Summary>(root: &ChildEntry<S>, offset: usize) -> usize {
    let (slice, slice_start, metric_acc) = descend_to_byte(root, offset);
    metric_acc + S::from_byte_offset(slice.bytes(), offset - slice_start)
}

fn visit_dir_items<const REV: bool, T, B, St>(
    items: &[T],
    base: usize,
    total_bytes: usize,
    range: &Range<usize>,
    mut state: St,
    item_len: impl Fn(&T) -> usize,
    mut visit: impl FnMut(&T, usize, usize, St) -> (ControlFlow<B>, St),
) -> (ControlFlow<B>, St) {
    if REV {
        let mut cur_end = base + total_bytes;
        for item in items.iter().rev() {
            let item_start = cur_end - item_len(item);
            if cur_end > range.start && item_start < range.end {
                let (flow, next_state) = visit(item, item_start, cur_end, state);
                state = next_state;
                if let ControlFlow::Break(b) = flow {
                    return (ControlFlow::Break(b), state);
                }
            }
            cur_end = item_start;
            if cur_end <= range.start {
                break;
            }
        }
    } else {
        let mut cur_base = base;
        for item in items {
            let item_end = cur_base + item_len(item);
            if item_end > range.start && cur_base < range.end {
                let (flow, next_state) = visit(item, cur_base, item_end, state);
                state = next_state;
                if let ControlFlow::Break(b) = flow {
                    return (ControlFlow::Break(b), state);
                }
            }
            cur_base = item_end;
            if cur_base >= range.end {
                break;
            }
        }
    }
    (ControlFlow::Continue(()), state)
}

pub(crate) fn visit_slices<const REV: bool, B, S: Summary, F: FnMut(&[u8]) -> ControlFlow<B>>(
    entry: &ChildEntry<S>,
    base: usize,
    range: &Range<usize>,
    f: F,
) -> (ControlFlow<B>, F) {
    match entry.node.as_ref() {
        SliceNode::Internal { children } => visit_dir_items::<REV, _, B, F>(
            children,
            base,
            entry.bytes,
            range,
            f,
            |c| c.bytes,
            |child, child_start, _, next_f| {
                visit_slices::<REV, B, S, F>(child, child_start, range, next_f)
            },
        ),
        SliceNode::Leaf { slices } => visit_dir_items::<REV, _, B, F>(
            slices,
            base,
            entry.bytes,
            range,
            f,
            ByteSlice::len,
            |slice, slice_start, slice_end, mut next_f| {
                let lo = range.start.saturating_sub(slice_start);
                let hi = (range.end - slice_start).min(slice_end - slice_start);
                let flow = next_f(&slice.bytes()[lo..hi]);
                (flow, next_f)
            },
        ),
    }
}

fn assert_entry_totals<S: Summary>(
    entry: &ChildEntry<S>,
    bytes: usize,
    backing: usize,
    summary: S,
) {
    assert_eq!(entry.bytes, bytes);
    assert_eq!(entry.backing_bytes, backing);
    assert!(entry.summary == summary);
}

pub(crate) fn check_entry<S: Summary>(entry: &ChildEntry<S>, is_root: bool) -> usize {
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
            assert_entry_totals(entry, sum_bytes, sum_backing, sum_summary);
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
                let d = check_entry(child, false);
                match expected_depth {
                    None => expected_depth = Some(d),
                    Some(ed) => assert_eq!(d, ed, "D-02 uniform leaf depth violated"),
                }
                sum_bytes += child.bytes;
                sum_backing = sum_backing.saturating_add(child.backing_bytes);
                sum_summary = sum_summary.combine(&child.summary);
            }
            assert_entry_totals(entry, sum_bytes, sum_backing, sum_summary);
            expected_depth.expect("non-empty internal") + 1
        }
    }
}

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

pub(crate) fn entries_eq<S: Summary>(a: &ChildEntry<S>, b: &ChildEntry<S>) -> bool {
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
