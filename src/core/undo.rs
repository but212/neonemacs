//! Undo log in groups, kept in memory.

use crate::core::eol::Eol;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Insert {
        pos: usize,
        bytes: Vec<u8>,
    },
    Delete {
        pos: usize,
        bytes: Vec<u8>,
    },
    /// Changes the save encoding only. The text is unchanged.
    SetEol {
        from: Eol,
        to: Eol,
    },
}

impl Edit {
    pub fn inverse(&self) -> Edit {
        match self {
            Edit::Insert { pos, bytes } => Edit::Delete {
                pos: *pos,
                bytes: bytes.clone(),
            },
            Edit::Delete { pos, bytes } => Edit::Insert {
                pos: *pos,
                bytes: bytes.clone(),
            },
            Edit::SetEol { from, to } => Edit::SetEol {
                from: *to,
                to: *from,
            },
        }
    }

    const fn size(&self) -> usize {
        match self {
            Edit::Insert { bytes, .. } | Edit::Delete { bytes, .. } => bytes.len(),
            Edit::SetEol { .. } => 0,
        }
    }
}

/// Edits grouped into undo units. The last group accepts edits while `open`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UndoLog {
    groups: Vec<Vec<Edit>>,
    open: bool,
    size: usize,
}

impl UndoLog {
    pub const fn new() -> Self {
        Self {
            groups: Vec::new(),
            open: false,
            size: 0,
        }
    }

    pub fn groups(&self) -> &[Vec<Edit>] {
        &self.groups
    }

    /// Appends `edit` to the open group, or starts a new group. `limit` is `Config.undo_limit`,
    /// the only undo limit (I-35). Returns the new log and the number of oldest groups dropped.
    pub fn record(mut self, edit: Edit, limit: usize) -> (Self, usize) {
        if !self.open {
            self.groups.push(Vec::new());
            self.open = true;
        }
        self.size += edit.size();
        self.groups
            .last_mut()
            .expect("open group exists")
            .push(edit);
        self.trim(limit)
    }

    pub const fn close(mut self) -> Self {
        self.open = false;
        self
    }

    fn trim(mut self, limit: usize) -> (Self, usize) {
        let keep_open = usize::from(self.open);
        let mut dropped = 0;
        while self.size > limit && self.groups.len() > keep_open {
            let group = self.groups.remove(0);
            self.size -= group.iter().map(Edit::size).sum::<usize>();
            dropped += 1;
        }
        (self, dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(pos: usize, s: &str) -> Edit {
        Edit::Insert {
            pos,
            bytes: s.as_bytes().to_vec(),
        }
    }

    #[test]
    fn groups_close_explicitly() {
        let (log, _) = UndoLog::new().record(insert(0, "a"), 1 << 20);
        let (log, _) = log.record(insert(1, "b"), 1 << 20);
        assert_eq!(log.groups().len(), 1);
        let (log, _) = log.close().record(insert(2, "c"), 1 << 20);
        assert_eq!(log.groups().len(), 2);
    }

    #[test]
    fn limit_drops_oldest_groups() {
        let (log, _) = UndoLog::new().record(insert(0, "abc"), 4);
        let (log, dropped) = log.close().record(insert(0, "de"), 4);
        assert_eq!(dropped, 1);
        let log = log.close();
        assert_eq!(log.groups(), &[vec![insert(0, "de")]]);
    }
}
