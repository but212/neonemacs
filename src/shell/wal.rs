//! Lightweight crash recovery WAL (Write-Ahead Log) for unsaved edits (§7, §14, I-23, I-25).
//! Operates strictly in the imperative shell layer: persists snapshots/deltas during idle time.

use crate::core::DiskState;
use crate::core::Eol;
use crate::core::effect::WalProbe;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

pub const WAL_MAGIC: &[u8; 8] = b"NEONWAL2";
const HEADER_LEN: usize = 30;

pub use crate::core::effect::Recovered;

/// Returns the WAL sibling file path: `.filename.wal`
#[must_use]
pub fn wal_path(path: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(".wal");
    path.with_file_name(name)
}

const fn eol_byte(eol: Eol) -> u8 {
    match eol {
        Eol::Lf => 0,
        Eol::Crlf => 1,
    }
}

/// Serializes payload with its EOL and `DiskState` header (schema).
#[must_use]
pub fn encode_wal(disk: DiskState, eol: Eol, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + bytes.len());
    out.extend_from_slice(WAL_MAGIC);
    out.push(eol_byte(eol));
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
    eol: Eol,
    disk: DiskState,
}

impl WalHeader {
    fn parse(raw: &[u8]) -> Option<Self> {
        if raw.len() < HEADER_LEN || &raw[0..8] != WAL_MAGIC {
            return None;
        }
        let eol = match raw[8] {
            0 => Eol::Lf,
            1 => Eol::Crlf,
            _ => return None,
        };
        let tag = raw[9];
        let secs = u64::from_le_bytes(raw[10..18].try_into().ok()?);
        let nanos = u32::from_le_bytes(raw[18..22].try_into().ok()?);
        if nanos >= 1_000_000_000 {
            return None;
        }
        let size = u64::from_le_bytes(raw[22..30].try_into().ok()?);

        let disk = match tag {
            1 => DiskState::Known {
                mtime: UNIX_EPOCH.checked_add(Duration::new(secs, nanos))?,
                size,
            },
            0 => DiskState::Unknown,
            _ => return None,
        };
        Some(Self { eol, disk })
    }

    fn matches(&self, current: DiskState) -> bool {
        self.disk == current
    }
}

/// Validates header against current disk state and returns the payload if matching.
#[must_use]
pub fn decode_wal(raw: &[u8], disk: DiskState) -> Option<Recovered> {
    let header = WalHeader::parse(raw)?;
    if !header.matches(disk) {
        return None;
    }
    Some(Recovered {
        eol: header.eol,
        bytes: raw[HEADER_LEN..].to_vec(),
    })
}

/// Checks if a valid WAL file exists for `path`.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
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

/// Writes buffer contents with its EOL and base `DiskState` to the WAL file `wp` atomically.
fn write_at(wp: &Path, disk: DiskState, eol: Eol, bytes: &[u8]) -> io::Result<()> {
    let encoded = encode_wal(disk, eol, bytes);
    crate::shell::file::atomic_write(wp, ".", ".wal.tmp", &encoded, None)
}

/// Writes buffer contents with its EOL and base `DiskState` to WAL atomically via temp file.
///
/// # Errors
/// Returns `io::Error` if atomic writing of WAL bytes fails.
pub fn write_wal(path: &Path, disk: DiskState, eol: Eol, bytes: &[u8]) -> io::Result<()> {
    write_at(&wal_path(path), disk, eol, bytes)
}

/// Writes the unsaved text of the scratch buffer to its slot, creating the slot directory. The
/// scratch has no file, so its disk state is always `Unknown`.
///
/// # Errors
/// Returns `io::Error` if the directory or the atomic write fails.
pub fn write_scratch(slot: &Path, eol: Eol, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = slot.parent() {
        fs::create_dir_all(dir)?;
    }
    write_at(slot, DiskState::Unknown, eol, bytes)
}

/// `$XDG_STATE_HOME/neonemacs/scratch.wal`, else `<home>/.local/state/neonemacs/scratch.wal`,
/// the same shape as `lua::config_path`.
#[must_use]
pub fn scratch_path_from(xdg: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    xdg.filter(|s| !s.is_empty()).map_or_else(
        || {
            home.map(|h| {
                h.join(".local")
                    .join("state")
                    .join("neonemacs")
                    .join("scratch.wal")
            })
        },
        |xdg| Some(PathBuf::from(xdg).join("neonemacs").join("scratch.wal")),
    )
}

/// The scratch slot for this process, from the environment.
#[must_use]
pub fn scratch_path() -> Option<PathBuf> {
    let xdg = env::var_os("XDG_STATE_HOME");
    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"));
    scratch_path_from(xdg.as_deref(), home.as_deref().map(Path::new))
}

/// Reads the WAL contents if available and matching disk state.
///
/// # Errors
/// Returns `io::Error` if reading file bytes fails or if WAL is corrupt or disk state mismatches.
#[cfg(any(test, feature = "test-support"))]
pub fn read_wal(path: &Path, disk: DiskState) -> io::Result<Recovered> {
    let wp = wal_path(path);
    let raw = fs::read(wp)?;
    decode_wal(&raw, disk)
        .ok_or_else(|| io::Error::other("WAL format invalid or disk state mismatch"))
}

/// Where an unusable WAL file `wp` is kept: the same name plus `.stale`.
fn stale_of(wp: &Path) -> PathBuf {
    let mut name = wp.as_os_str().to_owned();
    name.push(".stale");
    PathBuf::from(name)
}

/// Where an unusable WAL of `path` is kept: `.filename.wal.stale`.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn stale_path(path: &Path) -> PathBuf {
    stale_of(&wal_path(path))
}

/// Looks for the WAL of `path`, which was just opened with `disk` (I-44).
#[must_use]
pub fn probe(path: &Path, disk: DiskState) -> WalProbe {
    probe_file(&wal_path(path), disk)
}

/// Looks at the WAL file `wp`. One that does not match `disk` (changed file, damaged, old format)
/// is moved to `wp` plus `.stale`, replacing an older one, so the next flush cannot overwrite it
/// (I-44).
#[must_use]
pub fn probe_file(wp: &Path, disk: DiskState) -> WalProbe {
    let wp = wp.to_path_buf();
    let raw = match fs::read(&wp) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return WalProbe::Absent,
        Err(e) => return WalProbe::Stale(Err(e.to_string())),
    };
    if let Some(recovered) = decode_wal(&raw, disk) {
        return WalProbe::Valid(recovered);
    }
    let stale = stale_of(&wp);
    WalProbe::Stale(match fs::rename(&wp, &stale) {
        Ok(()) => Ok(stale),
        Err(e) => Err(e.to_string()),
    })
}

/// Removes the WAL file upon successful save, revert, or buffer close (I-25).
pub fn remove_wal(path: &Path) {
    remove_file(&wal_path(path));
}

/// Removes the WAL file `wp`. A missing file is not an error.
pub fn remove_file(wp: &Path) {
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

        write_wal(&target, DiskState::Unknown, Eol::Lf, b"unsaved edits").unwrap();
        assert!(has_wal(&target));

        let recovered = read_wal(&target, DiskState::Unknown).unwrap();
        assert_eq!(recovered.bytes, b"unsaved edits");

        remove_wal(&target);
        assert!(!has_wal(&target));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_get_distinct_wal_paths() {
        use std::os::unix::ffi::OsStrExt;
        let dir = Path::new("d");
        let a = dir.join(OsStr::from_bytes(b"a\xFF"));
        let b = dir.join(OsStr::from_bytes(b"a\xFE"));
        assert_ne!(wal_path(&a), wal_path(&b));
    }

    fn probe_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("doc.txt");
        (dir, target)
    }

    #[test]
    fn probe_without_a_wal_is_absent() {
        let (_dir, target) = probe_dir();
        assert_eq!(probe(&target, DiskState::Unknown), WalProbe::Absent);
    }

    #[test]
    fn probe_of_a_matching_wal_returns_it_and_keeps_the_file() {
        let (_dir, target) = probe_dir();
        write_wal(&target, DiskState::Unknown, Eol::Crlf, b"x\n").unwrap();
        assert_eq!(
            probe(&target, DiskState::Unknown),
            WalProbe::Valid(Recovered {
                eol: Eol::Crlf,
                bytes: b"x\n".to_vec(),
            })
        );
        assert!(has_wal(&target));
    }

    #[test]
    fn probe_moves_an_unusable_wal_aside_instead_of_deleting_it() {
        // I-44
        let (_dir, target) = probe_dir();
        let other = DiskState::Known {
            mtime: UNIX_EPOCH,
            size: 9,
        };
        write_wal(&target, other, Eol::Lf, b"draft").unwrap();
        let stale = stale_path(&target);
        assert_eq!(
            probe(&target, DiskState::Unknown),
            WalProbe::Stale(Ok(stale.clone()))
        );
        assert!(!has_wal(&target), "the next flush must not overwrite it");
        assert_eq!(
            decode_wal(&fs::read(&stale).unwrap(), other).unwrap().bytes,
            b"draft"
        );
    }

    #[test]
    fn probe_moves_garbage_and_old_format_aside() {
        for raw in [b"GARBAGE".to_vec(), {
            let mut old = encode_wal(DiskState::Unknown, Eol::Lf, b"x");
            old[..8].copy_from_slice(b"NEONWAL1");
            old
        }] {
            let (_dir, target) = probe_dir();
            fs::write(wal_path(&target), &raw).unwrap();
            let stale = stale_path(&target);
            assert_eq!(
                probe(&target, DiskState::Unknown),
                WalProbe::Stale(Ok(stale.clone()))
            );
            assert_eq!(fs::read(&stale).unwrap(), raw);
        }
    }

    #[test]
    fn probe_reports_a_wal_that_cannot_be_read_instead_of_ignoring_it() {
        // Only a missing file is `Absent`; any other read error must reach the user (I-44).
        let (_dir, target) = probe_dir();
        fs::create_dir(wal_path(&target)).unwrap();
        assert!(matches!(
            probe(&target, DiskState::Unknown),
            WalProbe::Stale(Err(_))
        ));
    }

    #[test]
    fn a_newer_unusable_wal_replaces_the_older_stale_copy() {
        let (_dir, target) = probe_dir();
        let stale = stale_path(&target);
        fs::write(&stale, b"older").unwrap();
        fs::write(wal_path(&target), b"newer garbage").unwrap();
        assert!(matches!(
            probe(&target, DiskState::Unknown),
            WalProbe::Stale(Ok(_))
        ));
        assert_eq!(fs::read(&stale).unwrap(), b"newer garbage");
    }

    #[test]
    fn scratch_path_follows_the_state_directory_convention() {
        use std::ffi::OsStr;
        let slot = |xdg: Option<&str>, home: Option<&str>| {
            scratch_path_from(xdg.map(OsStr::new), home.map(Path::new))
        };
        assert_eq!(
            slot(Some("/x"), Some("/h")),
            Some(PathBuf::from("/x/neonemacs/scratch.wal"))
        );
        assert_eq!(
            slot(Some(""), Some("/h")),
            Some(PathBuf::from("/h/.local/state/neonemacs/scratch.wal"))
        );
        assert_eq!(
            slot(None, Some("/h")),
            Some(PathBuf::from("/h/.local/state/neonemacs/scratch.wal"))
        );
        assert_eq!(slot(None, None), None);
    }

    #[test]
    fn scratch_slot_is_created_with_its_directory_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let slot = dir
            .path()
            .join("state")
            .join("neonemacs")
            .join("scratch.wal");
        write_scratch(&slot, Eol::Crlf, b"a\nb").unwrap();
        assert_eq!(
            probe_file(&slot, DiskState::Unknown),
            WalProbe::Valid(Recovered {
                eol: Eol::Crlf,
                bytes: b"a\nb".to_vec(),
            })
        );
        remove_file(&slot);
        assert_eq!(probe_file(&slot, DiskState::Unknown), WalProbe::Absent);
    }

    #[test]
    fn an_unusable_scratch_slot_is_moved_aside_too() {
        let dir = tempfile::tempdir().unwrap();
        let slot = dir.path().join("scratch.wal");
        fs::write(&slot, b"GARBAGE").unwrap();
        let stale = dir.path().join("scratch.wal.stale");
        assert_eq!(
            probe_file(&slot, DiskState::Unknown),
            WalProbe::Stale(Ok(stale.clone()))
        );
        assert!(!slot.exists());
        assert_eq!(fs::read(stale).unwrap(), b"GARBAGE");
    }

    #[test]
    fn wal_round_trips_eol() {
        let raw = encode_wal(DiskState::Unknown, Eol::Crlf, b"a\nb\n");
        assert_eq!(
            decode_wal(&raw, DiskState::Unknown),
            Some(Recovered {
                eol: Eol::Crlf,
                bytes: b"a\nb\n".to_vec(),
            })
        );
    }

    #[test]
    fn old_format_wal_is_ignored() {
        let mut raw = encode_wal(DiskState::Unknown, Eol::Lf, b"x");
        raw[..8].copy_from_slice(b"NEONWAL1");
        assert_eq!(decode_wal(&raw, DiskState::Unknown), None);
    }

    #[test]
    fn unknown_eol_byte_invalidates_header() {
        let mut raw = encode_wal(DiskState::Unknown, Eol::Lf, b"x");
        raw[8] = 2;
        assert_eq!(decode_wal(&raw, DiskState::Unknown), None);
    }

    #[test]
    fn read_wal_returns_eol_from_header() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("doc.txt");
        write_wal(&target, DiskState::Unknown, Eol::Crlf, b"x\n").unwrap();
        let recovered = read_wal(&target, DiskState::Unknown).unwrap();
        assert_eq!(recovered.eol, Eol::Crlf);
    }
}
