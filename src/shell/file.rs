//! File I/O for the shell: reads, atomic saves, and disk state (§8, A2, A12).

use crate::core::{DiskState, Doc, Eol, EolStreamDecoder, RawFile, Read as FileRead};
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static ATOMIC_WRITE_SEQ: AtomicU64 = AtomicU64::new(1);

/// Atomically writes to `target` via a PID- and sequence-isolated temporary file using `write_fn`.
/// Calls `sync_all` before renaming over `target`, and cleans up the temporary file on error.
///
/// # Errors
/// Returns `io::Error` if creating, writing, flushing, or renaming the temporary file fails.
pub fn atomic_write_with(
    target: &Path,
    prefix: &str,
    suffix: &str,
    permissions: Option<Permissions>,
    write_fn: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let pid = std::process::id();
    let seq = ATOMIC_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!("{prefix}{name}.{pid}_{seq}{suffix}"));

    let result =
        write_with_and_sync(&tmp, permissions, write_fn).and_then(|()| fs::rename(&tmp, target));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_with_and_sync(
    tmp: &Path,
    permissions: Option<Permissions>,
    write_fn: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(tmp)?;
    write_fn(&mut f)?;
    f.sync_all()?;
    if let Some(perms) = permissions {
        let _ = fs::set_permissions(tmp, perms);
    }
    Ok(())
}

fn state_of(meta: &fs::Metadata) -> DiskState {
    meta.modified()
        .map_or(DiskState::Unknown, |mtime| DiskState::Known {
            mtime: mtime
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default(),
            size: meta.len(),
        })
}

#[must_use]
pub fn disk_state(path: &Path) -> DiskState {
    fs::metadata(path).map_or(DiskState::Unknown, |m| state_of(&m))
}

/// Reads `path` in 64 KiB blocks directly into a `Doc` B-Tree without allocating a flat buffer.
/// A missing file is `Read::Missing`. A file longer than `max_size` is not read: `doc` is empty
/// and `len` reports its size.
///
/// # Errors
/// Returns `io::Error` if reading file metadata or blocks, or allocating the document, fails.
pub fn read_raw(path: &Path, max_size: u64) -> io::Result<FileRead> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(FileRead::Missing),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    let (doc, eol) = if meta.len() > max_size {
        (Doc::default(), None)
    } else {
        read_streamed(&mut file, meta.len())?
    };
    Ok(FileRead::Found(RawFile {
        len: meta.len(),
        disk: state_of(&meta),
        doc,
        eol,
    }))
}

/// Streams at most `n` bytes in 64 KiB chunks into a fallibly allocated document.
fn read_streamed(file: &mut File, n: u64) -> io::Result<(Doc, Option<Eol>)> {
    let mut limited = Read::by_ref(file).take(n);
    let mut decoder = EolStreamDecoder::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = match limited.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        decoder = decoder
            .try_push_chunk(&buf[..read])
            .map_err(|_| io::ErrorKind::OutOfMemory)?;
    }
    decoder
        .try_finish()
        .map_err(|_| io::ErrorKind::OutOfMemory.into())
}

/// Writes `bytes` to a temporary file beside the target, then renames it over
/// the target. Symlinks are followed; permissions are kept (A2).
///
/// # Errors
/// Returns `io::Error` if canonicalization or atomic file writing fails.
pub fn save(path: &Path, bytes: &[u8]) -> io::Result<DiskState> {
    save_doc(path, Eol::Lf, &crate::core::Doc::from_bytes(bytes))
}

/// Streams `doc` slices to `path` atomically, encoding line endings according to `eol`.
/// Symlinks are followed; permissions are kept (A2).
///
/// # Errors
/// Returns `io::Error` if canonicalization or atomic file writing fails.
pub fn save_doc(path: &Path, eol: Eol, doc: &crate::core::Doc) -> io::Result<DiskState> {
    use std::ops::ControlFlow;

    let target = if fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()) {
        fs::canonicalize(path)?
    } else {
        path.to_path_buf()
    };
    let permissions = fs::metadata(&target).ok().map(|m| m.permissions());
    atomic_write_with(&target, ".", ".tmp", permissions, |f| {
        let mut w = BufWriter::with_capacity(64 * 1024, f);
        let flow = doc
            .try_for_each_slice(0..doc.total_bytes(), |chunk| {
                let result = match eol {
                    Eol::Lf => w.write_all(chunk),
                    Eol::Crlf => write_crlf_slice(&mut w, chunk),
                };
                match result {
                    Ok(()) => ControlFlow::Continue(()),
                    Err(e) => ControlFlow::Break(e),
                }
            })
            .expect("full range is valid");
        match flow {
            ControlFlow::Continue(()) => w.flush(),
            ControlFlow::Break(e) => Err(e),
        }
    })?;
    Ok(disk_state(&target))
}

fn write_crlf_slice(w: &mut impl Write, slice: &[u8]) -> io::Result<()> {
    for (i, part) in slice.split(|&b| b == b'\n').enumerate() {
        if i > 0 {
            w.write_all(b"\r\n")?;
        }
        if !part.is_empty() {
            w.write_all(part)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Eol;
    use crate::core::{FilePath, loaded_from};

    #[test]
    fn roundtrip_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        let bytes = [b'a', 0xE9, 0xC0, 0xAF, 0xE4, 0xB8, b'\n'];
        fs::write(&path, bytes).unwrap();

        let read = read_raw(&path, 1 << 20).unwrap();
        let loaded = loaded_from(
            &FilePath::resolve("", path.to_str().unwrap()),
            read,
            1 << 20,
            Eol::Lf,
        )
        .unwrap();
        assert_eq!(loaded.text, crate::core::Doc::from_bytes(&bytes));
        save_doc(&path, Eol::Lf, &loaded.text).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn oversized_file_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        fs::write(&path, b"0123456789").unwrap();

        let read = read_raw(&path, 4).unwrap();
        let FileRead::Found(raw) = read else {
            panic!("expected Found");
        };
        assert_eq!(raw.len, 10);
        assert_eq!(raw.doc, Doc::default());
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

    #[test]
    fn read_raw_preserves_multibyte_utf8_straddling_64k_boundary() {
        use crate::core::Unit;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("utf8_64k.txt");
        let mut bytes = vec![b'a'; 65_535];
        bytes.extend_from_slice("é".as_bytes()); // bytes 65535..65537 straddle 65536
        bytes.resize(131_070, b'b');
        bytes.extend_from_slice("가".as_bytes()); // bytes 131070..131073 straddle 131072
        fs::write(&path, &bytes).unwrap();

        let FileRead::Found(raw) = read_raw(&path, 1 << 20).unwrap() else {
            panic!("expected Found");
        };
        assert_eq!(raw.doc, Doc::from_bytes(&bytes));
        assert_eq!(raw.doc.decode_at(65_535), Some((Unit::Char('é'), 2)));
        assert!(!raw.doc.is_boundary(65_536));
        assert_eq!(raw.doc.decode_at(131_070), Some((Unit::Char('가'), 3)));
        assert!(!raw.doc.is_boundary(131_071));
        assert!(!raw.doc.is_boundary(131_072));
    }
}
