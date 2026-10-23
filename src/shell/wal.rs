//! Lightweight crash recovery WAL (Write-Ahead Log) for unsaved edits (§7, §14, I-23, I-25).
//! Operates strictly in the imperative shell layer: persists snapshots/deltas during idle time.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Returns the WAL sibling file path: `.filename.wal`
pub fn wal_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.wal"))
}

/// Checks if a valid WAL file exists for `path`.
pub fn has_wal(path: &Path) -> bool {
    let wp = wal_path(path);
    if !wp.is_file() {
        return false;
    }
    // Only valid if WAL exists and has non-zero size
    fs::metadata(&wp).map(|m| m.len() > 0).unwrap_or(false)
}

/// Writes buffer contents to WAL atomically via temp file.
pub fn write_wal(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let wp = wal_path(path);
    let tmp = wp.with_extension("wal.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, wp)?;
    Ok(())
}

/// Reads the WAL contents if available.
pub fn read_wal(path: &Path) -> io::Result<Vec<u8>> {
    let wp = wal_path(path);
    fs::read(wp)
}

/// Removes the WAL file upon successful regular save (I-25).
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

        write_wal(&target, b"unsaved edits").unwrap();
        assert!(has_wal(&target));

        let recovered = read_wal(&target).unwrap();
        assert_eq!(recovered, b"unsaved edits");

        remove_wal(&target);
        assert!(!has_wal(&target));
    }
}
