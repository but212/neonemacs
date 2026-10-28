//! File I/O for the shell: reads, the atomic save, and disk state (§8, A2, A12).

use crate::core::effect::{self, RawFile};
use crate::core::file::DiskState;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

fn state_of(meta: &fs::Metadata) -> DiskState {
    meta.modified()
        .map_or(DiskState::Unknown, |mtime| DiskState::Known {
            mtime,
            size: meta.len(),
        })
}

#[must_use]
pub fn disk_state(path: &Path) -> DiskState {
    fs::metadata(path).map_or(DiskState::Unknown, |m| state_of(&m))
}

/// Reads `path` as bytes without decoding it. A missing file is `Read::Missing`.
/// A file longer than `max_size` is not read: `bytes` is empty and `len` reports its size.
///
/// # Errors
/// Returns `io::Error` if reading file metadata or bytes fails.
pub fn read_raw(path: &Path, max_size: u64) -> io::Result<effect::Read> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(effect::Read::Missing),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    let bytes = if meta.len() > max_size {
        Vec::new()
    } else {
        read_capped(&mut file, meta.len())?
    };
    Ok(effect::Read::Found(RawFile {
        len: meta.len(),
        disk: state_of(&meta),
        bytes,
    }))
}

/// Reads `n` bytes, reporting allocation failure as an error (A12).
fn read_capped(file: &mut File, n: u64) -> io::Result<Vec<u8>> {
    let n = usize::try_from(n).map_err(|_| io::Error::other("file too large for this platform"))?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(n)
        .map_err(|_| io::Error::other("cannot allocate buffer for file"))?;
    Read::by_ref(file).take(n as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Writes `bytes` to a temporary file beside the target, then renames it over
/// the target. Symlinks are followed; permissions are kept (A2).
///
/// # Errors
/// Returns `io::Error` if canonicalization or atomic file writing fails.
pub fn save(path: &Path, bytes: &[u8]) -> io::Result<DiskState> {
    let target = if fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()) {
        fs::canonicalize(path)?
    } else {
        path.to_path_buf()
    };
    let permissions = fs::metadata(&target).ok().map(|m| m.permissions());
    crate::shell::atomic::atomic_write(&target, ".", ".tmp", bytes, permissions)?;
    Ok(disk_state(&target))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::file::{FilePath, loaded_from};

    #[test]
    fn roundtrip_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        let bytes = [b'a', 0xE9, 0xC0, 0xAF, 0xE4, 0xB8, b'\n'];
        fs::write(&path, bytes).unwrap();

        let read = read_raw(&path, 1 << 20).unwrap();
        let loaded = loaded_from(&FilePath::resolve(Path::new(""), &path), read, 1 << 20).unwrap();
        assert_eq!(loaded.text.to_bytes(), bytes);
        save(&path, &loaded.text.to_bytes()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn oversized_file_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        fs::write(&path, b"0123456789").unwrap();

        let read = read_raw(&path, 4).unwrap();
        let effect::Read::Found(raw) = read else {
            panic!("expected Found");
        };
        assert_eq!(raw.len, 10);
        assert_eq!(raw.bytes, [] as [u8; 0]);
    }

    #[test]
    fn save_leaves_no_temp_file_and_reports_disk_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        let state = save(&path, b"hi").unwrap();
        assert!(matches!(state, DiskState::Known { size: 2, .. }));
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec!["f.txt"]);
    }
}
