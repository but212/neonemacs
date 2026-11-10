//! Atomic file write utilities ensuring crash safety and concurrent isolation (§2, §7, docs/context.md §4.1, §4.2).

use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static ATOMIC_WRITE_SEQ: AtomicU64 = AtomicU64::new(1);

/// Atomically writes `bytes` to `target` using a PID- and sequence-isolated temporary file.
/// Calls `sync_all` before renaming over `target`, and cleans up the temporary file on error.
///
/// # Errors
/// Returns `io::Error` if creating, writing, flushing, or renaming the temporary file fails.
pub fn atomic_write(
    target: &Path,
    prefix: &str,
    suffix: &str,
    bytes: &[u8],
    permissions: Option<Permissions>,
) -> io::Result<()> {
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let pid = std::process::id();
    let seq = ATOMIC_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!("{prefix}{name}.{pid}_{seq}{suffix}"));

    let result = write_and_sync(&tmp, bytes, permissions).and_then(|()| fs::rename(&tmp, target));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_and_sync(tmp: &Path, bytes: &[u8], permissions: Option<Permissions>) -> io::Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    if let Some(perms) = permissions {
        let _ = fs::set_permissions(tmp, perms);
    }
    Ok(())
}
