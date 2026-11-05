//! Buffer and `BufferPool` state (§4, §5, §7).
//! Enforces I-08: at least one buffer exists at all times.

use std::path::Path;

use crate::core::eol::Eol;
use crate::core::file::{Backing, DiskState};
use crate::core::text::Text;
use crate::core::undo::{Edit, UndoLog};
use crate::core::window::snap_point as snap;

/// The name of the buffer that always exists and whose unsaved text survives a crash (I-42).
pub const SCRATCH_NAME: &str = "*scratch*";

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
    /// Counts changes of the text that `modified` is measured against (I-41).
    epoch: u32,
    view_only: bool,
    mark: Option<usize>,
    /// Point when this buffer was last left by its window (anchor, I-01).
    saved_point: usize,
    eol: Eol,
}

impl Buffer {
    #[must_use]
    pub fn new(id: BufferId, name: &str, text: Text, backing: Backing) -> Self {
        Self {
            id,
            name: name.to_owned(),
            text,
            backing,
            undo: UndoLog::new(),
            modified: false,
            epoch: 0,
            view_only: false,
            mark: None,
            saved_point: 0,
            eol: Eol::default(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> BufferId {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn text(&self) -> &Text {
        &self.text
    }

    #[must_use]
    pub const fn backing(&self) -> &Backing {
        &self.backing
    }

    #[must_use]
    pub const fn undo(&self) -> &UndoLog {
        &self.undo
    }

    #[must_use]
    pub const fn modified(&self) -> bool {
        self.modified
    }

    /// The save epoch: it changes whenever the text that `modified` compares against changes (I-41).
    #[must_use]
    pub const fn epoch(&self) -> u32 {
        self.epoch
    }

    #[must_use]
    pub const fn view_only(&self) -> bool {
        self.view_only
    }

    #[must_use]
    pub const fn mark(&self) -> Option<usize> {
        self.mark
    }

    #[must_use]
    pub const fn saved_point(&self) -> usize {
        self.saved_point
    }

    #[must_use]
    pub const fn eol(&self) -> Eol {
        self.eol
    }

    /// Returns the file path if this buffer is backed by a disk file.
    #[must_use]
    pub fn file_path(&self) -> Option<&Path> {
        self.backing.path()
    }

    #[must_use]
    pub const fn is_wal_dirty(&self) -> bool {
        self.backing.is_wal_dirty()
    }

    /// Sets `mark`, snapped to a valid unit boundary in `0..=text.len()` (I-01).
    #[must_use]
    pub fn set_mark(mut self, mark: Option<usize>) -> Self {
        self.mark = mark.map(|m| snap(&self.text, m));
        self
    }

    /// Sets `saved_point`, snapped to a valid unit boundary in `0..=text.len()` (I-01).
    #[must_use]
    pub fn set_saved_point(mut self, pos: usize) -> Self {
        self.saved_point = snap(&self.text, pos);
        self
    }

    #[must_use]
    pub const fn set_view_only(mut self, view_only: bool) -> Self {
        self.view_only = view_only;
        self
    }

    #[must_use]
    pub const fn mark_modified(mut self) -> Self {
        self.modified = true;
        self
    }

    /// Clears `modified` and `wal_dirty` together, so `wal_dirty` never outlives `modified` (I-31).
    #[must_use]
    pub fn mark_clean(mut self) -> Self {
        self.modified = false;
        self.epoch = self.epoch.wrapping_add(1);
        self.backing = self.backing.set_wal_dirty(false);
        self
    }

    /// Clears `modified` after an undo returned the text to the one it was measured against.
    /// The epoch stays, since that text did not change (I-41).
    #[must_use]
    pub fn restore_clean(mut self) -> Self {
        self.modified = false;
        self.backing = self.backing.set_wal_dirty(false);
        self
    }

    #[must_use]
    pub const fn set_eol(mut self, eol: Eol) -> Self {
        self.eol = eol;
        self
    }

    #[must_use]
    pub fn set_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    #[must_use]
    pub fn set_backing(mut self, backing: Backing) -> Self {
        self.backing = backing;
        self
    }

    /// Replaces `text` and snaps `saved_point` and `mark` to valid unit boundaries (I-01).
    /// The undo history described the old text, so it starts over (I-34).
    #[must_use]
    pub fn with_text(mut self, text: Text) -> Self {
        self.text = text;
        self.undo = UndoLog::new();
        self.epoch = self.epoch.wrapping_add(1);
        self.snap_anchors()
    }

    #[must_use]
    pub fn close_undo(mut self) -> Self {
        self.undo = self.undo.close();
        self
    }

    #[must_use]
    pub fn clear_wal_dirty(mut self) -> Self {
        self.backing = self.backing.set_wal_dirty(false);
        self
    }

    #[must_use]
    pub fn record_saved(mut self, new_disk: DiskState) -> Self {
        self.backing = self.backing.with_disk(new_disk).set_wal_dirty(false);
        self.modified = false;
        self.epoch = self.epoch.wrapping_add(1);
        self.undo = self.undo.close();
        self
    }

    #[must_use]
    pub fn replace_from_disk(mut self, text: Text, backing: Backing, eol: Eol) -> Self {
        self.text = text;
        self.backing = backing;
        self.eol = eol;
        self.modified = false;
        self.epoch = self.epoch.wrapping_add(1);
        self.undo = UndoLog::new();
        self.snap_anchors()
    }

    /// Applies `edit` and records it for undo, keeping at most `undo_limit` bytes (I-35).
    pub fn apply_edit(
        mut self,
        edit: Edit,
        remap_fn: impl Fn(usize) -> usize,
        undo_limit: usize,
    ) -> (Self, usize) {
        match &edit {
            Edit::Replace {
                pos,
                removed,
                inserted,
            } => {
                self.text = self
                    .text
                    .delete(*pos..*pos + removed.len())
                    .insert(*pos, inserted);
            }
            Edit::SetEol { to, .. } => self.eol = *to,
        }
        let clean_epoch = (!self.modified).then_some(self.epoch);
        self.modified = true;
        self.backing = self.backing.set_wal_dirty(true);
        let anchor = |a| snap(&self.text, remap_fn(a));
        self.mark = self.mark.map(anchor);
        self.saved_point = anchor(self.saved_point);
        let (undo, dropped) = self.undo.record(edit, undo_limit, clean_epoch);
        self.undo = undo;
        (self, dropped)
    }

    /// Snaps `saved_point` and `mark` to valid UTF-8 unit boundaries within the current text (I-01, I-25).
    #[must_use]
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

impl Default for BufferPool {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferPool {
    #[must_use]
    pub fn new() -> Self {
        let scratch = Buffer::new(
            BufferId(0),
            SCRATCH_NAME,
            Text::default(),
            Backing::scratch(),
        );
        Self {
            buffers: vec![scratch],
            next_id: 1,
        }
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.buffers.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.buffers.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Buffer> {
        self.buffers.iter()
    }

    /// Returns the pool index of buffer `id`.
    ///
    /// # Panics
    /// Panics if `id` is not present in the pool (violating I-08).
    #[must_use]
    pub fn index_of(&self, id: BufferId) -> usize {
        self.buffers
            .iter()
            .position(|b| b.id == id)
            .expect("window refers to a live buffer (I-08)")
    }

    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Buffer> {
        self.buffers.get(index)
    }

    #[must_use]
    pub fn get_by_id(&self, id: BufferId) -> &Buffer {
        &self.buffers[self.index_of(id)]
    }

    #[must_use]
    pub fn find_by_path(&self, path: &Path) -> Option<&Buffer> {
        self.buffers.iter().find(|b| b.file_path() == Some(path))
    }

    #[must_use]
    pub fn find_by_name(&self, name: &str) -> Option<&Buffer> {
        self.buffers.iter().find(|b| b.name() == name)
    }

    /// Adds a buffer with an automatically assigned unique id. Ids are never reused.
    #[must_use]
    pub fn add(mut self, name: &str, text: Text, backing: Backing) -> (Self, BufferId) {
        let id = BufferId(self.next_id);
        self.next_id += 1;
        self.buffers.push(Buffer::new(id, name, text, backing));
        (self, id)
    }

    /// Replaces the buffer with `id` by `f` applied to it. Panics when `id` is unknown (I-20).
    #[must_use]
    pub fn with_buffer(mut self, id: BufferId, f: impl FnOnce(Buffer) -> Buffer) -> Self {
        let i = self.index_of(id);
        let buf = self.buffers.remove(i);
        self.buffers.insert(i, f(buf));
        self
    }

    /// Restores the content of the buffer visiting `path` from WAL bytes and EOL (I-23).
    #[must_use]
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
                let mut buf = buf
                    .with_text(Text::from_bytes(bytes))
                    .set_eol(eol)
                    .mark_modified()
                    .clear_wal_dirty();
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
    ///
    /// # Panics
    /// Panics if no replacement buffer exists (violating I-08).
    #[must_use]
    pub fn kill(mut self, id: BufferId) -> (Self, BufferId) {
        if self.buffers.len() == 1 {
            let scratch_id = BufferId(self.next_id);
            self.next_id += 1;
            self.buffers.push(Buffer::new(
                scratch_id,
                SCRATCH_NAME,
                Text::default(),
                Backing::scratch(),
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
    fn the_pool_scratch_is_scratch_backed_before_and_after_a_kill() {
        // I-42
        let pool = BufferPool::new();
        assert!(pool.get(0).unwrap().backing().is_scratch());
        let (pool, replacement) = pool.kill(BufferId(0));
        assert_eq!(pool.get_by_id(replacement).name(), SCRATCH_NAME);
        assert!(pool.get_by_id(replacement).backing().is_scratch());
    }

    #[test]
    fn buffer_pool_starts_with_scratch() {
        let pool = BufferPool::new();
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.get(0).unwrap().name(), "*scratch*");
        assert_eq!(pool.get(0).unwrap().id(), BufferId(0));
    }

    #[test]
    fn buffer_pool_kill_last_creates_scratch_enforcing_i08() {
        let pool = BufferPool::new();
        let (pool, replacement) = pool.kill(BufferId(0));
        assert_eq!(pool.len(), 1);
        assert_ne!(replacement, BufferId(0));
        assert_eq!(pool.get_by_id(replacement).name(), "*scratch*");
    }

    #[test]
    fn buffer_pool_unique_ids() {
        let pool = BufferPool::new();
        let (pool, id1) = pool.add("a", Text::default(), Backing::None);
        let (pool, id2) = pool.add("b", Text::default(), Backing::None);
        assert_eq!(id1, BufferId(1));
        assert_eq!(id2, BufferId(2));
        assert_eq!(pool.len(), 3);
    }

    #[test]
    fn backing_none_never_wal_dirty() {
        let buf = Buffer::new(BufferId(0), "*scratch*", Text::default(), Backing::None);
        let (buf, _) = buf.apply_edit(Edit::insert(0, b"a".as_slice()), |a| a, 1 << 20);
        assert!(buf.modified());
        assert!(!buf.is_wal_dirty());
    }

    #[test]
    fn mark_clean_clears_wal_dirty() {
        use crate::core::file::{DiskState, FilePath};
        let backing = Backing::file(
            FilePath::resolve(Path::new("/w"), Path::new("a.txt")),
            DiskState::Unknown,
        );
        let buf = Buffer::new(BufferId(0), "a.txt", Text::default(), backing);
        let (buf, _) = buf.apply_edit(Edit::insert(0, b"a".as_slice()), |a| a, 1 << 20);
        assert!(buf.modified() && buf.is_wal_dirty());
        let buf = buf.mark_clean();
        assert!(!buf.modified());
        assert!(!buf.is_wal_dirty());
    }
}
