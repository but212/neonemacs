//! Lightweight crash recovery WAL (Write-Ahead Log) for unsaved edits (§7, §14, I-23, I-25).
//! Operates strictly in the imperative shell layer: persists snapshots/deltas during idle time.

use crate::core::file::DiskState;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

pub const WAL_MAGIC: &[u8; 8] = b"NEONWAL1";
const HEADER_LEN: usize = 29;

/// Returns the WAL sibling file path: `.filename.wal`
pub fn wal_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.wal"))
}

/// Serializes payload with DiskState header (schema).
pub fn encode_wal(disk: DiskState, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + bytes.len());
    out.extend_from_slice(WAL_MAGIC);
    match disk {
        DiskState::Known { mtime, size } => {
            out.push(1);
            let since = mtime.duration_since(UNIX_EPOCH).unwrap_or_default();
            out.extend_from_slice(&since.as_secs().to_le_bytes());
            out.extend_from_slice(&since.subsec_nanos().to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
        }
        DiskState::Unknown => {
            out.push(0);
            out.extend_from_slice(&0u64.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&0u64.to_le_bytes());
        }
    }
    out.extend_from_slice(bytes);
    out
}

/// Decoded and validated metadata from a WAL header (I-23).
struct WalHeader {
    disk: DiskState,
}

impl WalHeader {
    fn parse(raw: &[u8]) -> Option<Self> {
        if raw.len() < HEADER_LEN || &raw[0..8] != WAL_MAGIC {
            return None;
        }
        let tag = raw[8];
        let secs = u64::from_le_bytes(raw[9..17].try_into().ok()?);
        let nanos = u32::from_le_bytes(raw[17..21].try_into().ok()?);
        if nanos >= 1_000_000_000 {
            return None;
        }
        let size = u64::from_le_bytes(raw[21..29].try_into().ok()?);

        let disk = match tag {
            1 => DiskState::Known {
                mtime: UNIX_EPOCH.checked_add(Duration::new(secs, nanos))?,
                size,
            },
            0 => DiskState::Unknown,
            _ => return None,
        };
        Some(Self { disk })
    }

    fn matches(&self, current: DiskState) -> bool {
        self.disk == current
    }
}

/// Validates header against current disk state and returns payload if matching.
pub fn decode_wal(raw: &[u8], disk: DiskState) -> Option<Vec<u8>> {
    let header = WalHeader::parse(raw)?;
    if !header.matches(disk) {
        return None;
    }
    Some(raw[HEADER_LEN..].to_vec())
}

/// Checks if a valid WAL file exists for `path`.
pub fn has_wal(path: &Path) -> bool {
    let wp = wal_path(path);
    if !wp.is_file() {
        return false;
    }
    let Ok(bytes) = fs::read(&wp) else {
        return false;
    };
    bytes.len() >= HEADER_LEN && &bytes[0..8] == WAL_MAGIC
}

/// Writes buffer contents with its base DiskState to WAL atomically via temp file.
pub fn write_wal(path: &Path, disk: DiskState, bytes: &[u8]) -> io::Result<()> {
    let wp = wal_path(path);
    let encoded = encode_wal(disk, bytes);
    crate::shell::atomic::atomic_write(&wp, ".", ".wal.tmp", &encoded, None)
}

/// Reads the WAL contents if available and matching disk state.
pub fn read_wal(path: &Path, disk: DiskState) -> io::Result<Vec<u8>> {
    let wp = wal_path(path);
    let raw = fs::read(wp)?;
    decode_wal(&raw, disk)
        .ok_or_else(|| io::Error::other("WAL format invalid or disk state mismatch"))
}

/// Removes the WAL file upon successful save, revert, or buffer close (I-25).
pub fn remove_wal(path: &Path) {
    let wp = wal_path(path);
    let _ = fs::remove_file(wp);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("document.txt");

        assert!(!has_wal(&target));

        write_wal(&target, DiskState::Unknown, b"unsaved edits").unwrap();
        assert!(has_wal(&target));

        let recovered = read_wal(&target, DiskState::Unknown).unwrap();
        assert_eq!(recovered, b"unsaved edits");

        remove_wal(&target);
        assert!(!has_wal(&target));
    }
}
