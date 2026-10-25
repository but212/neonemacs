//! The kill ring: killed text, newest last, with ring rotation for `yank-pop`.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KillRing {
    entries: Vec<Vec<u8>>,
}

impl KillRing {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub const fn from_entries(entries: Vec<Vec<u8>>) -> Self {
        Self { entries }
    }

    pub fn entries(&self) -> &[Vec<u8>] {
        &self.entries
    }

    pub fn push(mut self, bytes: Vec<u8>) -> Self {
        self.entries.push(bytes);
        self
    }

    pub fn append_last(mut self, bytes: &[u8]) -> Self {
        if let Some(last) = self.entries.last_mut() {
            last.extend_from_slice(bytes);
        } else {
            self.entries.push(bytes.to_vec());
        }
        self
    }

    pub fn rotate_right(mut self, n: usize) -> Self {
        if !self.entries.is_empty() {
            let len = self.entries.len();
            self.entries.rotate_right(n % len);
        }
        self
    }

    pub fn last(&self) -> Option<&[u8]> {
        self.entries.last().map(|v| v.as_slice())
    }

    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub const fn len(&self) -> usize {
        self.entries.len()
    }
}
