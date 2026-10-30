//! The kill ring: killed text, newest last, with ring rotation for `yank-pop`.

/// Most entries the kill ring keeps (I-26).
pub const KILL_RING_MAX: usize = 120;

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

    /// Builds a ring from entries without the `KILL_RING_MAX` check, so tests only (I-26).
    #[cfg(test)]
    #[must_use]
    pub const fn from_entries(entries: Vec<Vec<u8>>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn entries(&self) -> &[Vec<u8>] {
        &self.entries
    }

    /// Adds a new entry. Past `KILL_RING_MAX` the oldest entry is dropped first (I-26).
    #[must_use]
    pub fn push(mut self, bytes: Vec<u8>) -> Self {
        if self.entries.len() == KILL_RING_MAX {
            self.entries.remove(0);
        }
        self.entries.push(bytes);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_drops_oldest_past_max() {
        let mut kr = KillRing::new();
        for i in 0..=KILL_RING_MAX {
            kr = kr.push(i.to_string().into_bytes());
        }
        assert_eq!(kr.len(), KILL_RING_MAX);
        assert_eq!(kr.entries()[0], b"1".to_vec());
        assert_eq!(kr.last(), Some(KILL_RING_MAX.to_string().as_bytes()));
    }
}
