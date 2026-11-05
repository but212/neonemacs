//! Undo log in groups, kept in memory.

use crate::core::eol::Eol;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Replaces `removed` at `pos` by `inserted`. An insert has empty `removed`, a delete has
    /// empty `inserted`, and the inverse swaps the two.
    Replace {
        pos: usize,
        removed: Vec<u8>,
        inserted: Vec<u8>,
    },
    /// Changes the save encoding only. The text is unchanged.
    SetEol { from: Eol, to: Eol },
}

impl Edit {
    #[must_use]
    pub fn insert(pos: usize, bytes: impl Into<Vec<u8>>) -> Self {
        Self::Replace {
            pos,
            removed: Vec::new(),
            inserted: bytes.into(),
        }
    }

    #[must_use]
    pub fn delete(pos: usize, bytes: impl Into<Vec<u8>>) -> Self {
        Self::Replace {
            pos,
            removed: bytes.into(),
            inserted: Vec::new(),
        }
    }

    #[must_use]
    pub fn inverse(&self) -> Self {
        match self {
            Self::Replace {
                pos,
                removed,
                inserted,
            } => Self::Replace {
                pos: *pos,
                removed: inserted.clone(),
                inserted: removed.clone(),
            },
            Self::SetEol { from, to } => Self::SetEol {
                from: *to,
                to: *from,
            },
        }
    }

    const fn size(&self) -> usize {
        match self {
            Self::Replace {
                removed, inserted, ..
            } => removed.len() + inserted.len(),
            Self::SetEol { .. } => 0,
        }
    }
}

/// One undo unit: its edits, and the save epoch of the buffer if the group opened on clean text
/// (I-41).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    edits: Vec<Edit>,
    clean_epoch: Option<u32>,
}

impl Group {
    #[must_use]
    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }

    /// The buffer's save epoch when this group opened on clean text, or `None` if it opened on
    /// modified text.
    #[must_use]
    pub const fn clean_epoch(&self) -> Option<u32> {
        self.clean_epoch
    }

    fn size(&self) -> usize {
        self.edits.iter().map(Edit::size).sum()
    }
}

/// Edits grouped into undo units. The last group accepts edits while `open`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UndoLog {
    groups: Vec<Group>,
    open: bool,
    size: usize,
}

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
    pub fn record(mut self, edit: Edit, limit: usize, clean_epoch: Option<u32>) -> (Self, usize) {
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

    fn trim(mut self, limit: usize) -> (Self, usize) {
        let keep_open = usize::from(self.open);
        let mut dropped = 0;
        while self.size > limit && self.groups.len() > keep_open {
            let group = self.groups.remove(0);
            self.size -= group.size();
            dropped += 1;
        }
        (self, dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(pos: usize, s: &str) -> Edit {
        Edit::insert(pos, s.as_bytes())
    }

    #[test]
    fn inverse_is_involution() {
        let edits = [
            Edit::insert(2, b"ab".as_slice()),
            Edit::delete(2, b"ab".as_slice()),
            Edit::Replace {
                pos: 1,
                removed: b"x".to_vec(),
                inserted: b"yz".to_vec(),
            },
            Edit::SetEol {
                from: Eol::Lf,
                to: Eol::Crlf,
            },
        ];
        for e in edits {
            assert_eq!(e.inverse().inverse(), e);
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
}
