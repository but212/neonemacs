//! Undo log in groups, and its persistent format v1 (§7, I-10).

use crate::core::file::DiskState;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const MAGIC: &[u8; 8] = b"NEONUNDO";
const VERSION: u8 = 1;
const TAG_BOUNDARY: u8 = 0;
const TAG_INSERT: u8 = 1;
const TAG_DELETE: u8 = 2;

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

    /// Serializes the log as format v1, stamped with the file state it applies to.
    /// Returns `None` when the disk state is unknown, since such a log cannot be verified later.
    pub fn encode(&self, disk: DiskState) -> Option<Vec<u8>> {
        let DiskState::Known { mtime, size } = disk else {
            return None;
        };
        let since = mtime.duration_since(UNIX_EPOCH).ok()?;
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&since.as_secs().to_le_bytes());
        out.extend_from_slice(&since.subsec_nanos().to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        let out = self.groups.iter().fold(out, |out, group| {
            let mut out = group.iter().fold(out, put_edit);
            out.push(TAG_BOUNDARY);
            out
        });
        Some(out)
    }

    /// Parses format v1. Returns `None` on any mismatch with `disk`, bad
    /// magic or version, or malformed entries (I-10).
    pub fn decode(bytes: &[u8], disk: DiskState, limit: usize) -> Option<UndoLog> {
        let (magic, pos) = take_at(bytes, 0, MAGIC.len())?;
        let (version, pos) = u8_at(bytes, pos)?;
        if magic != MAGIC || version != VERSION {
            return None;
        }
        let (secs, pos) = u64_at(bytes, pos)?;
        let (nanos, pos) = u32_at(bytes, pos)?;
        if nanos >= 1_000_000_000 {
            return None;
        }
        let stamp = Duration::new(secs, nanos);
        let (file_size, mut pos) = u64_at(bytes, pos)?;
        let DiskState::Known { mtime, size } = disk else {
            return None;
        };
        if mtime.duration_since(UNIX_EPOCH).ok()? != stamp || size != file_size {
            return None;
        }

        let mut groups = Vec::new();
        let mut current = Vec::new();
        while pos < bytes.len() {
            let (tag, next) = u8_at(bytes, pos)?;
            pos = next;
            match tag {
                TAG_BOUNDARY => {
                    groups.push(current);
                    current = Vec::new();
                }
                TAG_INSERT | TAG_DELETE => {
                    let (at, next) = u64_at(bytes, pos)?;
                    let (n, next) = u64_at(bytes, next)?;
                    let (data, next) = take_at(bytes, next, usize::try_from(n).ok()?)?;
                    pos = next;
                    let edit_pos = usize::try_from(at).ok()?;
                    let data = data.to_vec();
                    current.push(if tag == TAG_INSERT {
                        Edit::Insert {
                            pos: edit_pos,
                            bytes: data,
                        }
                    } else {
                        Edit::Delete {
                            pos: edit_pos,
                            bytes: data,
                        }
                    });
                }
                _ => return None,
            }
        }
        if !current.is_empty() {
            return None;
        }

        let size = groups.iter().flatten().map(Edit::size).sum();
        let log = UndoLog {
            groups,
            open: false,
            size,
            limit,
        };
        Some(log.trim().0)
    }
}

fn put_edit(mut out: Vec<u8>, edit: &Edit) -> Vec<u8> {
    let (tag, pos, bytes) = match edit {
        Edit::Insert { pos, bytes } => (TAG_INSERT, pos, bytes),
        Edit::Delete { pos, bytes } => (TAG_DELETE, pos, bytes),
    };
    out.push(tag);
    out.extend_from_slice(&(*pos as u64).to_le_bytes());
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

/// Reads `n` bytes at `pos`. Returns them with the position after them.
fn take_at(bytes: &[u8], pos: usize, n: usize) -> Option<(&[u8], usize)> {
    let end = pos.checked_add(n)?;
    Some((bytes.get(pos..end)?, end))
}

fn u8_at(bytes: &[u8], pos: usize) -> Option<(u8, usize)> {
    let (s, end) = take_at(bytes, pos, 1)?;
    Some((s[0], end))
}

fn u32_at(bytes: &[u8], pos: usize) -> Option<(u32, usize)> {
    let (s, end) = take_at(bytes, pos, 4)?;
    Some((u32::from_le_bytes(s.try_into().ok()?), end))
}

fn u64_at(bytes: &[u8], pos: usize) -> Option<(u64, usize)> {
    let (s, end) = take_at(bytes, pos, 8)?;
    Some((u64::from_le_bytes(s.try_into().ok()?), end))
}

/// `.<name>.undo` beside the file.
pub fn undo_path(path: &Path) -> PathBuf {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    dir.join(format!(".{name}.undo"))
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

    #[test]
    fn undo_path_is_hidden_sibling() {
        assert_eq!(
            undo_path(Path::new("dir/a.txt")),
            Path::new("dir/.a.txt.undo")
        );
    }
}
