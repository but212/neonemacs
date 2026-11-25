//! Buffer and `BufferPool` state (§4, §5, §7).
//! Enforces I-08: at least one buffer exists at all times.

use sha2::{Digest, Sha256};
use std::ops::ControlFlow;
use std::path::Path;

use crate::eol::Eol;
use crate::file::{Backing, DiskState};
use crate::undo::{Edit, RecordedEdit, UndoLog};
use crate::weave_tree::{Doc, Replace};

/// The name of the buffer that always exists and whose unsaved text survives a crash (I-42).
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

const WAL_DELTA_MIN_CAP: usize = 64 * 1024;
pub const WAL_DELTA_RECORD_BYTES: usize = 24;
pub const MAX_WAL_DELTA_RECORDS: usize = 4096;
pub const MAX_WAL_DELTA_BYTES: usize = 4 * 1024 * 1024;
pub type WalBaseDigest = [u8; 32];

/// SHA-256 of the normalized document bytes, independent of tree layout.
#[must_use]
pub fn wal_base_digest(doc: &Doc) -> WalBaseDigest {
    let mut digest = Sha256::new();
    let _ = doc.try_for_each_slice(0..doc.total_bytes(), |chunk| {
        digest.update(chunk);
        ControlFlow::<()>::Continue(())
    });
    digest.finalize().into()
}

/// A delta WAL successfully replayed against its baseline. Records are kept for later WAL writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredDelta {
    doc: Doc,
    eol: Eol,
    deltas: Vec<Replace>,
    base_len: usize,
}

impl RecoveredDelta {
    /// Returns `None` if any record cannot be applied; no partial document escapes.
    #[must_use]
    pub fn replay(base: Doc, eol: Eol, deltas: Vec<Replace>) -> Option<Self> {
        let base_len = base.total_bytes();
        let doc = deltas.iter().try_fold(base, |doc, delta| {
            doc.apply(delta.clone()).ok().map(|(next, _)| next)
        })?;
        Some(Self {
            doc,
            eol,
            deltas,
            base_len,
        })
    }

    #[must_use]
    pub fn into_content(self) -> (Doc, Eol) {
        (self.doc, self.eol)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WalDeltaLog {
    base: WalBaseDigest,
    edits: Vec<Replace>,
    bytes: usize,
}

impl WalDeltaLog {
    const fn empty(base: WalBaseDigest) -> Self {
        Self {
            base,
            edits: Vec::new(),
            bytes: 0,
        }
    }

    fn push(mut self, mut edit: Replace, doc_len: usize) -> Option<Self> {
        edit.insert = edit.insert.compact_small();
        // The retained allocation charge also bounds the serialized insert length.
        let cost = edit
            .insert
            .backing_bytes()
            .saturating_add(WAL_DELTA_RECORD_BYTES);
        let cap = doc_len.clamp(WAL_DELTA_MIN_CAP, MAX_WAL_DELTA_BYTES);
        if self.edits.len() >= MAX_WAL_DELTA_RECORDS || self.bytes.saturating_add(cost) > cap {
            return None;
        }
        self.bytes += cost;
        self.edits.push(edit);
        Some(self)
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
    wal_deltas: Option<WalDeltaLog>,
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
            wal_deltas: None,
        }
    }

    /// Starts delta tracking from the current, verified disk baseline when requested.
    #[must_use]
    pub(crate) fn track_wal_baseline(mut self, enabled: bool) -> Self {
        self.wal_deltas = (enabled
            && matches!(
                self.backing,
                Backing::File {
                    disk: DiskState::Known { .. },
                    ..
                }
            ))
        .then(|| WalDeltaLog::empty(wal_base_digest(&self.text)));
        self
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
    pub fn wal_deltas(&self) -> Option<(WalBaseDigest, &[Replace])> {
        self.wal_deltas
            .as_ref()
            .map(|log| (log.base, log.edits.as_slice()))
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
    pub fn file_path(&self) -> Option<&Path> {
        self.backing.path()
    }

    #[must_use]
    pub const fn is_wal_dirty(&self) -> bool {
        self.backing.is_wal_dirty()
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

    /// Clears `modified` and `wal_dirty` together, so `wal_dirty` never outlives `modified` (I-31).
    #[must_use]
    pub fn mark_clean(mut self) -> Self {
        self.modified = false;
        self.epoch = self.epoch.wrapping_add(1);
        self.backing = self.backing.set_wal_dirty(false);
        // Marking clean alone does not establish a replayable disk baseline.
        self.wal_deltas = None;
        self
    }

    /// Clears `modified` after an undo returned the text to the one it was measured against.
    /// The epoch stays, since that text did not change (I-41).
    #[must_use]
    pub fn restore_clean(mut self) -> Self {
        self.modified = false;
        self.backing = self.backing.set_wal_dirty(false);
        self.wal_deltas = self.wal_deltas.map(|log| WalDeltaLog::empty(log.base));
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
        self.wal_deltas = None;
        self
    }

    /// Replaces `text` and snaps `saved_point` and `mark` to valid unit boundaries (I-01).
    /// The undo history described the old text, so it starts over (I-34).
    #[must_use]
    pub fn with_text(mut self, text: Doc) -> Self {
        self.text = text;
        self.undo = UndoLog::new();
        self.epoch = self.epoch.wrapping_add(1);
        self.wal_deltas = None;
        self.snap_anchors()
    }

    #[must_use]
    pub fn close_undo(mut self, undo_limit: usize) -> Self {
        self.undo = self.undo.close_with_limit(undo_limit);
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
        self.wal_deltas = None;
        self
    }

    #[must_use]
    pub fn replace_from_disk(mut self, text: Doc, backing: Backing, eol: Eol) -> Self {
        self.text = text;
        self.backing = backing;
        self.eol = eol;
        self.modified = false;
        self.epoch = self.epoch.wrapping_add(1);
        self.undo = UndoLog::new();
        self.wal_deltas = None;
        self.snap_anchors()
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

    pub(crate) fn apply_recorded(
        mut self,
        RecordedEdit { edit, span }: RecordedEdit,
        remap_fn: impl Fn(usize) -> usize,
        undo_limit: usize,
    ) -> (Self, Option<usize>) {
        let inverse = match edit {
            Edit::Replace(r) => {
                let forward = self.wal_deltas.is_some().then(|| r.clone());
                match self.text.apply(r) {
                    Ok((next, inv)) => {
                        self.text = next;
                        if let Some(f) = forward {
                            self.wal_deltas = self
                                .wal_deltas
                                .take()
                                .and_then(|log| log.push(f, self.text.total_bytes()));
                        }
                        Edit::Replace(inv)
                    }
                    Err((doc, _err)) => {
                        self.text = doc;
                        return (self, None);
                    }
                }
            }
            Edit::SetEol { from, to } => {
                self.eol = to;
                Edit::SetEol { from: to, to: from }
            }
        };
        let clean_epoch = (!self.modified).then_some(self.epoch);
        self.modified = true;
        self.backing = self.backing.set_wal_dirty(true);
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

    /// Snaps `saved_point` and `mark` to valid UTF-8 unit boundaries within the current text (I-01, I-25).
    #[must_use]
    pub fn snap_anchors(mut self) -> Self {
        self.saved_point = self.text.floor_boundary(self.saved_point);
        self.mark = self.mark.map(|m| self.text.floor_boundary(m));
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

    pub(crate) fn clear_wal_deltas(mut self) -> Self {
        self.buffers
            .iter_mut()
            .for_each(|buffer| buffer.wal_deltas = None);
        self
    }

    /// Restores the content of the buffer visiting `path` from a WAL snapshot (I-23).
    #[must_use]
    pub fn restore_content(
        self,
        path: &Path,
        doc: Doc,
        eol: Eol,
        line: Option<usize>,
    ) -> (Self, Option<BufferId>) {
        self.install_recovered(path, doc, eol, line, None)
    }

    fn install_recovered(
        self,
        path: &Path,
        doc: Doc,
        eol: Eol,
        line: Option<usize>,
        log: Option<WalDeltaLog>,
    ) -> (Self, Option<BufferId>) {
        let found = self.find_by_path(path).map(|b| b.id);
        if let Some(id) = found {
            let pool = self.with_buffer(id, |buf| {
                let mut buf = buf
                    .with_text(doc)
                    .set_eol(eol)
                    .mark_modified()
                    .clear_wal_dirty();
                buf.wal_deltas = log;
                if let Some(n) = line {
                    let line_idx = n.saturating_sub(1).min(buf.text.total_lines() - 1);
                    let pos = buf
                        .text
                        .line_to_offset(line_idx)
                        .expect("line < total_lines");
                    buf = buf.set_saved_point(pos);
                }
                buf
            });
            (pool, Some(id))
        } else {
            (self, None)
        }
    }

    /// Installs a replayed delta WAL, retaining the records if they fit the tracking budget.
    #[must_use]
    pub(crate) fn restore_deltas(
        self,
        path: &Path,
        delta: RecoveredDelta,
        line: Option<usize>,
    ) -> (Self, Option<BufferId>) {
        let log = self
            .find_by_path(path)
            .filter(|buffer| !buffer.modified)
            .and_then(|buffer| buffer.wal_deltas.as_ref())
            .map(|log| WalDeltaLog::empty(log.base));
        let mut len = delta.base_len;
        let log = log.and_then(|log| {
            delta.deltas.into_iter().try_fold(log, |log, edit| {
                // Replay already validated these lengths; charging needs no tree edits.
                len = len - edit.delete_len + edit.insert.total_bytes();
                log.push(edit, len)
            })
        });
        self.install_recovered(path, delta.doc, delta.eol, line, log)
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
        let (pool, id1) = pool.add("a", Doc::default(), Backing::None);
        let (pool, id2) = pool.add("b", Doc::default(), Backing::None);
        assert_eq!(id1, BufferId(1));
        assert_eq!(id2, BufferId(2));
        assert_eq!(pool.len(), 3);
    }

    #[test]
    fn backing_none_never_wal_dirty() {
        let buf = Buffer::new(BufferId(0), "*scratch*", Doc::default(), Backing::None);
        let (buf, _) = buf.apply_edit(Edit::insert(0, b"a".as_slice()), |a| a, 1 << 20);
        assert!(buf.modified());
        assert!(!buf.is_wal_dirty());
    }

    #[test]
    fn mark_clean_clears_wal_dirty() {
        use crate::file::{DiskState, FilePath};
        let backing = Backing::file(
            FilePath::resolve(Path::new("/w"), Path::new("a.txt")),
            DiskState::Unknown,
        );
        let buf = Buffer::new(BufferId(0), "a.txt", Doc::default(), backing);
        let (buf, _) = buf.apply_edit(Edit::insert(0, b"a".as_slice()), |a| a, 1 << 20);
        assert!(buf.modified() && buf.is_wal_dirty());
        let buf = buf.mark_clean();
        assert!(!buf.modified());
        assert!(!buf.is_wal_dirty());
    }

    #[test]
    fn with_text_and_restore_content_invalidate_wal_deltas() {
        use crate::file::{DiskState, FilePath};
        use std::time::SystemTime;

        let path = FilePath::resolve(Path::new("/w"), Path::new("a.txt"));
        let known = DiskState::Known {
            mtime: SystemTime::UNIX_EPOCH,
            size: 4,
        };
        let buf = Buffer::new(
            BufferId(1),
            "a.txt",
            Doc::from_bytes(b"disk"),
            Backing::file(path.clone(), known),
        )
        .track_wal_baseline(true);
        assert_eq!(
            buf.wal_deltas(),
            Some((wal_base_digest(buf.text()), &[][..]))
        );

        let buf = buf.with_text(Doc::from_bytes(b"replaced"));
        assert_eq!(buf.wal_deltas(), None);
        let (buf, _) = buf.apply_edit(Edit::insert(0, b"X".as_slice()), |a| a, 1 << 20);
        assert_eq!(buf.wal_deltas(), None);

        let (pool, id) = BufferPool::new().add(
            "a.txt",
            Doc::from_bytes(b"disk"),
            Backing::file(path.clone(), known),
        );
        let pool = pool.with_buffer(id, |b| b.track_wal_baseline(true));
        assert_eq!(
            pool.get_by_id(id).wal_deltas().map(|(_, edits)| edits),
            Some(&[][..])
        );
        let (pool, restored) = pool.restore_content(
            path.as_path(),
            Doc::from_bytes(b"recovered draft"),
            Eol::Lf,
            None,
        );
        assert_eq!(restored, Some(id));
        assert_eq!(pool.get_by_id(id).wal_deltas(), None);
        let pool = pool.with_buffer(id, |b| {
            b.apply_edit(Edit::insert(15, b"!".as_slice()), |a| a, 1 << 20)
                .0
        });
        assert_eq!(pool.get_by_id(id).wal_deltas(), None);
    }

    #[test]
    fn wal_deltas_degrade_to_none_when_byte_cap_exceeded() {
        use crate::file::{DiskState, FilePath};
        use std::time::SystemTime;

        let backing = Backing::file(
            FilePath::resolve(Path::new("/w"), Path::new("big.txt")),
            DiskState::Known {
                mtime: SystemTime::UNIX_EPOCH,
                size: 0,
            },
        );
        let mut buf =
            Buffer::new(BufferId(1), "big.txt", Doc::default(), backing).track_wal_baseline(true);
        let block = vec![b'a'; 32 * 1024];
        // Insert and delete 32 KiB three times: doc stays empty (cap = 64 KiB),
        // while accumulated delta bytes exceed 64 KiB.
        for _ in 0..3 {
            buf = buf
                .apply_edit(Edit::insert(0, block.as_slice()), |a| a, 1 << 20)
                .0;
            buf = buf
                .apply_edit(Edit::delete(0, block.len()), |a| a, 1 << 20)
                .0;
        }
        assert_eq!(buf.text().total_bytes(), 0);
        assert_eq!(buf.wal_deltas(), None);
    }

    #[test]
    fn wal_delta_budget_compacts_tiny_slices_and_charges_backing_for_larger_slices() {
        use crate::weave_tree::{CHUNK_CAP, MAX_SLICE_BYTES};

        let source = Doc::from_bytes(&vec![b'a'; MAX_SLICE_BYTES]);
        let record = |len| Replace {
            offset: 0,
            delete_len: 0,
            insert: source.slice(0..len).unwrap(),
        };
        let mut log = WalDeltaLog::empty([0; 32]);
        log = log.push(record(1), WAL_DELTA_MIN_CAP).unwrap();
        assert_eq!(log.bytes, WAL_DELTA_RECORD_BYTES + 1);
        assert_eq!(log.edits[0].insert.backing_bytes(), 1);

        // Four slightly larger inserts fit by wire length, but pin >64 KiB of backing.
        for _ in 0..3 {
            log = log.push(record(CHUNK_CAP + 1), WAL_DELTA_MIN_CAP).unwrap();
        }
        assert!(log.push(record(CHUNK_CAP + 1), WAL_DELTA_MIN_CAP).is_none());
    }

    #[test]
    fn invalid_delta_never_produces_recovered_content() {
        let base = Doc::from_bytes("é".as_bytes());
        // Delta 1 succeeds ("Aé"), but delta 2 splits 'é'.
        let partial = vec![
            Replace {
                offset: 0,
                delete_len: 0,
                insert: Doc::from_bytes(b"A"),
            },
            Replace {
                offset: 2,
                delete_len: 1,
                insert: Doc::default(),
            },
        ];
        assert!(RecoveredDelta::replay(base.clone(), Eol::Crlf, partial).is_none());

        // The first record can also fail.
        let oob = vec![Replace {
            offset: 100,
            delete_len: 0,
            insert: Doc::from_bytes(b"x"),
        }];
        assert!(RecoveredDelta::replay(base, Eol::Crlf, oob).is_none());
    }
}
