//! Undo log in groups, kept in memory.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Insert { pos: usize, bytes: Vec<u8> },
    Delete { pos: usize, bytes: Vec<u8> },
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
        }
    }

    const fn size(&self) -> usize {
        match self {
            Edit::Insert { bytes, .. } | Edit::Delete { bytes, .. } => bytes.len(),
        }
    }
}

/// Edits grouped into undo units. The last group accepts edits while `open`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UndoLog {
    groups: Vec<Vec<Edit>>,
    open: bool,
    size: usize,
    limit: usize,
}

impl UndoLog {
    pub const fn new(limit: usize) -> Self {
        Self {
            groups: Vec::new(),
            open: false,
            size: 0,
            limit,
        }
    }

    pub fn groups(&self) -> &[Vec<Edit>] {
        &self.groups
    }

    pub const fn limit(&self) -> usize {
        self.limit
    }

    /// Appends `edit` to the open group, or starts a new group. Returns the
    /// new log and the number of oldest groups dropped to respect the limit.
    pub fn record(mut self, edit: Edit) -> (Self, usize) {
        if !self.open {
            self.groups.push(Vec::new());
            self.open = true;
        }
        self.size += edit.size();
        self.groups
            .last_mut()
            .expect("open group exists")
            .push(edit);
        self.trim()
    }

    pub const fn close(mut self) -> Self {
        self.open = false;
        self
    }

    fn trim(mut self) -> (Self, usize) {
        let keep_open = usize::from(self.open);
        let mut dropped = 0;
        while self.size > self.limit && self.groups.len() > keep_open {
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
        let (log, _) = UndoLog::new(1 << 20).record(insert(0, "a"));
        let (log, _) = log.record(insert(1, "b"));
        assert_eq!(log.groups().len(), 1);
        let (log, _) = log.close().record(insert(2, "c"));
        assert_eq!(log.groups().len(), 2);
    }

    #[test]
    fn limit_drops_oldest_groups() {
        let (log, _) = UndoLog::new(4).record(insert(0, "abc"));
        let (log, dropped) = log.close().record(insert(0, "de"));
        assert_eq!(dropped, 1);
        let log = log.close();
        assert_eq!(log.groups(), &[vec![insert(0, "de")]]);
    }
}
