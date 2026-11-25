//! RFC 3629 UTF-8 unit decoding, local boundary queries, and seam-merge logic (D-01).

use alloc::vec::Vec;
use triomphe::{AllocError, Arc};

use crate::{ByteSlice, CHUNK_CAP, MAX_SLICE_BYTES, Summary, Unit};

/// Decodes a single `Unit` at `pos` without checking whether `pos` is a unit boundary.
///
/// Only RFC 3629 UTF-8 sequences (no overlongs, no surrogates `U+D800..=U+DFFF`,
/// `<= U+10FFFF`) are decoded as `Unit::Char`; everything else yields `Unit::Raw`.
#[must_use]
pub fn decode_unit_raw(bytes: &[u8], pos: usize) -> Option<(Unit, usize)> {
    let b0 = *bytes.get(pos)?;
    if b0 < 0x80 {
        return Some((Unit::Char(b0 as char), 1));
    }
    let width = match b0 {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return Some((Unit::Raw(b0), 1)),
    };
    let Some(end) = pos.checked_add(width) else {
        return Some((Unit::Raw(b0), 1));
    };
    let Some(seq) = bytes.get(pos..end) else {
        return Some((Unit::Raw(b0), 1));
    };
    let Ok(s) = core::str::from_utf8(seq) else {
        return Some((Unit::Raw(b0), 1));
    };
    let ch = s.chars().next().expect("non-empty valid utf-8");
    Some((Unit::Char(ch), width))
}

/// Returns `true` if `pos` is a valid `Unit` boundary within `slice` (which itself
/// starts and ends on valid `Unit` boundaries).
#[must_use]
pub fn is_boundary_in_slice(slice: &[u8], pos: usize) -> bool {
    if pos == 0 || pos == slice.len() {
        return true;
    }
    if pos > slice.len() {
        return false;
    }
    if (slice[pos] & 0xC0) != 0x80 {
        return true;
    }
    let max_back = pos.min(3);
    for d in 1..=max_back {
        if let Some((Unit::Char(_), len)) = decode_unit_raw(slice, pos - d)
            && len > d
        {
            return false;
        }
    }
    true
}

/// Decodes the `Unit` starting at `pos` within `slice`, returning `None` if `pos`
/// is out of range or not a valid `Unit` boundary.
#[must_use]
pub fn decode_in_slice(slice: &[u8], pos: usize) -> Option<(Unit, usize)> {
    if pos >= slice.len() || !is_boundary_in_slice(slice, pos) {
        return None;
    }
    decode_unit_raw(slice, pos)
}

#[must_use]
pub fn floor_in_slice(slice: &[u8], pos: usize) -> usize {
    let mut p = pos.min(slice.len());
    while p > 0 && !is_boundary_in_slice(slice, p) {
        p -= 1;
    }
    p
}

#[must_use]
pub fn ceil_in_slice(slice: &[u8], pos: usize) -> usize {
    let mut p = pos.min(slice.len());
    while p < slice.len() && !is_boundary_in_slice(slice, p) {
        p += 1;
    }
    p
}

#[must_use]
pub fn prev_in_slice(slice: &[u8], pos: usize) -> usize {
    let p = pos.min(slice.len());
    if p == 0 {
        return 0;
    }
    floor_in_slice(slice, p - 1)
}

#[must_use]
pub fn next_in_slice(slice: &[u8], pos: usize) -> usize {
    let p = pos.min(slice.len());
    if p >= slice.len() {
        return slice.len();
    }
    ceil_in_slice(slice, p + 1)
}

/// Splits `bytes` into `ByteSlice<S>` chunks of at most `MAX_SLICE_BYTES`,
/// always breaking at valid `Unit` boundaries (D-01, D-02).
#[must_use]
pub(crate) fn chunk_bytes_to_slices<S: Summary>(bytes: &[u8]) -> Vec<ByteSlice<S>> {
    try_append_slices(Vec::new(), bytes).expect("cannot allocate document slices")
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
        let backing_len = v.len();
        let buf = Arc::try_new(v)?;
        let summary = S::from_leaf_slice(chunk, backing_len);
        slices.push(ByteSlice {
            buf,
            range: 0..chunk.len(),
            summary,
        });
        start = end;
    }
    Ok(slices)
}

/// Result of pre-mutation UTF-8 seam analysis around an edit site (D-01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeamMerge {
    pub(crate) k_left: usize,
    pub(crate) k_right: usize,
    pub(crate) abs_insert_left: usize,
    pub(crate) abs_insert_right: usize,
    pub(crate) left_merged: Vec<u8>,
    pub(crate) right_merged: Vec<u8>,
}

/// Computes `k_left`, `k_right`, and merged UTF-8 bytes for an edit seam.
#[must_use]
pub(crate) fn compute_seam_merge(
    left_raw: &[u8],
    insert_head: &[u8],
    insert_tail: &[u8],
    insert_total: usize,
    right_raw: &[u8],
) -> SeamMerge {
    if insert_total <= 2 && !left_raw.is_empty() && !right_raw.is_empty() {
        for kl in (1..=left_raw.len()).rev() {
            for kr in (1..=right_raw.len()).rev() {
                let total = kl + insert_total + kr;
                if total > 4 {
                    continue;
                }
                let mut cand = [0u8; 4];
                cand[..kl].copy_from_slice(&left_raw[left_raw.len() - kl..]);
                cand[kl..kl + insert_total].copy_from_slice(&insert_head[..insert_total]);
                cand[kl + insert_total..total].copy_from_slice(&right_raw[..kr]);
                if let Some((Unit::Char(_), len)) = decode_unit_raw(&cand[..total], 0)
                    && len == total
                {
                    return SeamMerge {
                        k_left: kl,
                        k_right: kr,
                        abs_insert_left: insert_total,
                        abs_insert_right: 0,
                        left_merged: cand[..total].to_vec(),
                        right_merged: Vec::new(),
                    };
                }
            }
        }
    }

    let mut k_left = 0;
    let mut abs_insert_left = 0;
    let mut left_merged = Vec::new();
    if !left_raw.is_empty() && insert_total > 0 {
        'left_search: for kl in 1..=left_raw.len() {
            let max_ai = insert_head.len().min(4 - kl);
            for ai in (1..=max_ai).rev() {
                let total = kl + ai;
                let mut cand = [0u8; 4];
                cand[..kl].copy_from_slice(&left_raw[left_raw.len() - kl..]);
                cand[kl..total].copy_from_slice(&insert_head[..ai]);
                if let Some((Unit::Char(_), len)) = decode_unit_raw(&cand[..total], 0)
                    && len == total
                {
                    k_left = kl;
                    abs_insert_left = ai;
                    left_merged = cand[..total].to_vec();
                    break 'left_search;
                }
            }
        }
    }

    let rem_insert = insert_total - abs_insert_left;
    let mut k_right = 0;
    let mut abs_insert_right = 0;
    let mut right_merged = Vec::new();
    if rem_insert > 0 && !right_raw.is_empty() {
        let avail_tail = insert_tail.len().min(rem_insert);
        let tail_slice = &insert_tail[insert_tail.len() - avail_tail..];
        'right_search: for ar in 1..=avail_tail {
            let max_kr = right_raw.len().min(4 - ar);
            for kr in (1..=max_kr).rev() {
                let total = ar + kr;
                let mut cand = [0u8; 4];
                cand[..ar].copy_from_slice(&tail_slice[avail_tail - ar..]);
                cand[ar..total].copy_from_slice(&right_raw[..kr]);
                if let Some((Unit::Char(_), len)) = decode_unit_raw(&cand[..total], 0)
                    && len == total
                {
                    k_right = kr;
                    abs_insert_right = ar;
                    right_merged = cand[..total].to_vec();
                    break 'right_search;
                }
            }
        }
    }

    SeamMerge {
        k_left,
        k_right,
        abs_insert_left,
        abs_insert_right,
        left_merged,
        right_merged,
    }
}
