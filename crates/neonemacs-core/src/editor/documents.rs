//! Documents group: the buffers, the window layout, the kill ring, and the edit counter (I-32).
//! `edit_seq` belongs here, so `Editor::apply` changes it in the same group it changes buffers
//! and windows (I-31). Fields are visible only to `editor`.

use crate::buffer::BufferPool;
use crate::window::{Window, WindowManager};
use std::path::PathBuf;

/// The kill ring: killed text, newest last, with ring rotation for `yank-pop`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KillRing {
    entries: Vec<Vec<u8>>,
}

impl KillRing {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Builds a ring from entries without the `max` check, so tests only (I-26).
    #[cfg(test)]
    #[must_use]
    pub const fn from_entries(entries: Vec<Vec<u8>>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn entries(&self) -> &[Vec<u8>] {
        &self.entries
    }

    /// Adds a new entry, then drops the oldest entries until at most `max` remain (I-26).
    #[must_use]
    pub fn push(mut self, bytes: Vec<u8>, max: usize) -> Self {
        self.entries.push(bytes);
        let excess = self.entries.len().saturating_sub(max);
        self.entries.drain(..excess);
        self
    }

    #[must_use]
    pub fn append_last(mut self, bytes: &[u8]) -> Self {
        if let Some(last) = self.entries.last_mut() {
            last.extend_from_slice(bytes);
        } else {
            self.entries.push(bytes.to_vec());
        }
        self
    }

    #[must_use]
    pub fn rotate_right(mut self, n: usize) -> Self {
        if !self.entries.is_empty() {
            let len = self.entries.len();
            self.entries.rotate_right(n % len);
        }
        self
    }

    #[must_use]
    pub fn last(&self) -> Option<&[u8]> {
        self.entries.last().map(Vec::as_slice)
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Debug, Clone)]
pub struct Documents {
    pub(super) buffers: BufferPool,
    pub(super) windows: WindowManager,
    pub(super) kill_ring: KillRing,
    pub(super) edit_seq: u64,
    /// Where the unsaved text of the scratch buffer is kept. `None` keeps it in memory only (I-43).
    pub(super) scratch_slot: Option<PathBuf>,
}

impl Documents {
    pub fn new(initial_window: Window) -> Self {
        Self {
            buffers: BufferPool::new(),
            windows: WindowManager::new(initial_window),
            kill_ring: KillRing::new(),
            edit_seq: 0,
            scratch_slot: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_drops_oldest_past_max() {
        let max = 120;
        let mut kr = KillRing::new();
        for i in 0..=max {
            kr = kr.push(i.to_string().into_bytes(), max);
        }
        assert_eq!(kr.len(), max);
        assert_eq!(kr.entries()[0], b"1".to_vec());
        assert_eq!(kr.last(), Some(max.to_string().as_bytes()));
    }

    #[test]
    fn lowering_max_drops_oldest_entries_on_next_push() {
        let kr = KillRing::from_entries(vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
        let kr = kr.push(b"d".to_vec(), 2);
        assert_eq!(kr.entries(), &[b"c".to_vec(), b"d".to_vec()]);
    }
}
