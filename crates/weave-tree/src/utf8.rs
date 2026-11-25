//! RFC 3629 UTF-8 unit decoding, local boundary queries, and seam-merge logic (D-01).

use alloc::vec::Vec;
use triomphe::{AllocError, Arc};

use crate::{ByteSlice, CHUNK_CAP, MAX_SLICE_BYTES, Summary, Unit};

/// Returns `true` if `b` is a UTF-8 continuation byte (`10xxxxxx`).
pub(crate) const fn is_utf8_cont(b: u8) -> bool {
    (b & 0xC0) == 0x80
}

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
    let window = &bytes[pos..bytes.len().min(pos + 4)];
    let chunk = window.utf8_chunks().next()?;
    if let Some(ch) = chunk.valid().chars().next() {
        Some((Unit::Char(ch), ch.len_utf8()))
    } else {
        Some((Unit::Raw(b0), 1))
    }
}

/// Returns `true` if `pos` is a valid `Unit` boundary within `slice` (which itself
/// starts and ends on valid `Unit` boundaries).
#[must_use]
pub(crate) fn is_boundary_in_slice(slice: &[u8], pos: usize) -> bool {
    if pos == 0 || pos == slice.len() {
        return true;
    }
    if pos > slice.len() {
        return false;
    }
    if !is_utf8_cont(slice[pos]) {
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
pub(crate) fn floor_in_slice(slice: &[u8], pos: usize) -> usize {
    let mut p = pos.min(slice.len());
    while p > 0 && !is_boundary_in_slice(slice, p) {
        p -= 1;
    }
    p
}

#[must_use]
pub(crate) fn ceil_in_slice(slice: &[u8], pos: usize) -> usize {
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
pub(crate) fn next_in_slice(slice: &[u8], pos: usize) -> usize {
    let p = pos.min(slice.len());
    if p >= slice.len() {
        return slice.len();
    }
    ceil_in_slice(slice, p + 1)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeamMerge {
    pub(crate) k_left: usize,
    pub(crate) k_right: usize,
    pub(crate) abs_insert_left: usize,
    pub(crate) abs_insert_right: usize,
    left_merged: [u8; 4],
    left_merged_len: u8,
    right_merged: [u8; 4],
    right_merged_len: u8,
}

impl SeamMerge {
    pub(crate) fn left_bytes(&self) -> &[u8] {
        &self.left_merged[..usize::from(self.left_merged_len)]
    }

    pub(crate) fn right_bytes(&self) -> &[u8] {
        &self.right_merged[..usize::from(self.right_merged_len)]
    }
}

/// Computes `k_left`, `k_right`, and merged UTF-8 bytes for an edit seam.
fn try_assemble_char(parts: &[&[u8]]) -> Option<([u8; 4], u8)> {
    let total: usize = parts.iter().map(|p| p.len()).sum();
    if total > 4 {
        return None;
    }
    let mut cand = [0u8; 4];
    let mut offset = 0usize;
    for part in parts {
        cand[offset..offset + part.len()].copy_from_slice(part);
        offset += part.len();
    }
    matches!(decode_unit_raw(&cand[..total], 0), Some((Unit::Char(_), len)) if len == total)
        .then_some((cand, total as u8))
}

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
                if let Some((cand, len)) = try_assemble_char(&[
                    &left_raw[left_raw.len() - kl..],
                    &insert_head[..insert_total],
                    &right_raw[..kr],
                ]) {
                    return SeamMerge {
                        k_left: kl,
                        k_right: kr,
                        abs_insert_left: insert_total,
                        abs_insert_right: 0,
                        left_merged: cand,
                        left_merged_len: len,
                        right_merged: [0u8; 4],
                        right_merged_len: 0,
                    };
                }
            }
        }
    }

    let mut k_left = 0;
    let mut abs_insert_left = 0;
    let mut left_merged = [0u8; 4];
    let mut left_merged_len = 0u8;
    if !left_raw.is_empty() && insert_total > 0 {
        'left_search: for kl in 1..=left_raw.len() {
            let max_ai = insert_head.len().min(4 - kl);
            for ai in (1..=max_ai).rev() {
                if let Some((cand, len)) =
                    try_assemble_char(&[&left_raw[left_raw.len() - kl..], &insert_head[..ai]])
                {
                    k_left = kl;
                    abs_insert_left = ai;
                    left_merged = cand;
                    left_merged_len = len;
                    break 'left_search;
                }
            }
        }
    }

    let rem_insert = insert_total - abs_insert_left;
    let mut k_right = 0;
    let mut abs_insert_right = 0;
    let mut right_merged = [0u8; 4];
    let mut right_merged_len = 0u8;
    if rem_insert > 0 && !right_raw.is_empty() {
        let avail_tail = insert_tail.len().min(rem_insert);
        let tail_slice = &insert_tail[insert_tail.len() - avail_tail..];
        'right_search: for ar in 1..=avail_tail {
            let max_kr = right_raw.len().min(4 - ar);
            for kr in (1..=max_kr).rev() {
                if let Some((cand, len)) =
                    try_assemble_char(&[&tail_slice[avail_tail - ar..], &right_raw[..kr]])
                {
                    k_right = kr;
                    abs_insert_right = ar;
                    right_merged = cand;
                    right_merged_len = len;
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
        left_merged_len,
        right_merged,
        right_merged_len,
    }
}
