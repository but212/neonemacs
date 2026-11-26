//! Buffer and `BufferPool` state (§4, §5, §7).
//! Enforces I-08: at least one buffer exists at all times.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::Doc;
use crate::eol::Eol;
use crate::file::{Backing, DiskState};
use crate::syntax::{LexState, SyntaxDef, highlight_line_with_state};
use crate::undo::{Edit, RecordedEdit, UndoLog};

/// The name of the buffer that always exists (I-08, I-42).
pub const SCRATCH_NAME: &str = "*scratch*";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BufferId(pub u32);

/// The mark of a buffer. Only an active mark makes a region, which is shown on screen. A text
/// edit, `C-g`, or a copy turns an active mark into an inactive one, which stays for
/// `exchange-point-and-mark` and Lua (I-47).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Unset,
    Inactive(usize),
    Active(usize),
}

impl Mark {
    const fn pos(self) -> Option<usize> {
        match self {
            Self::Unset => None,
            Self::Inactive(p) | Self::Active(p) => Some(p),
        }
    }

    const fn deactivated(self) -> Self {
        match self {
            Self::Active(p) => Self::Inactive(p),
            other => other,
        }
    }

    fn map(self, f: impl FnOnce(usize) -> usize) -> Self {
        match self {
            Self::Unset => Self::Unset,
            Self::Inactive(p) => Self::Inactive(f(p)),
            Self::Active(p) => Self::Active(f(p)),
        }
    }
}

/// Per-line end-of-line `LexState` cache for multi-line syntax highlighting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyntaxStateCache {
    line_end_states: Vec<LexState>,
    dirty_from: usize,
}

impl SyntaxStateCache {
    #[must_use]
    pub fn start_state(&self, line: usize) -> Option<LexState> {
        if line == 0 {
            Some(LexState::Normal)
        } else if line <= self.dirty_from && line <= self.line_end_states.len() {
            Some(self.line_end_states[line - 1])
        } else {
            None
        }
    }

    #[must_use]
    pub fn invalidate_from(mut self, line: usize) -> Self {
        self.dirty_from = self.dirty_from.min(line);
        self
    }

    #[must_use]
    pub fn ensure_through(mut self, doc: &Doc, syntax: &SyntaxDef, through_line: usize) -> Self {
        let total_lines = crate::total_lines(doc);
        let target = through_line.min(total_lines);
        if self.dirty_from >= target && self.line_end_states.len() >= target {
            return self;
        }
        let start_line = self.dirty_from.min(self.line_end_states.len());
        self.line_end_states.truncate(start_line);
        let mut state = if start_line == 0 {
            LexState::Normal
        } else {
            self.line_end_states[start_line - 1]
        };
        let total = doc.total_bytes();
        let mut start = crate::line_to_offset(doc, start_line).unwrap_or(total);
        for line in start_line..target {
            let next = if line + 1 < total_lines {
                crate::line_to_offset(doc, line + 1).ok()
            } else {
                None
            };
            let end = next.map_or(total, |offset| offset.saturating_sub(1));
            let (_, next_state) = highlight_line_with_state(Some(syntax), doc, start..end, state);
            self.line_end_states.push(next_state);
            state = next_state;
            start = next.unwrap_or(total);
        }
        self.dirty_from = target;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buffer {
    id: BufferId,
    name: String,
    text: Doc,
    backing: Backing,
    undo: UndoLog,
    modified: bool,
    /// Counts changes of the text that `modified` is measured against (I-41).
    epoch: u32,
    view_only: bool,
    mark: Mark,
    /// Point when this buffer was last left by its window (anchor, I-01).
    saved_point: usize,
    eol: Eol,
    syntax_cache: SyntaxStateCache,
}

impl Buffer {
    #[must_use]
    pub fn new(id: BufferId, name: &str, text: Doc, backing: Backing) -> Self {
        Self {
            id,
            name: name.to_owned(),
            text,
            backing,
            undo: UndoLog::new(),
            modified: false,
            epoch: 0,
            view_only: false,
            mark: Mark::Unset,
            saved_point: 0,
            eol: Eol::default(),
            syntax_cache: SyntaxStateCache::default(),
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
    pub const fn text(&self) -> &Doc {
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
        self.mark.pos()
    }

    /// Whether the mark makes a region between it and point (I-47).
    #[must_use]
    pub const fn mark_active(&self) -> bool {
        matches!(self.mark, Mark::Active(_))
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
    pub fn file_path(&self) -> Option<&str> {
        self.backing.path()
    }

    /// Sets `mark`, snapped to a valid unit boundary in `0..=text.total_bytes()` (I-01).
    #[must_use]
    pub fn set_mark(mut self, mark: Option<usize>) -> Self {
        self.mark = match mark {
            Some(m) => Mark::Active(self.text.floor_boundary(m)),
            None => Mark::Unset,
        };
        self
    }

    /// Ends the region and keeps the mark position.
    #[must_use]
    pub const fn deactivate_mark(mut self) -> Self {
        self.mark = self.mark.deactivated();
        self
    }

    /// Sets `saved_point`, snapped to a valid unit boundary in `0..=text.total_bytes()` (I-01).
    #[must_use]
    pub fn set_saved_point(mut self, pos: usize) -> Self {
        self.saved_point = self.text.floor_boundary(pos);
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

    #[must_use]
    pub fn mark_clean(mut self) -> Self {
        self.modified = false;
        self.epoch = self.epoch.wrapping_add(1);
        self
    }

    /// Clears `modified` after an undo returned the text to the one it was measured against.
    /// The epoch stays, since that text did not change (I-41).
    #[must_use]
    pub fn restore_clean(mut self) -> Self {
        self.modified = false;
        self
    }

    #[must_use]
    pub const fn set_eol(mut self, eol: Eol) -> Self {
        self.eol = eol;
        self
    }

    #[must_use]
    pub fn syntax_start_state(&self, line: usize) -> Option<LexState> {
        self.syntax_cache.start_state(line)
    }

    #[must_use]
    pub fn ensure_syntax_cache(mut self, syntax: &SyntaxDef, through_line: usize) -> Self {
        self.syntax_cache = self
            .syntax_cache
            .ensure_through(&self.text, syntax, through_line);
        self
    }

    #[must_use]
    pub fn clear_syntax_cache(mut self) -> Self {
        self.syntax_cache = SyntaxStateCache::default();
        self
    }

    #[must_use]
    pub fn set_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self.syntax_cache = SyntaxStateCache::default();
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
    pub fn with_text(mut self, text: Doc) -> Self {
        self.text = text;
        self.undo = UndoLog::new();
        self.epoch = self.epoch.wrapping_add(1);
        self.saved_point = self.text.floor_boundary(self.saved_point);
        self.mark = self.mark.map(|m| self.text.floor_boundary(m));
        self.syntax_cache = SyntaxStateCache::default();
        self
    }

    #[must_use]
    pub fn close_undo(mut self, undo_limit: usize) -> Self {
        self.undo = self.undo.close_with_limit(undo_limit);
        self
    }

    #[must_use]
    pub fn record_saved(mut self, new_disk: DiskState) -> Self {
        self.backing = self.backing.with_disk(new_disk);
        self.modified = false;
        self.epoch = self.epoch.wrapping_add(1);
        self.undo = self.undo.close();
        self
    }

    #[must_use]
    pub fn replace_from_disk(mut self, text: Doc, backing: Backing, eol: Eol) -> Self {
        self.backing = backing;
        self.eol = eol;
        self.modified = false;
        self.with_text(text)
    }

    /// Applies `edit` and records its inverse for undo (I-35). Returns `None` for an invalid edit.
    pub fn apply_edit(
        self,
        edit: Edit,
        remap_fn: impl Fn(usize) -> usize,
        undo_limit: usize,
    ) -> (Self, Option<usize>) {
        self.apply_recorded(edit.into(), remap_fn, undo_limit)
    }

    fn apply_text_edit(
        mut self,
        f: impl FnOnce(Doc) -> Result<(Doc, crate::Replace), (Doc, crate::DocError)>,
    ) -> (Self, Option<Edit>) {
        match f(self.text) {
            Ok((next, inv)) => {
                self.text = next;
                (self, Some(Edit::Replace(inv)))
            }
            Err((doc, _err)) => {
                self.text = doc;
                (self, None)
            }
        }
    }

    pub(crate) fn apply_recorded(
        mut self,
        RecordedEdit { edit, span }: RecordedEdit,
        remap_fn: impl Fn(usize) -> usize,
        undo_limit: usize,
    ) -> (Self, Option<usize>) {
        let (inverse, edit_offset) = match edit {
            Edit::Replace(r) => {
                let off = r.offset;
                let (next_self, inv) = self.apply_text_edit(|doc| doc.apply(r));
                let Some(inv) = inv else {
                    return (next_self, None);
                };
                self = next_self;
                (inv, Some(off))
            }
            Edit::InsertInline { offset, data } => {
                let (next_self, inv) =
                    self.apply_text_edit(|doc| doc.replace(offset, 0, data.as_slice()));
                let Some(inv) = inv else {
                    return (next_self, None);
                };
                self = next_self;
                (inv, Some(offset))
            }
            Edit::SetEol { from, to } => {
                self.eol = to;
                (Edit::SetEol { from: to, to: from }, None)
            }
        };
        if let Some(off) = edit_offset {
            let line = crate::line_at(&self.text, off.min(self.text.total_bytes()));
            self.syntax_cache = self.syntax_cache.invalidate_from(line);
        }
        let clean_epoch = (!self.modified).then_some(self.epoch);
        self.modified = true;
        let anchor = |a| self.text.floor_boundary(remap_fn(a));
        self.mark = self.mark.deactivated().map(anchor);
        self.saved_point = anchor(self.saved_point);
        let inverse = RecordedEdit {
            edit: inverse,
            span: span.inverse(),
        };
        let (undo, dropped) = self.undo.record(inverse, undo_limit, clean_epoch);
        self.undo = undo;
        (self, Some(dropped))
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
            Doc::default(),
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

    pub fn iter(&self) -> core::slice::Iter<'_, Buffer> {
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
    pub fn get_by_id(&self, id: BufferId) -> &Buffer {
        &self.buffers[self.index_of(id)]
    }

    #[must_use]
    pub fn find_by_path(&self, path: &str) -> Option<&Buffer> {
        self.buffers.iter().find(|b| b.file_path() == Some(path))
    }

    #[must_use]
    pub fn find_by_name(&self, name: &str) -> Option<&Buffer> {
        self.buffers.iter().find(|b| b.name() == name)
    }

    /// The buffer named `name` that visits no file: one that the editor fills itself, such as
    /// `*init-log*`. A buffer that visits a file is never one, even when the file has that name.
    #[must_use]
    pub fn find_unbacked(&self, name: &str) -> Option<&Buffer> {
        self.buffers
            .iter()
            .find(|b| b.name() == name && b.file_path().is_none())
    }

    /// The buffer that the name `name` means to the user. Among buffers with the same name, the
    /// one without a file comes first: a file named like `*init-log*` must not hide the buffer
    /// that the editor fills.
    #[must_use]
    pub fn find_named(&self, name: &str) -> Option<&Buffer> {
        self.find_unbacked(name).or_else(|| self.find_by_name(name))
    }

    /// Adds a buffer with an automatically assigned unique id. Ids are never reused.
    #[must_use]
    pub fn add(mut self, name: &str, text: Doc, backing: Backing) -> (Self, BufferId) {
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

    #[must_use]
    pub fn clear_syntax_caches(mut self) -> Self {
        self.buffers = self
            .buffers
            .into_iter()
            .map(Buffer::clear_syntax_cache)
            .collect();
        self
    }

    /// Removes buffer `id`. If `id` was the only buffer, an empty `*scratch*` buffer is created first.
    ///
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
                Doc::default(),
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
    type IntoIter = core::slice::Iter<'a, Buffer>;

    fn into_iter(self) -> Self::IntoIter {
        self.buffers.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_pool_starts_with_scratch_and_kill_last_recreates_it() {
        // I-08, I-42
        let pool = BufferPool::new();
        assert_eq!(pool.len(), 1);
        let first = pool.iter().next().unwrap();
        assert_eq!(first.name(), SCRATCH_NAME);
        assert_eq!(first.id(), BufferId(0));
        assert!(first.backing().is_scratch());

        let (pool, replacement) = pool.kill(BufferId(0));
        assert_eq!(pool.len(), 1);
        assert_ne!(replacement, BufferId(0));
        assert_eq!(pool.get_by_id(replacement).name(), SCRATCH_NAME);
        assert!(pool.get_by_id(replacement).backing().is_scratch());
    }

    #[test]
    fn buffer_pool_unique_ids() {
        let pool = BufferPool::new();
        let (pool, id1) = pool.add("a", Doc::default(), Backing::None);
        let (pool, id2) = pool.add("b", Doc::default(), Backing::None);
        assert_eq!(id1, BufferId(1));
        assert_eq!(id2, BufferId(2));
        assert_eq!(pool.len(), 3);
    }
}
