//! Lightweight crash recovery WAL (Write-Ahead Log) for unsaved edits (§7, §14, I-23, I-25).
//! Operates strictly in the imperative shell layer: persists snapshots/deltas during idle time.

use crate::core::effect::WalProbe;
use crate::core::eol::Eol;
use crate::core::file::DiskState;
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
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.wal"))
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

/// Writes buffer contents with its EOL and base `DiskState` to WAL atomically via temp file.
///
/// # Errors
/// Returns `io::Error` if atomic writing of WAL bytes fails.
pub fn write_wal(path: &Path, disk: DiskState, eol: Eol, bytes: &[u8]) -> io::Result<()> {
    let wp = wal_path(path);
    let encoded = encode_wal(disk, eol, bytes);
    crate::shell::atomic::atomic_write(&wp, ".", ".wal.tmp", &encoded, None)
}

/// Reads the WAL contents if available and matching disk state.
///
/// # Errors
/// Returns `io::Error` if reading file bytes fails or if WAL is corrupt or disk state mismatches.
pub fn read_wal(path: &Path, disk: DiskState) -> io::Result<Recovered> {
    let wp = wal_path(path);
    let raw = fs::read(wp)?;
    decode_wal(&raw, disk)
        .ok_or_else(|| io::Error::other("WAL format invalid or disk state mismatch"))
}

/// Where an unusable WAL of `path` is kept: `.filename.wal.stale`.
#[must_use]
pub fn stale_path(path: &Path) -> PathBuf {
    let mut name = wal_path(path).into_os_string();
    name.push(".stale");
    PathBuf::from(name)
}

/// Looks for the WAL of `path`, which was just opened with `disk`. A WAL that does not match
/// (changed file, damaged, old format) is moved to `stale_path`, replacing an older one, so the
/// next flush cannot overwrite it (I-44).
#[must_use]
pub fn probe(path: &Path, disk: DiskState) -> WalProbe {
    let wp = wal_path(path);
    let raw = match fs::read(&wp) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return WalProbe::Absent,
        Err(e) => return WalProbe::Stale(Err(e.to_string())),
    };
    if let Some(recovered) = decode_wal(&raw, disk) {
        return WalProbe::Valid(recovered);
    }
    let stale = stale_path(path);
    WalProbe::Stale(match fs::rename(&wp, &stale) {
        Ok(()) => Ok(stale),
        Err(e) => Err(e.to_string()),
    })
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

        write_wal(&target, DiskState::Unknown, Eol::Lf, b"unsaved edits").unwrap();
        assert!(has_wal(&target));

        let recovered = read_wal(&target, DiskState::Unknown).unwrap();
        assert_eq!(recovered.bytes, b"unsaved edits");

        remove_wal(&target);
        assert!(!has_wal(&target));
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
