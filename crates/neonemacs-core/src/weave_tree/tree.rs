//! Structural B-Tree operations (`from_slices`, `split_at`, `join`) preserving D-02 and D-03.

use std::sync::Arc;

use super::utf8::count_line_breaks;
use super::{ByteSlice, CHUNK_CAP, ChildEntry, MAX_FANOUT, MAX_SLICE_BYTES, MIN_FANOUT, SliceNode};

#[must_use]
pub(crate) fn summarize_leaf(slices: Vec<ByteSlice>) -> ChildEntry {
    let mut bytes = 0usize;
    let mut line_breaks = 0usize;
    for s in &slices {
        bytes += s.range.len();
        line_breaks += s.line_breaks;
    }
    ChildEntry {
        node: Arc::new(SliceNode::Leaf { slices }),
        bytes,
        line_breaks,
    }
}

#[must_use]
pub(crate) fn summarize_internal(children: Vec<ChildEntry>) -> ChildEntry {
    let mut bytes = 0usize;
    let mut line_breaks = 0usize;
    for c in &children {
        bytes += c.bytes;
        line_breaks += c.line_breaks;
    }
    ChildEntry {
        node: Arc::new(SliceNode::Internal { children }),
        bytes,
        line_breaks,
    }
}

#[must_use]
pub(crate) fn entry_height(entry: &ChildEntry) -> usize {
    let mut cur = entry;
    let mut h = 1;
    while let SliceNode::Internal { children } = cur.node.as_ref() {
        h += 1;
        cur = &children[0];
    }
    h
}

/// Groups items into chunks of `MAX_FANOUT`, redistributing the last two chunks
/// if there are >= 2 chunks and the last has fewer than `MIN_FANOUT` items.
fn pack_fanout<T>(items: Vec<T>) -> Vec<Vec<T>> {
    let mut groups: Vec<Vec<T>> = Vec::new();
    let mut cur = Vec::with_capacity(MAX_FANOUT);
    for item in items {
        cur.push(item);
        if cur.len() == MAX_FANOUT {
            groups.push(cur);
            cur = Vec::with_capacity(MAX_FANOUT);
        }
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    let len = groups.len();
    if len >= 2 && groups[len - 1].len() < MIN_FANOUT {
        let last = groups.pop().expect("len >= 2");
        let mut prev = groups.pop().expect("len >= 2");
        let total = prev.len() + last.len();
        let left_len = total / 2;
        prev.extend(last);
        let right = prev.split_off(left_len);
        groups.push(prev);
        groups.push(right);
    }
    groups
}

/// Bottom-up O(N) B-Tree builder from valid `ByteSlice`s.
#[must_use]
pub(crate) fn from_slices(slices: Vec<ByteSlice>) -> Option<ChildEntry> {
    if slices.is_empty() {
        return None;
    }
    let mut level: Vec<ChildEntry> = pack_fanout(slices)
        .into_iter()
        .map(summarize_leaf)
        .collect();
    while level.len() > 1 {
        level = pack_fanout(level)
            .into_iter()
            .map(summarize_internal)
            .collect();
    }
    level.into_iter().next()
}

fn from_children_list(mut children: Vec<ChildEntry>) -> Option<ChildEntry> {
    match children.len() {
        0 => None,
        1 => children.pop(),
        _ => Some(summarize_internal(children)),
    }
}

fn non_empty_leaf(slices: Vec<ByteSlice>) -> Option<ChildEntry> {
    if slices.is_empty() {
        None
    } else {
        Some(summarize_leaf(slices))
    }
}

fn can_merge_slices(a: &ByteSlice, b: &ByteSlice) -> bool {
    let combined = a.range.len() + b.range.len();
    (Arc::ptr_eq(&a.buf, &b.buf) && a.range.end == b.range.start && combined <= MAX_SLICE_BYTES)
        || combined <= CHUNK_CAP
}

fn merge_two_slices(a: ByteSlice, b: ByteSlice) -> ByteSlice {
    let combined = a.range.len() + b.range.len();
    if Arc::ptr_eq(&a.buf, &b.buf) && a.range.end == b.range.start && combined <= MAX_SLICE_BYTES {
        return ByteSlice {
            buf: a.buf,
            range: a.range.start..b.range.end,
            line_breaks: a.line_breaks + b.line_breaks,
        };
    }
    let mut buf = Vec::with_capacity(combined);
    buf.extend_from_slice(&a.buf[a.range]);
    buf.extend_from_slice(&b.buf[b.range]);
    ByteSlice {
        buf: Arc::from(buf),
        range: 0..combined,
        line_breaks: a.line_breaks + b.line_breaks,
    }
}

fn rightmost_slice(entry: &ChildEntry) -> &ByteSlice {
    let mut cur = entry;
    loop {
        match cur.node.as_ref() {
            SliceNode::Leaf { slices } => return slices.last().expect("non-empty leaf"),
            SliceNode::Internal { children } => cur = children.last().expect("non-empty internal"),
        }
    }
}

fn leftmost_slice(entry: &ChildEntry) -> &ByteSlice {
    let mut cur = entry;
    loop {
        match cur.node.as_ref() {
            SliceNode::Leaf { slices } => return &slices[0],
            SliceNode::Internal { children } => cur = &children[0],
        }
    }
}

fn child_len(entry: &ChildEntry) -> usize {
    match entry.node.as_ref() {
        SliceNode::Leaf { slices } => slices.len(),
        SliceNode::Internal { children } => children.len(),
    }
}

/// Rebalances the last child of `children` if it has fewer than `MIN_FANOUT` items
/// (or is `None`), borrowing from or merging with its left sibling.
fn rebalance_last_child(
    mut children: Vec<ChildEntry>,
    last: Option<ChildEntry>,
) -> Vec<ChildEntry> {
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

/// Rebalances the first child of `children` if it has fewer than `MIN_FANOUT` items
/// (or is `None`), borrowing from or merging with its right sibling.
fn rebalance_first_child(
    first: Option<ChildEntry>,
    mut children: Vec<ChildEntry>,
) -> Vec<ChildEntry> {
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

fn pop_rightmost_slice(entry: ChildEntry) -> (Option<ChildEntry>, ByteSlice) {
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

fn pop_leftmost_slice(entry: ChildEntry) -> (ByteSlice, Option<ChildEntry>) {
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

fn join_at_height(l: ChildEntry, r: ChildEntry, hl: usize, hr: usize) -> Vec<ChildEntry> {
    if hl == hr {
        match (l.node.as_ref(), r.node.as_ref()) {
            (SliceNode::Leaf { slices: ls }, SliceNode::Leaf { slices: rs }) => {
                let mut all = Vec::with_capacity(ls.len() + rs.len());
                all.extend_from_slice(ls);
                all.extend_from_slice(rs);
                if all.len() <= MAX_FANOUT {
                    vec![summarize_leaf(all)]
                } else {
                    let mid = all.len() / 2;
                    let right = all.split_off(mid);
                    vec![summarize_leaf(all), summarize_leaf(right)]
                }
            }
            (SliceNode::Internal { children: lc }, SliceNode::Internal { children: rc }) => {
                let mut all = Vec::with_capacity(lc.len() + rc.len());
                all.extend_from_slice(lc);
                all.extend_from_slice(rc);
                if all.len() <= MAX_FANOUT {
                    vec![summarize_internal(all)]
                } else {
                    let mid = all.len() / 2;
                    let right = all.split_off(mid);
                    vec![summarize_internal(all), summarize_internal(right)]
                }
            }
            _ => unreachable!("hl == hr guarantees matching node variants"),
        }
    } else if hl > hr {
        let SliceNode::Internal { children } = l.node.as_ref() else {
            unreachable!("hl > hr guarantees internal node");
        };
        let mut all = children.clone();
        let last = all.pop().expect("non-empty internal");
        let mid = join_at_height(last, r, hl - 1, hr);
        all.extend(mid);
        if all.len() <= MAX_FANOUT {
            vec![summarize_internal(all)]
        } else {
            let split = all.len() / 2;
            let right = all.split_off(split);
            vec![summarize_internal(all), summarize_internal(right)]
        }
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
        if combined.len() <= MAX_FANOUT {
            vec![summarize_internal(combined)]
        } else {
            let split = combined.len() / 2;
            let right = combined.split_off(split);
            vec![summarize_internal(combined), summarize_internal(right)]
        }
    }
}

fn join_trees_raw(left: Option<ChildEntry>, right: Option<ChildEntry>) -> Option<ChildEntry> {
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

/// Joins two valid trees in O(log P), merging adjacent seam slices when
/// contiguous in the same `Arc<[u8]>` (`<= MAX_SLICE_BYTES`) or `<= CHUNK_CAP`.
#[must_use]
pub(crate) fn join(left: Option<ChildEntry>, right: Option<ChildEntry>) -> Option<ChildEntry> {
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

fn split_byte_slice(slice: &ByteSlice, local: usize) -> (ByteSlice, ByteSlice) {
    let split_pt = slice.range.start + local;
    let left_range = slice.range.start..split_pt;
    let right_range = split_pt..slice.range.end;
    let left_lb = count_line_breaks(&slice.buf[left_range.clone()]);
    let right_lb = slice.line_breaks - left_lb;
    (
        ByteSlice {
            buf: Arc::clone(&slice.buf),
            range: left_range,
            line_breaks: left_lb,
        },
        ByteSlice {
            buf: Arc::clone(&slice.buf),
            range: right_range,
            line_breaks: right_lb,
        },
    )
}

/// Splits `root` at byte offset `pos` (must be a valid `Unit` boundary) in O(log P).
#[must_use]
pub(crate) fn split_at(
    root: Option<ChildEntry>,
    pos: usize,
) -> (Option<ChildEntry>, Option<ChildEntry>) {
    let Some(entry) = root else {
        return (None, None);
    };
    if pos == 0 {
        return (None, Some(entry));
    }
    if pos >= entry.bytes {
        return (Some(entry), None);
    }
    split_entry(entry, pos)
}

fn split_entry(entry: ChildEntry, pos: usize) -> (Option<ChildEntry>, Option<ChildEntry>) {
    match entry.node.as_ref() {
        SliceNode::Leaf { slices } => {
            let mut acc = 0usize;
            for (idx, slice) in slices.iter().enumerate() {
                let len = slice.range.len();
                if pos == acc {
                    return (
                        non_empty_leaf(slices[..idx].to_vec()),
                        non_empty_leaf(slices[idx..].to_vec()),
                    );
                }
                if pos < acc + len {
                    let (ls, rs) = split_byte_slice(slice, pos - acc);
                    let mut left = slices[..idx].to_vec();
                    left.push(ls);
                    let mut right = Vec::with_capacity(slices.len() - idx);
                    right.push(rs);
                    right.extend_from_slice(&slices[idx + 1..]);
                    return (non_empty_leaf(left), non_empty_leaf(right));
                }
                acc += len;
            }
            (Some(entry), None)
        }
        SliceNode::Internal { children } => {
            let mut acc = 0usize;
            for (idx, child) in children.iter().enumerate() {
                if pos == acc {
                    return (
                        from_children_list(children[..idx].to_vec()),
                        from_children_list(children[idx..].to_vec()),
                    );
                }
                if pos < acc + child.bytes {
                    let (mid_l, mid_r) = split_entry(child.clone(), pos - acc);
                    let left_prefix = from_children_list(children[..idx].to_vec());
                    let right_suffix = from_children_list(children[idx + 1..].to_vec());
                    return (join(left_prefix, mid_l), join(mid_r, right_suffix));
                }
                acc += child.bytes;
            }
            (Some(entry), None)
        }
    }
}
