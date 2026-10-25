//! Buffer and BufferPool state (§4, §5, §7).
//! Enforces I-08: at least one buffer exists at all times.

use std::path::Path;

use crate::core::eol::Eol;
use crate::core::file::{Backing, DiskState};
use crate::core::text::Text;
use crate::core::undo::{Edit, UndoLog};
use crate::core::window::snap_point as snap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BufferId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buffer {
    id: BufferId,
    name: String,
    text: Text,
    backing: Backing,
    undo: UndoLog,
    modified: bool,
    view_only: bool,
    mark: Option<usize>,
    /// Point when this buffer was last left by its window (anchor, I-01).
    saved_point: usize,
    eol: Eol,
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
            eol: Eol::default(),
        }
    }

    pub const fn id(&self) -> BufferId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn text(&self) -> &Text {
        &self.text
    }

    pub const fn backing(&self) -> &Backing {
        &self.backing
    }

    pub const fn undo(&self) -> &UndoLog {
        &self.undo
    }

    pub const fn modified(&self) -> bool {
        self.modified
    }

    pub const fn view_only(&self) -> bool {
        self.view_only
    }

    pub const fn mark(&self) -> Option<usize> {
        self.mark
    }

    pub const fn saved_point(&self) -> usize {
        self.saved_point
    }

    pub const fn eol(&self) -> Eol {
        self.eol
    }

    /// Returns the file path if this buffer is backed by a disk file.
    pub fn file_path(&self) -> Option<&Path> {
        self.backing.path()
    }

    pub const fn is_wal_dirty(&self) -> bool {
        self.backing.is_wal_dirty()
    }

    /// Sets `mark`, snapped to a valid unit boundary in `0..=text.len()` (I-01).
    pub fn set_mark(mut self, mark: Option<usize>) -> Self {
        self.mark = mark.map(|m| snap(&self.text, m));
        self
    }

    /// Sets `saved_point`, snapped to a valid unit boundary in `0..=text.len()` (I-01).
    pub fn set_saved_point(mut self, pos: usize) -> Self {
        self.saved_point = snap(&self.text, pos);
        self
    }

    pub const fn set_view_only(mut self, view_only: bool) -> Self {
        self.view_only = view_only;
        self
    }

    pub const fn set_modified(mut self, modified: bool) -> Self {
        self.modified = modified;
        self
    }

    pub const fn set_eol(mut self, eol: Eol) -> Self {
        self.eol = eol;
        self
    }

    pub fn set_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn set_backing(mut self, backing: Backing) -> Self {
        self.backing = backing;
        self
    }

    /// Replaces `text` and snaps `saved_point` and `mark` to valid unit boundaries (I-01).
    pub fn with_text(mut self, text: Text) -> Self {
        self.text = text;
        self.snap_anchors()
    }

    pub fn with_undo(mut self, undo: UndoLog) -> Self {
        self.undo = undo;
        self
    }

    pub fn close_undo(mut self) -> Self {
        self.undo = self.undo.close();
        self
    }

    pub fn clear_wal_dirty(mut self) -> Self {
        self.backing = self.backing.set_wal_dirty(false);
        self
    }

    pub fn record_saved(mut self, new_disk: DiskState) -> Self {
        self.backing = self.backing.with_disk(new_disk).set_wal_dirty(false);
        self.modified = false;
        self.undo = self.undo.close();
        self
    }

    pub fn replace_from_disk(
        mut self,
        text: Text,
        backing: Backing,
        eol: Eol,
        undo_limit: usize,
    ) -> Self {
        self.text = text;
        self.backing = backing;
        self.eol = eol;
        self.modified = false;
        self.undo = UndoLog::new(undo_limit);
        self.snap_anchors()
    }

    pub fn apply_edit(mut self, edit: Edit, remap_fn: impl Fn(usize) -> usize) -> (Self, usize) {
        match &edit {
            Edit::Insert { pos, bytes } => self.text = self.text.insert(*pos, bytes),
            Edit::Delete { pos, bytes } => {
                self.text = self.text.delete(*pos..*pos + bytes.len());
            }
            Edit::SetEol { to, .. } => self.eol = *to,
        }
        self.modified = true;
        self.backing = self.backing.set_wal_dirty(true);
        let anchor = |a| snap(&self.text, remap_fn(a));
        self.mark = self.mark.map(anchor);
        self.saved_point = anchor(self.saved_point);
        let (undo, dropped) = self.undo.record(edit);
        self.undo = undo;
        (self, dropped)
    }

    /// Snaps `saved_point` and `mark` to valid UTF-8 unit boundaries within the current text (I-01, I-25).
    pub fn snap_anchors(mut self) -> Self {
        self.saved_point = snap(&self.text, self.saved_point);
        self.mark = self.mark.map(|m| snap(&self.text, m));
        self
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

    pub fn find_by_id(&self, id: BufferId) -> Option<&Buffer> {
        self.buffers.iter().find(|b| b.id == id)
    }

    pub fn get_by_id(&self, id: BufferId) -> &Buffer {
        &self.buffers[self.index_of(id)]
    }

    pub fn find_by_path(&self, path: &Path) -> Option<&Buffer> {
        self.buffers.iter().find(|b| b.file_path() == Some(path))
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

    /// Restores the content of the buffer visiting `path` from WAL bytes and EOL (I-23).
    pub fn restore_content(
        self,
        path: &Path,
        bytes: &[u8],
        eol: Eol,
        line: Option<usize>,
    ) -> (Self, Option<BufferId>) {
        let found = self.find_by_path(path).map(|b| b.id);
        if let Some(id) = found {
            let pool = self.with_buffer(id, |buf| {
                let limit = buf.undo.limit();
                let mut buf = buf
                    .with_text(Text::from_bytes(bytes))
                    .set_eol(eol)
                    .set_modified(true)
                    .clear_wal_dirty()
                    .with_undo(UndoLog::new(limit));
                if let Some(n) = line {
                    let pos = buf.text.nth_line_start(n);
                    buf = buf.set_saved_point(pos);
                }
                buf
            });
            (pool, Some(id))
        } else {
            (self, None)
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
        assert_eq!(pool.get(0).unwrap().name(), "*scratch*");
        assert_eq!(pool.get(0).unwrap().id(), BufferId(0));
    }

    #[test]
    fn buffer_pool_kill_last_creates_scratch_enforcing_i08() {
        let pool = BufferPool::new(100);
        let (pool, replacement) = pool.kill(BufferId(0), 100);
        assert_eq!(pool.len(), 1);
        assert_ne!(replacement, BufferId(0));
        assert_eq!(pool.find_by_id(replacement).unwrap().name(), "*scratch*");
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

    #[test]
    fn backing_none_never_wal_dirty() {
        let buf = Buffer::new(
            BufferId(0),
            "*scratch*",
            Text::default(),
            Backing::None,
            100,
        );
        let (buf, _) = buf.apply_edit(
            Edit::Insert {
                pos: 0,
                bytes: b"a".to_vec(),
            },
            |a| a,
        );
        assert!(buf.modified());
        assert!(!buf.is_wal_dirty());
    }
}
