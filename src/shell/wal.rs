//! Lightweight crash recovery WAL (Write-Ahead Log) for unsaved edits (§7, §14, I-23, I-25).
//! Operates strictly in the imperative shell layer: persists snapshots/deltas during idle time.

use crate::core::effect::{RecoveredDelta, WalProbe};
use crate::core::weave_tree::DocBuilder;
use crate::core::{
    DiskState, Doc, Eol, MAX_WAL_DELTA_BYTES, MAX_WAL_DELTA_RECORDS, Replace,
    WAL_DELTA_RECORD_BYTES, WalBaseDigest, wal_base_digest,
};
use sha2::{Digest, Sha256};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const WAL_MAGIC: &[u8; 8] = b"NEONWAL2";
pub const WAL_DELTA_MAGIC: &[u8; 8] = b"NEONWALD";
const HEADER_LEN: usize = 30;
const DELTA_COUNT_OFFSET: usize = HEADER_LEN + size_of::<WalBaseDigest>();
const DELTA_HEADER_LEN: usize = DELTA_COUNT_OFFSET + size_of::<u32>();
const DELTA_CHECKSUM_LEN: usize = 32;
const MAX_DELTA_FILE_BYTES: usize = DELTA_HEADER_LEN + MAX_WAL_DELTA_BYTES + DELTA_CHECKSUM_LEN;

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

/// Encodes the fixed 30-byte WAL header (`NEONWAL2`, EOL mode, and `DiskState`).
#[must_use]
pub fn encode_wal_header(disk: DiskState, eol: Eol) -> [u8; HEADER_LEN] {
    encode_header_with_magic(WAL_MAGIC, disk, eol)
}

fn encode_header_with_magic(magic: &[u8; 8], disk: DiskState, eol: Eol) -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];
    header[..8].copy_from_slice(magic);
    header[8] = eol_byte(eol);
    match disk {
        DiskState::Known { mtime, size } => {
            header[9] = 1;
            header[10..18].copy_from_slice(&mtime.as_secs().to_le_bytes());
            header[18..22].copy_from_slice(&mtime.subsec_nanos().to_le_bytes());
            header[22..30].copy_from_slice(&size.to_le_bytes());
        }
        DiskState::Unknown => {
            header[9] = 0;
        }
    }
    header
}

/// Serializes payload with its EOL and `DiskState` header (schema).
#[must_use]
pub fn encode_wal(disk: DiskState, eol: Eol, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + bytes.len());
    out.extend_from_slice(&encode_wal_header(disk, eol));
    out.extend_from_slice(bytes);
    out
}

/// Decoded and validated metadata from a WAL header (I-23).
struct WalHeader {
    eol: Eol,
    disk: DiskState,
}

impl WalHeader {
    fn parse_with_magic(raw: &[u8], expected_magic: &[u8; 8]) -> Option<Self> {
        if raw.len() < HEADER_LEN || &raw[0..8] != expected_magic {
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
                mtime: Duration::new(secs, nanos),
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
    let header = WalHeader::parse_with_magic(raw, WAL_MAGIC)?;
    if !header.matches(disk) {
        return None;
    }
    Some(Recovered {
        eol: header.eol,
        doc: Doc::from_bytes(&raw[HEADER_LEN..]),
    })
}

/// Validates the checksum, header, and baseline fingerprint before allocating any records.
fn decode_wal_deltas(raw: &[u8], disk: DiskState, base: &Doc) -> Option<(Eol, Vec<Replace>)> {
    if !matches!(disk, DiskState::Known { .. })
        || !(DELTA_HEADER_LEN + DELTA_CHECKSUM_LEN..=MAX_DELTA_FILE_BYTES).contains(&raw.len())
    {
        return None;
    }
    let (raw, checksum) = raw.split_at(raw.len() - DELTA_CHECKSUM_LEN);
    if Sha256::digest(raw).as_slice() != checksum {
        return None;
    }
    let header = WalHeader::parse_with_magic(raw, WAL_DELTA_MAGIC)?;
    let count = u32::from_le_bytes(
        raw.get(DELTA_COUNT_OFFSET..DELTA_HEADER_LEN)?
            .try_into()
            .ok()?,
    );
    let count = usize::try_from(count).ok()?;
    let mut rest = &raw[DELTA_HEADER_LEN..];
    if count > MAX_WAL_DELTA_RECORDS
        || count > rest.len() / WAL_DELTA_RECORD_BYTES
        || !header.matches(disk)
        || raw[HEADER_LEN..DELTA_COUNT_OFFSET] != wal_base_digest(base)
    {
        return None;
    }
    let mut deltas = Vec::with_capacity(count);
    for _ in 0..count {
        let (record, body) = rest.split_at_checked(WAL_DELTA_RECORD_BYTES)?;
        let offset = usize::try_from(u64::from_le_bytes(record[0..8].try_into().ok()?)).ok()?;
        let delete_len =
            usize::try_from(u64::from_le_bytes(record[8..16].try_into().ok()?)).ok()?;
        let insert_len =
            usize::try_from(u64::from_le_bytes(record[16..24].try_into().ok()?)).ok()?;
        offset.checked_add(delete_len)?;
        let (insert, remaining) = body.split_at_checked(insert_len)?;
        rest = remaining;
        deltas.push(Replace {
            offset,
            delete_len,
            insert: Doc::from_bytes(insert),
        });
    }
    rest.is_empty().then_some((header.eol, deltas))
}

fn replay_wal_deltas(raw: &[u8], disk: DiskState, base: Doc) -> Option<RecoveredDelta> {
    let (eol, deltas) = decode_wal_deltas(raw, disk, &base)?;
    RecoveredDelta::replay(base, eol, deltas)
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
    bytes.len() >= HEADER_LEN && (&bytes[0..8] == WAL_MAGIC || &bytes[0..8] == WAL_DELTA_MAGIC)
}

/// Streams header and `doc` slices to the WAL file `wp` atomically.
fn write_doc_at(wp: &Path, disk: DiskState, eol: Eol, doc: &crate::core::Doc) -> io::Result<()> {
    use std::ops::ControlFlow;

    crate::shell::file::atomic_write_with(wp, ".", ".wal.tmp", None, |f| {
        let mut f = BufWriter::with_capacity(64 * 1024, f);
        let header = encode_wal_header(disk, eol);
        f.write_all(&header)?;
        let flow = doc
            .try_for_each_slice(0..doc.total_bytes(), |chunk| match f.write_all(chunk) {
                Ok(()) => ControlFlow::Continue(()),
                Err(e) => ControlFlow::Break(e),
            })
            .expect("full range is valid");
        match flow {
            ControlFlow::Continue(()) => f.flush(),
            ControlFlow::Break(e) => Err(e),
        }
    })
}

/// Writes buffer slices with EOL and base `DiskState` to the sibling WAL atomically.
///
/// # Errors
/// Returns `io::Error` if atomic writing of WAL bytes fails.
pub fn write_wal_doc(
    path: &Path,
    disk: DiskState,
    eol: Eol,
    doc: &crate::core::Doc,
) -> io::Result<()> {
    write_doc_at(&wal_path(path), disk, eol, doc)
}

/// Streams `Replace` deltas with EOL and base `DiskState` to the sibling WAL atomically.
///
/// # Errors
/// Returns `io::Error` if atomic writing of WAL delta bytes fails.
pub fn write_wal_deltas(
    path: &Path,
    disk: DiskState,
    eol: Eol,
    base: WalBaseDigest,
    deltas: &[Replace],
) -> io::Result<()> {
    use std::ops::ControlFlow;

    let bytes = deltas.iter().fold(0usize, |sum, r| {
        sum.saturating_add(WAL_DELTA_RECORD_BYTES)
            .saturating_add(r.insert.total_bytes())
    });
    if !matches!(disk, DiskState::Known { .. })
        || deltas.len() > MAX_WAL_DELTA_RECORDS
        || bytes > MAX_WAL_DELTA_BYTES
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid delta WAL baseline or budget",
        ));
    }

    let count = u32::try_from(deltas.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many delta records"))?;
    crate::shell::file::atomic_write_with(&wal_path(path), ".", ".wal.tmp", None, |f| {
        let mut f = BufWriter::with_capacity(64 * 1024, f);
        let mut digest = Sha256::new();
        let mut write = |bytes: &[u8]| {
            digest.update(bytes);
            f.write_all(bytes)
        };
        write(&encode_header_with_magic(WAL_DELTA_MAGIC, disk, eol))?;
        write(&base)?;
        write(&count.to_le_bytes())?;
        for r in deltas {
            write(&(r.offset as u64).to_le_bytes())?;
            write(&(r.delete_len as u64).to_le_bytes())?;
            let insert_len = r.insert.total_bytes();
            write(&(insert_len as u64).to_le_bytes())?;
            let flow = r
                .insert
                .try_for_each_slice(0..insert_len, |chunk| match write(chunk) {
                    Ok(()) => ControlFlow::Continue(()),
                    Err(e) => ControlFlow::Break(e),
                })
                .expect("full range is valid");
            if let ControlFlow::Break(e) = flow {
                return Err(e);
            }
        }
        f.write_all(&digest.finalize())?;
        f.flush()
    })
}

/// Streams the unsaved text slices of the scratch buffer to its slot, creating the slot directory.
///
/// # Errors
/// Returns `io::Error` if the directory or the atomic write fails.
pub fn write_scratch_doc(slot: &Path, eol: Eol, doc: &crate::core::Doc) -> io::Result<()> {
    if let Some(dir) = slot.parent() {
        fs::create_dir_all(dir)?;
    }
    write_doc_at(slot, DiskState::Unknown, eol, doc)
}

/// Writes buffer contents with its EOL and base `DiskState` to WAL atomically via temp file.
///
/// # Errors
/// Returns `io::Error` if atomic writing of WAL bytes fails.
pub fn write_wal(path: &Path, disk: DiskState, eol: Eol, bytes: &[u8]) -> io::Result<()> {
    write_wal_doc(path, disk, eol, &crate::core::Doc::from_bytes(bytes))
}

/// Writes the unsaved text of the scratch buffer to its slot, creating the slot directory. The
/// scratch has no file, so its disk state is always `Unknown`.
///
/// # Errors
/// Returns `io::Error` if the directory or the atomic write fails.
pub fn write_scratch(slot: &Path, eol: Eol, bytes: &[u8]) -> io::Result<()> {
    write_scratch_doc(slot, eol, &crate::core::Doc::from_bytes(bytes))
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
    let invalid = || io::Error::other("WAL format invalid or disk state mismatch");
    match read_contents(fs::File::open(wal_path(path))?, disk)? {
        Some(WalContents::Snapshot(recovered)) => Ok(recovered),
        Some(WalContents::Delta(raw)) => {
            let base = read_base(path, disk).ok_or_else(invalid)?;
            let (doc, eol) = replay_wal_deltas(&raw, disk, base)
                .ok_or_else(invalid)?
                .into_content();
            Ok(Recovered { eol, doc })
        }
        None => Err(invalid()),
    }
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

enum WalContents {
    Snapshot(Recovered),
    Delta(Vec<u8>),
}

fn read_contents(mut input: impl Read, disk: DiskState) -> io::Result<Option<WalContents>> {
    let mut prefix = [0; HEADER_LEN];
    match input.read_exact(&mut prefix) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    if let Some(header) = WalHeader::parse_with_magic(&prefix, WAL_MAGIC) {
        if !header.matches(disk) {
            return Ok(None);
        }
        // The snapshot holds document bytes, not disk encoding. Do not normalize CRLF again.
        let mut builder = DocBuilder::new();
        let mut block = [0; 64 * 1024];
        loop {
            match input.read(&mut block) {
                Ok(0) => break,
                Ok(n) => {
                    builder = builder
                        .try_push_bytes(&block[..n])
                        .map_err(|_| io::ErrorKind::OutOfMemory)?;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        let doc = builder
            .try_finish()
            .map_err(|_| io::ErrorKind::OutOfMemory)?;
        Ok(Some(WalContents::Snapshot(Recovered {
            eol: header.eol,
            doc,
        })))
    } else if WalHeader::parse_with_magic(&prefix, WAL_DELTA_MAGIC)
        .is_some_and(|header| header.matches(disk) && matches!(disk, DiskState::Known { .. }))
    {
        // Read one extra byte to detect oversized logs without loading their tails.
        let mut raw = prefix.to_vec();
        let remaining = MAX_DELTA_FILE_BYTES + 1 - HEADER_LEN;
        input.take(remaining as u64).read_to_end(&mut raw)?;
        Ok(Some(WalContents::Delta(raw)))
    } else {
        Ok(None)
    }
}

fn read_base(path: &Path, disk: DiskState) -> Option<Doc> {
    let DiskState::Known { size, .. } = disk else {
        return None;
    };
    match crate::shell::file::read_raw(path, size).ok()? {
        crate::core::Read::Found(f) if f.disk == disk => Some(f.doc),
        _ => None,
    }
}

/// Looks for the WAL of `path`, loading its baseline only if needed (I-44).
#[must_use]
pub fn probe(path: &Path, disk: DiskState) -> WalProbe {
    probe_at(&wal_path(path), disk, || read_base(path, disk))
}

/// Looks for the WAL of `path`. Delta logs require a loaded baseline (I-44).
#[must_use]
pub fn probe_with_base(path: &Path, disk: DiskState, base: Option<&Doc>) -> WalProbe {
    probe_at(&wal_path(path), disk, || base.cloned())
}

/// Looks at the WAL file `wp`. One that does not match `disk` (changed file, damaged, old format)
/// is moved to `wp` plus `.stale`, replacing an older one, so the next flush cannot overwrite it
/// (I-44).
#[must_use]
pub fn probe_file(wp: &Path, disk: DiskState) -> WalProbe {
    probe_at(wp, disk, || None)
}

fn probe_at(wp: &Path, disk: DiskState, base: impl FnOnce() -> Option<Doc>) -> WalProbe {
    let file = match fs::File::open(wp) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return WalProbe::Absent,
        Err(e) => return WalProbe::Stale(Err(e.to_string())),
    };
    match read_contents(file, disk) {
        Ok(Some(WalContents::Snapshot(recovered))) => return WalProbe::Valid(recovered),
        Ok(Some(WalContents::Delta(raw))) => {
            if let Some(delta) = base().and_then(|base| replay_wal_deltas(&raw, disk, base)) {
                return WalProbe::ValidDelta(delta);
            }
        }
        Ok(None) => {}
        Err(e) => return WalProbe::Stale(Err(e.to_string())),
    }
    let stale = stale_of(wp);
    WalProbe::Stale(match fs::rename(wp, &stale) {
        Ok(()) => Ok(stale.to_string_lossy().into_owned()),
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
        assert_eq!(recovered.doc, Doc::from_bytes(b"unsaved edits"));

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
                doc: Doc::from_bytes(b"x\n"),
            })
        );
        assert!(has_wal(&target));
    }

    #[test]
    fn streamed_snapshot_keeps_crlf_and_utf8_at_block_boundaries() {
        let (_dir, target) = probe_dir();
        let mut payload = vec![b'a'; 64 * 1024 - 1];
        payload.extend_from_slice("é\r\n".as_bytes());
        payload.push(0xFF);
        write_wal(&target, DiskState::Unknown, Eol::Crlf, &payload).unwrap();
        assert_eq!(
            probe(&target, DiskState::Unknown),
            WalProbe::Valid(Recovered {
                eol: Eol::Crlf,
                doc: Doc::from_bytes(&payload)
            })
        );
    }

    #[test]
    fn mismatched_snapshot_does_not_read_its_payload() {
        let header = encode_wal_header(DiskState::Unknown, Eol::Lf);
        let input = header.as_slice().chain(std::io::repeat(0));
        let disk = DiskState::Known {
            mtime: Duration::ZERO,
            size: 0,
        };
        // Limit the reader to the header plus one byte, then inspect the unread byte.
        let mut input = input.take((HEADER_LEN + 1) as u64);
        assert!(read_contents(&mut input, disk).unwrap().is_none());
        assert_eq!(input.limit(), 1);
    }

    #[test]
    fn probe_moves_an_unusable_wal_aside_instead_of_deleting_it() {
        // I-44
        let (_dir, target) = probe_dir();
        let other = DiskState::Known {
            mtime: Duration::ZERO,
            size: 9,
        };
        write_wal(&target, other, Eol::Lf, b"draft").unwrap();
        let stale = stale_path(&target);
        assert_eq!(
            probe(&target, DiskState::Unknown),
            WalProbe::Stale(Ok(stale.to_string_lossy().into_owned()))
        );
        assert!(!has_wal(&target), "the next flush must not overwrite it");
        assert_eq!(
            decode_wal(&fs::read(&stale).unwrap(), other).unwrap().doc,
            Doc::from_bytes(b"draft")
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
                WalProbe::Stale(Ok(stale.to_string_lossy().into_owned()))
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
                doc: Doc::from_bytes(b"a\nb"),
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
            WalProbe::Stale(Ok(stale.to_string_lossy().into_owned()))
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
                doc: Doc::from_bytes(b"a\nb\n"),
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

    #[test]
    fn read_delta_wal_rejects_changed_or_missing_base() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("base.txt");
        fs::write(&target, b"abc").unwrap();
        let disk = crate::shell::file::disk_state(&target);
        let delta = Replace {
            offset: 0,
            delete_len: 0,
            insert: Doc::from_bytes(b"X"),
        };
        let base = wal_base_digest(&Doc::from_bytes(b"abc"));
        write_wal_deltas(&target, disk, Eol::Lf, base, &[delta]).unwrap();
        assert_eq!(
            read_wal(&target, disk).unwrap().doc,
            Doc::from_bytes(b"Xabc")
        );

        fs::write(&target, b"1234").unwrap();
        assert!(read_wal(&target, disk).is_err());
        fs::remove_file(&target).unwrap();
        assert!(read_wal(&target, disk).is_err());
    }

    #[test]
    fn delta_wal_roundtrips_and_probes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("big.txt");
        fs::write(&target, b"abcdefgh").unwrap();
        let disk = crate::shell::file::disk_state(&target);
        let base = wal_base_digest(&Doc::from_bytes(b"abcdefgh"));
        let deltas = vec![
            Replace {
                offset: 5,
                delete_len: 3,
                insert: Doc::from_bytes(b"hello"),
            },
            Replace {
                offset: 0,
                delete_len: 1,
                insert: Doc::default(),
            },
        ];
        write_wal_deltas(&target, disk, Eol::Crlf, base, &deltas).unwrap();
        assert!(has_wal(&target));
        assert_eq!(
            probe(&target, disk),
            WalProbe::ValidDelta(
                RecoveredDelta::replay(Doc::from_bytes(b"abcdefgh"), Eol::Crlf, deltas).unwrap()
            )
        );
    }

    #[test]
    fn delta_wal_quarantines_a_complete_prefix_missing_its_last_record() {
        let (_dir, target) = probe_dir();
        let disk = DiskState::Known {
            mtime: Duration::ZERO,
            size: 0,
        };
        let base = Doc::default();
        let delta = Replace {
            offset: 0,
            delete_len: 0,
            insert: Doc::from_bytes(b"x"),
        };
        write_wal_deltas(
            &target,
            disk,
            Eol::Lf,
            wal_base_digest(&base),
            &[delta.clone(), delta],
        )
        .unwrap();
        let wp = wal_path(&target);
        let mut raw = fs::read(&wp).unwrap();
        raw.truncate(DELTA_HEADER_LEN + WAL_DELTA_RECORD_BYTES + 1);
        fs::write(&wp, &raw).unwrap();

        let stale = stale_path(&target);
        assert_eq!(
            probe_with_base(&target, disk, Some(&base)),
            WalProbe::Stale(Ok(stale.to_string_lossy().into_owned()))
        );
        assert!(!wp.exists());
        assert_eq!(fs::read(stale).unwrap(), raw);
    }

    #[test]
    fn delta_wal_quarantines_a_flipped_offset_bit() {
        let (_dir, target) = probe_dir();
        let base = Doc::from_bytes(b"ABCDE");
        let disk = DiskState::Known {
            mtime: Duration::ZERO,
            size: 5,
        };
        write_wal_deltas(
            &target,
            disk,
            Eol::Lf,
            wal_base_digest(&base),
            &[Replace {
                offset: 1,
                delete_len: 1,
                insert: Doc::default(),
            }],
        )
        .unwrap();
        let wp = wal_path(&target);
        let mut raw = fs::read(&wp).unwrap();
        raw[DELTA_HEADER_LEN] ^= 1; // Offset 0 is valid, but would recover BCDE instead of ACDE.
        fs::write(&wp, &raw).unwrap();

        let stale = stale_path(&target);
        assert_eq!(
            probe_with_base(&target, disk, Some(&base)),
            WalProbe::Stale(Ok(stale.to_string_lossy().into_owned()))
        );
        assert!(!wp.exists());
        assert_eq!(fs::read(stale).unwrap(), raw);
    }

    #[test]
    fn delta_wal_accepts_zero_records_for_eol_only_changes() {
        let (_dir, target) = probe_dir();
        let disk = DiskState::Known {
            mtime: Duration::ZERO,
            size: 0,
        };
        let base = Doc::default();
        write_wal_deltas(&target, disk, Eol::Crlf, wal_base_digest(&base), &[]).unwrap();
        assert_eq!(
            probe_with_base(&target, disk, Some(&base)),
            WalProbe::ValidDelta(RecoveredDelta::replay(base.clone(), Eol::Crlf, vec![]).unwrap())
        );
    }

    #[test]
    fn delta_wal_rejects_unknown_disk_state_and_quarantines_scratch() {
        let dir = tempfile::tempdir().unwrap();
        let slot = dir.path().join("scratch.wal");
        let mut raw =
            encode_header_with_magic(WAL_DELTA_MAGIC, DiskState::Unknown, Eol::Lf).to_vec();
        let base = Doc::default();
        raw.extend_from_slice(&wal_base_digest(&base));
        raw.extend_from_slice(&1u32.to_le_bytes());
        raw.extend_from_slice(&0u64.to_le_bytes());
        raw.extend_from_slice(&0u64.to_le_bytes());
        raw.extend_from_slice(&1u64.to_le_bytes());
        raw.push(b'x');
        raw.extend_from_slice(&Sha256::digest(&raw));

        assert_eq!(decode_wal_deltas(&raw, DiskState::Unknown, &base), None);

        fs::write(&slot, &raw).unwrap();
        let stale = dir.path().join("scratch.wal.stale");
        assert_eq!(
            probe_file(&slot, DiskState::Unknown),
            WalProbe::Stale(Ok(stale.to_string_lossy().into_owned()))
        );
        assert!(!slot.exists());
        assert_eq!(fs::read(&stale).unwrap(), raw);
    }

    #[test]
    fn delta_wal_rejects_overflowing_offset_plus_delete_len() {
        let disk = DiskState::Known {
            mtime: Duration::from_secs(100),
            size: 16,
        };
        let mut raw = encode_header_with_magic(WAL_DELTA_MAGIC, disk, Eol::Lf).to_vec();
        let base = Doc::default();
        raw.extend_from_slice(&wal_base_digest(&base));
        raw.extend_from_slice(&1u32.to_le_bytes());
        let max_offset = u64::try_from(usize::MAX).unwrap();
        raw.extend_from_slice(&max_offset.to_le_bytes());
        raw.extend_from_slice(&1u64.to_le_bytes());
        raw.extend_from_slice(&0u64.to_le_bytes());
        raw.extend_from_slice(&Sha256::digest(&raw));

        assert_eq!(decode_wal_deltas(&raw, disk, &base), None);
    }

    #[test]
    fn delta_reader_enforces_record_and_byte_budgets() {
        let (_dir, target) = probe_dir();
        let base = Doc::default();
        let disk = DiskState::Known {
            mtime: Duration::ZERO,
            size: 0,
        };
        let mut raw = encode_header_with_magic(WAL_DELTA_MAGIC, disk, Eol::Lf).to_vec();
        raw.extend_from_slice(&wal_base_digest(&base));
        let max_records = u32::try_from(MAX_WAL_DELTA_RECORDS).unwrap();
        raw.extend_from_slice(&max_records.to_le_bytes());
        raw.resize(
            DELTA_HEADER_LEN + MAX_WAL_DELTA_RECORDS * WAL_DELTA_RECORD_BYTES,
            0,
        );
        raw.extend_from_slice(&Sha256::digest(&raw));
        assert_eq!(
            decode_wal_deltas(&raw, disk, &base).unwrap().1.len(),
            MAX_WAL_DELTA_RECORDS
        );
        raw.truncate(raw.len() - DELTA_CHECKSUM_LEN);
        raw.extend_from_slice(&[0; WAL_DELTA_RECORD_BYTES]);
        raw.extend_from_slice(&Sha256::digest(&raw));
        assert!(decode_wal_deltas(&raw, disk, &base).is_none());
        raw.truncate(raw.len() - DELTA_CHECKSUM_LEN);
        raw[DELTA_COUNT_OFFSET..DELTA_HEADER_LEN].copy_from_slice(&(max_records + 1).to_le_bytes());
        raw.extend_from_slice(&Sha256::digest(&raw));
        assert!(decode_wal_deltas(&raw, disk, &base).is_none());

        raw.resize(MAX_DELTA_FILE_BYTES + 100, 0);
        let wp = wal_path(&target);
        fs::write(&wp, &raw).unwrap();
        let Some(WalContents::Delta(read)) =
            read_contents(fs::File::open(&wp).unwrap(), disk).unwrap()
        else {
            panic!("expected delta WAL");
        };
        assert_eq!(read.len(), MAX_DELTA_FILE_BYTES + 1);
        assert_eq!(
            probe_with_base(&target, disk, Some(&base)),
            WalProbe::Stale(Ok(stale_path(&target).to_string_lossy().into_owned()))
        );
        assert_eq!(fs::read(stale_path(&target)).unwrap(), raw);
    }

    #[test]
    fn probe_with_base_quarantines_unapplicable_oob_and_mid_utf8_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let disk = DiskState::Known {
            mtime: Duration::from_secs(100),
            size: 5,
        };

        // Out-of-bounds delta against base document is quarantined as `.stale` (I-44).
        let oob_target = dir.path().join("oob.txt");
        let oob_deltas = vec![Replace {
            offset: 10,
            delete_len: 1,
            insert: Doc::from_bytes(b"z"),
        }];
        let base = Doc::from_bytes(b"hello");
        write_wal_deltas(
            &oob_target,
            disk,
            Eol::Lf,
            wal_base_digest(&base),
            &oob_deltas,
        )
        .unwrap();
        assert_eq!(
            probe_with_base(&oob_target, disk, Some(&base)),
            WalProbe::Stale(Ok(stale_path(&oob_target).to_string_lossy().into_owned()))
        );
        assert!(!has_wal(&oob_target));
        assert!(stale_path(&oob_target).is_file());

        // Mid-UTF-8 boundary delta against base document is quarantined as `.stale` (I-44).
        let mid_target = dir.path().join("mid.txt");
        let mid_deltas = vec![Replace {
            offset: 1,
            delete_len: 1,
            insert: Doc::default(),
        }];
        let utf8_base = Doc::from_bytes("가".as_bytes());
        let digest = wal_base_digest(&utf8_base);
        write_wal_deltas(&mid_target, disk, Eol::Lf, digest, &mid_deltas).unwrap();
        assert_eq!(
            probe_with_base(&mid_target, disk, Some(&utf8_base)),
            WalProbe::Stale(Ok(stale_path(&mid_target).to_string_lossy().into_owned()))
        );
        assert!(!has_wal(&mid_target));
        assert!(stale_path(&mid_target).is_file());

        // Valid delta sequence succeeds and keeps the WAL file intact.
        let ok_target = dir.path().join("ok.txt");
        let ok_deltas = vec![Replace {
            offset: 0,
            delete_len: 3,
            insert: Doc::from_bytes(b"hi"),
        }];
        write_wal_deltas(&ok_target, disk, Eol::Lf, digest, &ok_deltas).unwrap();
        assert_eq!(
            probe_with_base(&ok_target, disk, Some(&utf8_base)),
            WalProbe::ValidDelta(
                RecoveredDelta::replay(utf8_base.clone(), Eol::Lf, ok_deltas).unwrap()
            )
        );
        assert!(has_wal(&ok_target));
    }
}
