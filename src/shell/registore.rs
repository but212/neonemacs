//! Registore: Zero-Overhead Single-Pass Atomic Persistence (`1x` sequential write).

use crate::core::{Doc, Eol};
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, BufWriter, Write};
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static ATOMIC_WRITE_SEQ: AtomicU64 = AtomicU64::new(1);

/// Single-pass atomic persistence with explicit line-ending encoding and optional permissions.
pub(crate) fn persist_with(
    path: &Path,
    doc: &Doc,
    eol: Eol,
    permissions: Option<Permissions>,
) -> io::Result<()> {
    persist_with_header(path, &[], doc, eol, permissions)
}

/// Single-pass atomic persistence writing `header` bytes followed by `doc` slices.
pub(crate) fn persist_with_header(
    path: &Path,
    header: &[u8],
    doc: &Doc,
    eol: Eol,
    permissions: Option<Permissions>,
) -> io::Result<()> {
    let parent = match path.parent() {
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) if p.is_dir() => p,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "target directory is missing",
            ));
        }
    };
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "target directory is missing"))?
        .to_string_lossy();
    let pid = std::process::id();
    let (tmp_path, file) = {
        let mut attempts = 0usize;
        loop {
            let seq = ATOMIC_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(".{name}.{pid}_{seq}.tmp"));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(f) => break (candidate, f),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempts < 64 => {
                    attempts += 1;
                }
                Err(e) => return Err(e),
            }
        }
    };
    let mut w = BufWriter::with_capacity(64 * 1024, file);

    let result = (|| -> io::Result<()> {
        if !header.is_empty() {
            w.write_all(header)?;
        }
        let flow = doc
            .try_for_each_slice(0..doc.total_bytes(), |chunk| {
                let res = match eol {
                    Eol::Lf => w.write_all(chunk),
                    Eol::Crlf => write_crlf_slice(&mut w, chunk),
                };
                match res {
                    Ok(()) => ControlFlow::Continue(()),
                    Err(e) => ControlFlow::Break(e),
                }
            })
            .expect("full range is valid");
        if let ControlFlow::Break(e) = flow {
            return Err(e);
        }

        w.flush()?;
        w.get_ref().sync_data()?;
        drop(w);

        if let Some(perms) = permissions {
            let _ = fs::set_permissions(&tmp_path, perms);
        }

        fs::rename(&tmp_path, path)?;

        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();

    if let Err(e) = result {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
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

    #[test]
    fn persist_with_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.txt");
        let payload = "hello\nregistore\n".repeat(1024);
        let doc = Doc::from_bytes(payload.as_bytes());

        persist_with(&path, &doc, Eol::Lf, None).unwrap();
        let leftover_tmps: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftover_tmps.is_empty());

        let loaded = Doc::from_bytes(&fs::read(&path).unwrap());
        assert_eq!(loaded, doc);
    }

    #[test]
    fn missing_parent_directory_returns_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_such_dir").join("file.txt");
        let doc = Doc::from_bytes(b"abc");
        let err = persist_with(&path, &doc, Eol::Lf, None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn failed_rename_preserves_target_and_cleans_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("is_a_dir");
        fs::create_dir(&target_dir).unwrap();

        let doc = Doc::from_bytes(b"should not survive");
        let err = persist_with(&target_dir, &doc, Eol::Lf, None).unwrap_err();
        assert_ne!(err.kind(), io::ErrorKind::NotFound);
        let leftover_tmps: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftover_tmps.is_empty());
        assert!(target_dir.is_dir());
    }

    #[test]
    fn skips_stale_tmp_file_on_seq_collision() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.txt");
        let pid = std::process::id();
        let base_seq = ATOMIC_WRITE_SEQ.load(Ordering::Relaxed);
        for seq in base_seq..base_seq + 8 {
            let stale_tmp = dir.path().join(format!(".note.txt.{pid}_{seq}.tmp"));
            fs::write(&stale_tmp, b"stale orphan bytes").unwrap();
        }

        let doc = Doc::from_bytes(b"fresh content\n");
        persist_with(&path, &doc, Eol::Lf, None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"fresh content\n");
        let first_stale = dir.path().join(format!(".note.txt.{pid}_{base_seq}.tmp"));
        assert_eq!(fs::read(&first_stale).unwrap(), b"stale orphan bytes");
    }

    #[test]
    fn persist_with_header_writes_prefix_before_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.bin");
        let doc = Doc::from_bytes(b"line1\nline2");

        persist_with_header(&path, b"HDR\x01", &doc, Eol::Crlf, None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"HDR\x01line1\r\nline2");
    }
}
