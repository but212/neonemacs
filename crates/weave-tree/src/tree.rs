//! Structural B-Tree operations (`from_slices`, `split_at`, `join`) preserving D-02 and D-03.

use alloc::vec;
use alloc::vec::Vec;
use triomphe::{AllocError, Arc};

use crate::{
    ByteSlice, CHUNK_CAP, ChildEntry, MAX_FANOUT, MAX_SLICE_BYTES, MIN_FANOUT, SliceNode, Summary,
};

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
        |s| (s.range.len(), s.buf.len(), &s.summary),
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
    let combined = a.range.len() + b.range.len();
    (Arc::ptr_eq(&a.buf, &b.buf) && a.range.end == b.range.start && combined <= MAX_SLICE_BYTES)
        || combined <= CHUNK_CAP
}

fn merge_two_slices<S: Summary>(a: ByteSlice<S>, b: ByteSlice<S>) -> ByteSlice<S> {
    let combined = a.range.len() + b.range.len();
    if Arc::ptr_eq(&a.buf, &b.buf) && a.range.end == b.range.start && combined <= MAX_SLICE_BYTES {
        return ByteSlice {
            buf: a.buf,
            range: a.range.start..b.range.end,
            summary: a.summary.combine(&b.summary),
        };
    }
    let mut buf = Vec::with_capacity(combined);
    buf.extend_from_slice(&a.buf[a.range]);
    buf.extend_from_slice(&b.buf[b.range]);
    ByteSlice {
        buf: Arc::new(buf),
        range: 0..combined,
        summary: a.summary.combine(&b.summary),
    }
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
    let split_pt = slice.range.start + local;
    let left_range = slice.range.start..split_pt;
    let right_range = split_pt..slice.range.end;
    let (ls, rs) = slice.summary.split_pair(
        &slice.buf[left_range.clone()],
        &slice.buf[right_range.clone()],
    );
    (
        ByteSlice {
            buf: Arc::clone(&slice.buf),
            range: left_range,
            summary: ls,
        },
        ByteSlice {
            buf: Arc::clone(&slice.buf),
            range: right_range,
            summary: rs,
        },
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
                    let len = slice.range.len();
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

fn mutate_internal_child<S: Summary, R>(
    mut entry: ChildEntry<S>,
    offset: usize,
    inclusive_end: bool,
    f: impl FnOnce(ChildEntry<S>, usize) -> Result<(ChildEntry<S>, R), ChildEntry<S>>,
) -> Result<(ChildEntry<S>, R), ChildEntry<S>> {
    let Some(SliceNode::Internal { children }) = Arc::get_mut(&mut entry.node) else {
        return Err(entry);
    };
    let mut acc = 0usize;
    let mut target_idx = None;
    for (idx, child) in children.iter().enumerate() {
        let hit = if inclusive_end {
            offset <= acc + child.bytes
        } else {
            offset < acc + child.bytes
        };
        if hit {
            target_idx = Some(idx);
            break;
        }
        acc += child.bytes;
    }
    let Some(idx) = target_idx else {
        return Err(entry);
    };
    let child = children.remove(idx);
    match f(child, offset - acc) {
        Ok((updated, out)) => {
            children.insert(idx, updated);
            let (total_bytes, total_backing, total_summary) =
                recompute_summary(children, |c| (c.bytes, c.backing_bytes, &c.summary));
            entry.bytes = total_bytes;
            entry.backing_bytes = total_backing;
            entry.summary = total_summary;
            Ok((entry, out))
        }
        Err(orig) => {
            children.insert(idx, orig);
            Err(entry)
        }
    }
}

fn with_refreshed_leaf_entry<S: Summary>(mut entry: ChildEntry<S>) -> ChildEntry<S> {
    if let SliceNode::Leaf { slices } = entry.node.as_ref() {
        let (total_bytes, total_backing, total_summary) =
            recompute_summary(slices, |s| (s.range.len(), s.buf.len(), &s.summary));
        entry.bytes = total_bytes;
        entry.backing_bytes = total_backing;
        entry.summary = total_summary;
    }
    entry
}

pub(crate) fn try_insert_in_place<S: Summary>(
    mut entry: ChildEntry<S>,
    offset: usize,
    bytes: &[u8],
) -> Result<ChildEntry<S>, ChildEntry<S>> {
    if matches!(entry.node.as_ref(), SliceNode::Internal { .. }) {
        return mutate_internal_child(entry, offset, true, |child, local_off| {
            try_insert_in_place(child, local_off, bytes).map(|updated| (updated, ()))
        })
        .map(|(updated, ())| updated);
    }
    let Some(SliceNode::Leaf { slices }) = Arc::get_mut(&mut entry.node) else {
        return Err(entry);
    };
    let mut acc = 0usize;
    for slice in slices.iter_mut() {
        let len = slice.range.len();
        if offset <= acc + len {
            let local = offset - acc;
            if local == len
                && len + bytes.len() <= MAX_SLICE_BYTES
                && let Some(buf) = Arc::get_mut(&mut slice.buf)
            {
                buf.truncate(slice.range.end);
                buf.extend_from_slice(bytes);
                slice.range.end += bytes.len();
                slice.summary = slice.summary.combine(&S::from_slice(bytes));
                return Ok(with_refreshed_leaf_entry(entry));
            }
            return Err(entry);
        }
        acc += len;
    }
    Err(entry)
}

pub(crate) fn try_delete_in_place<S: Summary>(
    mut entry: ChildEntry<S>,
    offset: usize,
    delete_len: usize,
) -> Result<(ChildEntry<S>, Vec<u8>), ChildEntry<S>> {
    if matches!(entry.node.as_ref(), SliceNode::Internal { .. }) {
        return mutate_internal_child(entry, offset, false, |child, local_off| {
            try_delete_in_place(child, local_off, delete_len)
        });
    }
    let Some(SliceNode::Leaf { slices }) = Arc::get_mut(&mut entry.node) else {
        return Err(entry);
    };
    let mut acc = 0usize;
    for slice in slices.iter_mut() {
        let len = slice.range.len();
        if offset < acc + len {
            let local_start = offset - acc;
            let local_end = local_start + delete_len;
            if local_end <= len && delete_len < len && (local_end == len || local_start == 0) {
                let abs_start = slice.range.start + local_start;
                let abs_end = slice.range.start + local_end;
                let deleted = slice.buf[abs_start..abs_end].to_vec();
                if local_end == len {
                    let kept_range = slice.range.start..abs_start;
                    let (kept_s, _) = slice.summary.split_pair(&slice.buf[kept_range], &deleted);
                    slice.range.end -= delete_len;
                    if let Some(buf) = Arc::get_mut(&mut slice.buf) {
                        buf.truncate(slice.range.end);
                    }
                    slice.summary = kept_s;
                } else {
                    let kept_range = abs_end..slice.range.end;
                    let (_, kept_s) = slice.summary.split_pair(&deleted, &slice.buf[kept_range]);
                    slice.range.start += delete_len;
                    slice.summary = kept_s;
                }
                return Ok((with_refreshed_leaf_entry(entry), deleted));
            }
            return Err(entry);
        }
        acc += len;
    }
    Err(entry)
}
