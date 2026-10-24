//! Buffer and BufferPool state (§4, §5, §7).
//! Enforces I-08: at least one buffer exists at all times.

use std::path::Path;

use crate::core::file::Backing;
use crate::core::text::Text;
use crate::core::undo::UndoLog;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct BufferId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buffer {
    pub id: BufferId,
    pub name: String,
    pub text: Text,
    pub backing: Backing,
    pub undo: UndoLog,
    pub modified: bool,
    pub view_only: bool,
    pub mark: Option<usize>,
    /// Point when this buffer was last left by its window (anchor, I-01).
    pub saved_point: usize,
}

impl Buffer {
    pub fn new(id: BufferId, name: &str, text: Text, backing: Backing, undo_limit: usize) -> Self {
        Self {
            id,
            name: name.to_owned(),
            text,
            backing,
            undo: UndoLog::new(undo_limit),
            modified: false,
            view_only: false,
            mark: None,
            saved_point: 0,
        }
    }
}

/// A collection of buffers enforcing the invariant that at least one buffer exists (I-08).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferPool {
    buffers: Vec<Buffer>,
    next_id: u32,
}

impl BufferPool {
    pub fn new(undo_limit: usize) -> Self {
        let scratch = Buffer::new(
            BufferId(0),
            "*scratch*",
            Text::default(),
            Backing::None,
            undo_limit,
        );
        Self {
            buffers: vec![scratch],
            next_id: 1,
        }
    }

    pub const fn len(&self) -> usize {
        self.buffers.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.buffers.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Buffer> {
        self.buffers.iter()
    }

    pub fn as_slice(&self) -> &[Buffer] {
        &self.buffers
    }

    pub fn index_of(&self, id: BufferId) -> usize {
        self.buffers
            .iter()
            .position(|b| b.id == id)
            .expect("window refers to a live buffer (I-08)")
    }

    pub fn get(&self, index: usize) -> Option<&Buffer> {
        self.buffers.get(index)
    }

    pub fn get_by_index(&self, index: usize) -> Option<&Buffer> {
        self.buffers.get(index)
    }

    pub fn find_by_id(&self, id: BufferId) -> Option<&Buffer> {
        self.buffers.iter().find(|b| b.id == id)
    }

    pub fn get_by_id(&self, id: BufferId) -> &Buffer {
        &self.buffers[self.index_of(id)]
    }

    pub fn find_by_name(&self, name: &str) -> Option<&Buffer> {
        self.buffers.iter().find(|b| b.name == name)
    }

    pub fn find_by_path(&self, path: &Path) -> Option<&Buffer> {
        self.buffers.iter().find(|b| match &b.backing {
            Backing::File { path: p, .. } => p == path,
            Backing::None => false,
        })
    }

    pub fn any_modified(&self) -> bool {
        self.buffers.iter().any(|b| b.modified)
    }

    /// Adds a buffer with an automatically assigned unique id. Ids are never reused.
    pub fn add(
        mut self,
        name: &str,
        text: Text,
        backing: Backing,
        undo_limit: usize,
    ) -> (Self, BufferId) {
        let id = BufferId(self.next_id);
        self.next_id += 1;
        self.buffers
            .push(Buffer::new(id, name, text, backing, undo_limit));
        (self, id)
    }

    /// Replaces the buffer with `id` by `f` applied to it. Panics when `id` is unknown (I-20).
    pub fn with_buffer(mut self, id: BufferId, f: impl FnOnce(Buffer) -> Buffer) -> Self {
        let i = self.index_of(id);
        let buf = self.buffers.remove(i);
        self.buffers.insert(i, f(buf));
        self
    }

    /// Restores the content of the buffer visiting `path` from WAL bytes (I-23).
    pub fn restore_content(self, path: &Path, bytes: &[u8]) -> Self {
        let found = self.find_by_path(path).map(|b| b.id);
        if let Some(id) = found {
            self.with_buffer(id, |mut buf| {
                buf.text = Text::from_bytes(bytes);
                buf.modified = true;
                buf
            })
        } else {
            self
        }
    }

    /// Removes buffer `id`. If `id` was the only buffer, an empty `*scratch*` buffer is created first.
    /// Returns the new pool and the `BufferId` of the replacement buffer that windows should switch to (I-08).
    pub fn kill(mut self, id: BufferId, undo_limit: usize) -> (Self, BufferId) {
        if self.buffers.len() == 1 {
            let scratch_id = BufferId(self.next_id);
            self.next_id += 1;
            self.buffers.push(Buffer::new(
                scratch_id,
                "*scratch*",
                Text::default(),
                Backing::None,
                undo_limit,
            ));
        }
        let replacement = self
            .buffers
            .iter()
            .map(|b| b.id)
            .find(|&b| b != id)
            .expect("another buffer exists (I-08)");
        self.buffers.retain(|b| b.id != id);
        (self, replacement)
    }
}

impl<'a> IntoIterator for &'a BufferPool {
    type Item = &'a Buffer;
    type IntoIter = std::slice::Iter<'a, Buffer>;

    fn into_iter(self) -> Self::IntoIter {
        self.buffers.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_pool_starts_with_scratch() {
        let pool = BufferPool::new(100);
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.get_by_index(0).unwrap().name, "*scratch*");
        assert_eq!(pool.get_by_index(0).unwrap().id, BufferId(0));
    }

    #[test]
    fn buffer_pool_kill_last_creates_scratch_enforcing_i08() {
        let pool = BufferPool::new(100);
        let (pool, replacement) = pool.kill(BufferId(0), 100);
        assert_eq!(pool.len(), 1);
        assert_ne!(replacement, BufferId(0));
        assert_eq!(pool.find_by_id(replacement).unwrap().name, "*scratch*");
    }

    #[test]
    fn buffer_pool_unique_ids() {
        let pool = BufferPool::new(100);
        let (pool, id1) = pool.add("a", Text::default(), Backing::None, 100);
        let (pool, id2) = pool.add("b", Text::default(), Backing::None, 100);
        assert_eq!(id1, BufferId(1));
        assert_eq!(id2, BufferId(2));
        assert_eq!(pool.len(), 3);
    }
}
