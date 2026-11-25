//! Structural B-Tree operations (`from_slices`, `split_at`, `join`) preserving D-02 and D-03.

use alloc::vec;
use alloc::vec::Vec;
use triomphe::{AllocError, Arc};

use crate::{
    ByteSlice, CHUNK_CAP, ChildEntry, MAX_FANOUT, MAX_SLICE_BYTES, MIN_FANOUT, SliceNode, Summary,
};

#[must_use]
pub(crate) fn summarize_leaf<S: Summary>(slices: Vec<ByteSlice<S>>) -> ChildEntry<S> {
    try_summarize_leaf(slices).expect("cannot allocate document leaf")
}

fn try_summarize_leaf<S: Summary>(slices: Vec<ByteSlice<S>>) -> Result<ChildEntry<S>, AllocError> {
    let (bytes, backing_bytes, summary) = recompute_leaf_summary(&slices);
    Ok(ChildEntry {
        node: Arc::try_new(SliceNode::Leaf { slices })?,
        bytes,
        backing_bytes,
        summary,
    })
}

#[must_use]
pub(crate) fn summarize_internal<S: Summary>(children: Vec<ChildEntry<S>>) -> ChildEntry<S> {
    try_summarize_internal(children).expect("cannot allocate document node")
}

fn try_summarize_internal<S: Summary>(
    children: Vec<ChildEntry<S>>,
) -> Result<ChildEntry<S>, AllocError> {
    let (bytes, backing_bytes, summary) = recompute_internal_summary(&children);
    Ok(ChildEntry {
        node: Arc::try_new(SliceNode::Internal { children })?,
        bytes,
        backing_bytes,
        summary,
    })
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

fn rightmost_slice<S: Summary>(entry: &ChildEntry<S>) -> &ByteSlice<S> {
    let mut cur = entry;
    loop {
        match cur.node.as_ref() {
            SliceNode::Leaf { slices } => return slices.last().expect("non-empty leaf"),
            SliceNode::Internal { children } => cur = children.last().expect("non-empty internal"),
        }
    }
}

fn leftmost_slice<S: Summary>(entry: &ChildEntry<S>) -> &ByteSlice<S> {
    let mut cur = entry;
    loop {
        match cur.node.as_ref() {
            SliceNode::Leaf { slices } => return &slices[0],
            SliceNode::Internal { children } => cur = &children[0],
        }
    }
}

fn child_len<S: Summary>(entry: &ChildEntry<S>) -> usize {
    match entry.node.as_ref() {
        SliceNode::Leaf { slices } => slices.len(),
        SliceNode::Internal { children } => children.len(),
    }
}

fn rebalance_last_child<S: Summary>(
    mut children: Vec<ChildEntry<S>>,
    last: Option<ChildEntry<S>>,
) -> Vec<ChildEntry<S>> {
    let Some(last_entry) = last else {
        return children;
    };
    if child_len(&last_entry) >= MIN_FANOUT || children.is_empty() {
        children.push(last_entry);
        return children;
    }
    let prev = children.pop().expect("non-empty children");
    match (prev.node.as_ref(), last_entry.node.as_ref()) {
        (SliceNode::Leaf { slices: p_slices }, SliceNode::Leaf { slices: l_slices }) => {
            if p_slices.len() > MIN_FANOUT {
                let mut new_p = p_slices.clone();
                let mut new_l = Vec::with_capacity(l_slices.len() + 1);
                new_l.push(new_p.pop().expect("p_slices > MIN_FANOUT"));
                new_l.extend_from_slice(l_slices);
                children.push(summarize_leaf(new_p));
                children.push(summarize_leaf(new_l));
            } else {
                let mut merged = p_slices.clone();
                merged.extend_from_slice(l_slices);
                children.push(summarize_leaf(merged));
            }
        }
        (SliceNode::Internal { children: p_ch }, SliceNode::Internal { children: l_ch }) => {
            if p_ch.len() > MIN_FANOUT {
                let mut new_p = p_ch.clone();
                let mut new_l = Vec::with_capacity(l_ch.len() + 1);
                new_l.push(new_p.pop().expect("p_ch > MIN_FANOUT"));
                new_l.extend_from_slice(l_ch);
                children.push(summarize_internal(new_p));
                children.push(summarize_internal(new_l));
            } else {
                let mut merged = p_ch.clone();
                merged.extend_from_slice(l_ch);
                children.push(summarize_internal(merged));
            }
        }
        _ => unreachable!("D-02 uniform leaf depth"),
    }
    children
}

fn rebalance_first_child<S: Summary>(
    first: Option<ChildEntry<S>>,
    mut children: Vec<ChildEntry<S>>,
) -> Vec<ChildEntry<S>> {
    let Some(first_entry) = first else {
        return children;
    };
    if child_len(&first_entry) >= MIN_FANOUT || children.is_empty() {
        let mut out = Vec::with_capacity(children.len() + 1);
        out.push(first_entry);
        out.extend(children);
        return out;
    }
    let next = children.remove(0);
    let mut out = Vec::with_capacity(children.len() + 2);
    match (first_entry.node.as_ref(), next.node.as_ref()) {
        (SliceNode::Leaf { slices: f_slices }, SliceNode::Leaf { slices: n_slices }) => {
            if n_slices.len() > MIN_FANOUT {
                let mut new_f = f_slices.clone();
                let mut new_n = n_slices.clone();
                new_f.push(new_n.remove(0));
                out.push(summarize_leaf(new_f));
                out.push(summarize_leaf(new_n));
            } else {
                let mut merged = f_slices.clone();
                merged.extend_from_slice(n_slices);
                out.push(summarize_leaf(merged));
            }
        }
        (SliceNode::Internal { children: f_ch }, SliceNode::Internal { children: n_ch }) => {
            if n_ch.len() > MIN_FANOUT {
                let mut new_f = f_ch.clone();
                let mut new_n = n_ch.clone();
                new_f.push(new_n.remove(0));
                out.push(summarize_internal(new_f));
                out.push(summarize_internal(new_n));
            } else {
                let mut merged = f_ch.clone();
                merged.extend_from_slice(n_ch);
                out.push(summarize_internal(merged));
            }
        }
        _ => unreachable!("D-02 uniform leaf depth"),
    }
    out.extend(children);
    out
}

fn pop_rightmost_slice<S: Summary>(entry: ChildEntry<S>) -> (Option<ChildEntry<S>>, ByteSlice<S>) {
    match entry.node.as_ref() {
        SliceNode::Leaf { slices } => {
            let mut rest = slices.clone();
            let last = rest.pop().expect("non-empty leaf");
            (non_empty_leaf(rest), last)
        }
        SliceNode::Internal { children } => {
            let mut rest = children.clone();
            let last = rest.pop().expect("non-empty internal");
            let (new_last, popped) = pop_rightmost_slice(last);
            let balanced = rebalance_last_child(rest, new_last);
            (from_children_list(balanced), popped)
        }
    }
}

fn pop_leftmost_slice<S: Summary>(entry: ChildEntry<S>) -> (ByteSlice<S>, Option<ChildEntry<S>>) {
    match entry.node.as_ref() {
        SliceNode::Leaf { slices } => {
            let mut rest = slices.clone();
            let first = rest.remove(0);
            (first, non_empty_leaf(rest))
        }
        SliceNode::Internal { children } => {
            let mut rest = children.clone();
            let first = rest.remove(0);
            let (popped, new_first) = pop_leftmost_slice(first);
            let balanced = rebalance_first_child(new_first, rest);
            (popped, from_children_list(balanced))
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
    if can_merge_slices(rightmost_slice(&l), leftmost_slice(&r)) {
        let (l_rest, a) = pop_rightmost_slice(l);
        let (b, r_rest) = pop_leftmost_slice(r);
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

fn recompute_leaf_summary<S: Summary>(slices: &[ByteSlice<S>]) -> (usize, usize, S) {
    let mut bytes = 0usize;
    let mut backing_bytes = 0usize;
    let mut summary = S::default();
    for s in slices {
        bytes += s.range.len();
        backing_bytes = backing_bytes.saturating_add(s.buf.len());
        summary = summary.combine(&s.summary);
    }
    (bytes, backing_bytes, summary)
}

fn recompute_internal_summary<S: Summary>(children: &[ChildEntry<S>]) -> (usize, usize, S) {
    let mut bytes = 0usize;
    let mut backing_bytes = 0usize;
    let mut summary = S::default();
    for c in children {
        bytes += c.bytes;
        backing_bytes = backing_bytes.saturating_add(c.backing_bytes);
        summary = summary.combine(&c.summary);
    }
    (bytes, backing_bytes, summary)
}

pub(crate) fn try_insert_in_place<S: Summary>(
    mut entry: ChildEntry<S>,
    offset: usize,
    bytes: &[u8],
) -> Result<ChildEntry<S>, ChildEntry<S>> {
    let Some(node) = Arc::get_mut(&mut entry.node) else {
        return Err(entry);
    };
    match node {
        SliceNode::Leaf { slices } => {
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
                        let (total_bytes, total_backing, total_summary) =
                            recompute_leaf_summary(slices);
                        entry.bytes = total_bytes;
                        entry.backing_bytes = total_backing;
                        entry.summary = total_summary;
                        return Ok(entry);
                    }
                    return Err(entry);
                }
                acc += len;
            }
            Err(entry)
        }
        SliceNode::Internal { children } => {
            let mut acc = 0usize;
            let mut target_idx = None;
            for (idx, child) in children.iter().enumerate() {
                if offset <= acc + child.bytes {
                    target_idx = Some(idx);
                    break;
                }
                acc += child.bytes;
            }
            let Some(idx) = target_idx else {
                return Err(entry);
            };
            let child = children.remove(idx);
            match try_insert_in_place(child, offset - acc, bytes) {
                Ok(updated) => {
                    children.insert(idx, updated);
                    let (total_bytes, total_backing, total_summary) =
                        recompute_internal_summary(children);
                    entry.bytes = total_bytes;
                    entry.backing_bytes = total_backing;
                    entry.summary = total_summary;
                    Ok(entry)
                }
                Err(orig) => {
                    children.insert(idx, orig);
                    Err(entry)
                }
            }
        }
    }
}

pub(crate) fn try_delete_in_place<S: Summary>(
    mut entry: ChildEntry<S>,
    offset: usize,
    delete_len: usize,
) -> Result<(ChildEntry<S>, Vec<u8>), ChildEntry<S>> {
    let Some(node) = Arc::get_mut(&mut entry.node) else {
        return Err(entry);
    };
    match node {
        SliceNode::Leaf { slices } => {
            let mut acc = 0usize;
            for slice in slices.iter_mut() {
                let len = slice.range.len();
                if offset < acc + len {
                    let local_start = offset - acc;
                    let local_end = local_start + delete_len;
                    if local_end <= len
                        && delete_len < len
                        && (local_end == len || local_start == 0)
                    {
                        let abs_start = slice.range.start + local_start;
                        let abs_end = slice.range.start + local_end;
                        let deleted = slice.buf[abs_start..abs_end].to_vec();
                        if local_end == len {
                            let left_end = abs_start;
                            let kept_range = slice.range.start..left_end;
                            let (kept_s, _) =
                                slice.summary.split_pair(&slice.buf[kept_range], &deleted);
                            slice.range.end -= delete_len;
                            if let Some(buf) = Arc::get_mut(&mut slice.buf) {
                                buf.truncate(slice.range.end);
                            }
                            slice.summary = kept_s;
                        } else {
                            let kept_range = abs_end..slice.range.end;
                            let (_, kept_s) =
                                slice.summary.split_pair(&deleted, &slice.buf[kept_range]);
                            slice.range.start += delete_len;
                            slice.summary = kept_s;
                        }
                        let (total_bytes, total_backing, total_summary) =
                            recompute_leaf_summary(slices);
                        entry.bytes = total_bytes;
                        entry.backing_bytes = total_backing;
                        entry.summary = total_summary;
                        return Ok((entry, deleted));
                    }
                    return Err(entry);
                }
                acc += len;
            }
            Err(entry)
        }
        SliceNode::Internal { children } => {
            let mut acc = 0usize;
            let mut target_idx = None;
            for (idx, child) in children.iter().enumerate() {
                if offset < acc + child.bytes {
                    target_idx = Some(idx);
                    break;
                }
                acc += child.bytes;
            }
            let Some(idx) = target_idx else {
                return Err(entry);
            };
            let child = children.remove(idx);
            match try_delete_in_place(child, offset - acc, delete_len) {
                Ok((updated, deleted)) => {
                    children.insert(idx, updated);
                    let (total_bytes, total_backing, total_summary) =
                        recompute_internal_summary(children);
                    entry.bytes = total_bytes;
                    entry.backing_bytes = total_backing;
                    entry.summary = total_summary;
                    Ok((entry, deleted))
                }
                Err(orig) => {
                    children.insert(idx, orig);
                    Err(entry)
                }
            }
        }
    }
}
