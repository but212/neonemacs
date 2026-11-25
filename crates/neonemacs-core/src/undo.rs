//! Undo log in groups, kept in memory.

use crate::eol::Eol;
use crate::{Doc, Replace, compact_small};
use alloc::vec::Vec;

/// Stack-allocated byte buffer of at most 16 bytes for small inline edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlineBytes {
    len: u8,
    bytes: [u8; 16],
}

impl InlineBytes {
    /// Wraps `src` if `src.len() <= 16`.
    #[must_use]
    pub fn new(src: &[u8]) -> Option<Self> {
        if src.len() > 16 {
            return None;
        }
        let mut bytes = [0u8; 16];
        bytes[..src.len()].copy_from_slice(src);
        Some(Self {
            len: src.len() as u8,
            bytes,
        })
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len as usize
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Replaces `delete_len` bytes at `offset` with `insert`.
    Replace(Replace),
    /// Fast-path inline byte insertion (avoids allocating a `Doc` for small edits).
    InsertInline { offset: usize, data: InlineBytes },
    /// Changes the save encoding only. The text is unchanged.
    SetEol { from: Eol, to: Eol },
}

impl Edit {
    #[must_use]
    pub fn insert(pos: usize, bytes: impl AsRef<[u8]>) -> Self {
        let bytes = bytes.as_ref();
        if let Some(data) = InlineBytes::new(bytes) {
            return Self::InsertInline { offset: pos, data };
        }
        Self::Replace(Replace {
            offset: pos,
            delete_len: 0,
            insert: Doc::from_bytes(bytes),
        })
    }

    #[must_use]
    pub fn delete(pos: usize, len: usize) -> Self {
        Self::Replace(Replace {
            offset: pos,
            delete_len: len,
            insert: Doc::default(),
        })
    }

    #[must_use]
    pub const fn replace(offset: usize, delete_len: usize, insert: Doc) -> Self {
        Self::Replace(Replace {
            offset,
            delete_len,
            insert,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditSpan {
    Replace {
        pos: usize,
        removed: usize,
        inserted: usize,
    },
    SetEol,
}

impl EditSpan {
    pub(crate) const fn inverse(self) -> Self {
        match self {
            Self::Replace {
                pos,
                removed,
                inserted,
            } => Self::Replace {
                pos,
                removed: inserted,
                inserted: removed,
            },
            Self::SetEol => Self::SetEol,
        }
    }

    pub(crate) const fn remap(self, anchor: usize) -> usize {
        match self {
            Self::Replace {
                pos,
                removed,
                inserted,
            } if anchor > pos => {
                if anchor <= pos + removed {
                    pos
                } else {
                    anchor - removed + inserted
                }
            }
            _ => anchor,
        }
    }
}

/// A replayable edit and its logical span, which excludes UTF-8 seam bytes restored by the patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedEdit {
    pub(crate) edit: Edit,
    pub(crate) span: EditSpan,
}

impl RecordedEdit {
    fn size(&self) -> u64 {
        match (&self.edit, self.span) {
            (
                Edit::Replace(patch),
                EditSpan::Replace {
                    removed, inserted, ..
                },
            ) => {
                // Preserve the logical charge for cheap inverses, but do not undercount CoW storage.
                let logical = (removed as u64).saturating_add(inserted as u64);
                logical.max(patch.insert.backing_bytes() as u64)
            }
            (Edit::InsertInline { data, .. }, _) => data.len() as u64,
            _ => 0,
        }
    }
}

impl From<Edit> for RecordedEdit {
    fn from(edit: Edit) -> Self {
        let span = match &edit {
            Edit::Replace(r) => EditSpan::Replace {
                pos: r.offset,
                removed: r.delete_len,
                inserted: r.insert.total_bytes(),
            },
            Edit::InsertInline { offset, data } => EditSpan::Replace {
                pos: *offset,
                removed: 0,
                inserted: data.len(),
            },
            Edit::SetEol { .. } => EditSpan::SetEol,
        };
        Self { edit, span }
    }
}

/// One undo unit: its edits, and the save epoch of the buffer if the group opened on clean text
/// (I-41).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    edits: Vec<RecordedEdit>,
    clean_epoch: Option<u32>,
}

impl Group {
    #[must_use]
    pub fn edits(&self) -> &[RecordedEdit] {
        &self.edits
    }

    /// The buffer's save epoch when this group opened on clean text, or `None` if it opened on
    /// modified text.
    #[must_use]
    pub const fn clean_epoch(&self) -> Option<u32> {
        self.clean_epoch
    }

    fn size(&self) -> u64 {
        self.edits.iter().map(RecordedEdit::size).sum()
    }
}

/// Edits grouped into undo units. The last group accepts edits while `open`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UndoLog {
    groups: Vec<Group>,
    open: bool,
    size: u64,
}

const _: () = assert!(core::mem::size_of::<UndoLog>() <= 40);

impl UndoLog {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            groups: Vec::new(),
            open: false,
            size: 0,
        }
    }

    #[must_use]
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// Appends `edit` to the open group, or starts a new group that remembers `clean_epoch`.
    ///
    /// `limit` is `Config.undo_limit`, the only undo limit (I-35). Returns the new log and the
    /// number of oldest groups dropped.
    ///
    /// # Panics
    /// Panics if the open group cannot be accessed.
    #[must_use]
    pub fn record(
        mut self,
        mut edit: RecordedEdit,
        limit: usize,
        clean_epoch: Option<u32>,
    ) -> (Self, usize) {
        if let Edit::Replace(patch) = &mut edit.edit {
            patch.insert = compact_small(core::mem::take(&mut patch.insert));
        }
        if !self.open {
            self.groups.push(Group {
                edits: Vec::new(),
                clean_epoch,
            });
            self.open = true;
        }
        self.size += edit.size();
        self.groups
            .last_mut()
            .expect("open group exists")
            .edits
            .push(edit);
        self.trim(limit)
    }

    #[must_use]
    pub const fn close(mut self) -> Self {
        self.open = false;
        self
    }

    /// Closes the open group and, when `limit > 0`, immediately evicts it if the group exceeds `limit` (I-35).
    #[must_use]
    pub fn close_with_limit(mut self, limit: usize) -> Self {
        self.open = false;
        if limit > 0 {
            self = self.trim(limit).0;
        }
        self
    }

    fn trim(mut self, limit: usize) -> (Self, usize) {
        let keep_open = usize::from(self.open);
        let max_drop = self.groups.len().saturating_sub(keep_open);
        let mut dropped = 0;
        while self.size > limit as u64 && dropped < max_drop {
            self.size -= self.groups[dropped].size();
            dropped += 1;
        }
        if dropped > 0 {
            self.groups.drain(..dropped);
        }
        (self, dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(pos: usize, s: &str) -> RecordedEdit {
        Edit::insert(pos, s.as_bytes()).into()
    }

    #[test]
    fn doc_apply_inverse_is_involution() {
        let doc = Doc::from_bytes(b"hello world");
        let patches = [
            Replace {
                offset: 2,
                delete_len: 0,
                insert: Doc::from_bytes(b"ab"),
            },
            Replace {
                offset: 2,
                delete_len: 2,
                insert: Doc::default(),
            },
            Replace {
                offset: 1,
                delete_len: 1,
                insert: Doc::from_bytes(b"yz"),
            },
        ];
        for p in patches {
            let (next, inv) = doc.clone().apply(p.clone()).unwrap();
            let (restored, inv2) = next.apply(inv).unwrap();
            assert_eq!(restored, doc);
            assert_eq!(inv2, p);
        }
    }

    #[test]
    fn groups_close_explicitly() {
        let (log, _) = UndoLog::new().record(insert(0, "a"), 1 << 20, None);
        let (log, _) = log.record(insert(1, "b"), 1 << 20, None);
        assert_eq!(log.groups().len(), 1);
        let (log, _) = log.close().record(insert(2, "c"), 1 << 20, None);
        assert_eq!(log.groups().len(), 2);
    }

    #[test]
    fn limit_drops_oldest_groups() {
        let (log, _) = UndoLog::new().record(insert(0, "abc"), 4, None);
        let (log, dropped) = log.close().record(insert(0, "de"), 4, None);
        assert_eq!(dropped, 1);
        let log = log.close();
        assert_eq!(log.groups().len(), 1);
        assert_eq!(log.groups()[0].edits(), &[insert(0, "de")]);
    }

    /// Three groups of 8, 4 and 1 bytes. The first is dropped when the second opens (size 10 > limit),
    /// leaving 4; the third then brings the total to 5.
    fn trimmed_twice(limit: usize) -> (UndoLog, usize) {
        let (log, _) = UndoLog::new().record(insert(0, "aaaa"), limit, None);
        let (log, _) = log.record(insert(4, "aaaa"), limit, None);
        let (log, d1) = log.close().record(insert(0, "bb"), limit, None);
        let (log, _) = log.record(insert(2, "bb"), limit, None);
        let (log, d2) = log.close().record(insert(0, "c"), limit, None);
        (log, d1 + d2)
    }

    #[test]
    fn a_dropped_group_leaves_exactly_its_own_size_out_of_the_total() {
        // Total after the drop is 4; with the third group 5 <= 5: nothing more is dropped.
        let (log, dropped) = trimmed_twice(5);
        assert_eq!(dropped, 1);
        assert_eq!(log.groups().len(), 2);
        // Limit 4: 5 > 4, so the second group goes too.
        let (log, dropped) = trimmed_twice(4);
        assert_eq!(dropped, 2);
        assert_eq!(log.groups().len(), 1);
    }

    #[test]
    fn close_with_limit_evicts_oversized_single_group_when_limit_positive() {
        let (log, _) = UndoLog::new().record(insert(0, "12345"), 4, None);
        assert_eq!(log.groups().len(), 1);
        let log = log.close_with_limit(4);
        assert!(log.groups().is_empty());

        let (exact, _) = UndoLog::new().record(insert(0, "1234"), 4, None);
        let exact = exact.close_with_limit(4);
        assert_eq!(exact.groups().len(), 1);

        let (zero, _) = UndoLog::new().record(insert(0, "1234"), 0, None);
        let zero = zero.close_with_limit(0);
        assert_eq!(zero.groups().len(), 1);
    }
}
