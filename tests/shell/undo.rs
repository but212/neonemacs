//! The undo file beside a saved file. It is kept only while the file still matches the state it was written for (I-10).

use neonemacs::core::file::DiskState;
use neonemacs::core::undo::{Edit, UndoLog, undo_path};
use neonemacs::shell::file;
use std::fs;

fn insert(pos: usize, s: &str) -> Edit {
    Edit::Insert {
        pos,
        bytes: s.as_bytes().to_vec(),
    }
}

fn sample_log() -> UndoLog {
    let (log, _) = UndoLog::new(1 << 20).record(insert(0, "ab"));
    let (log, _) = log.record(Edit::Delete {
        pos: 1,
        bytes: b"b".to_vec(),
    });
    let log = log.close();
    let (log, _) = log.record(insert(2, "\u{ac00}"));
    log.close()
}

#[test]
fn persist_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    file::save(&path, b"abc").unwrap();
    let disk = file::disk_state(&path);

    let log = sample_log();
    file::save(&undo_path(&path), &log.encode(disk).unwrap()).unwrap();
    let bytes = fs::read(undo_path(&path)).unwrap();
    let reloaded = UndoLog::decode(&bytes, file::disk_state(&path), 1 << 20).unwrap();
    assert_eq!(reloaded.groups(), log.groups());
}

#[test]
fn mismatch_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    file::save(&path, b"abc").unwrap();
    let disk = file::disk_state(&path);
    let bytes = sample_log().encode(disk).unwrap();

    // Changed size: the stamp no longer matches.
    file::save(&path, b"abcd").unwrap();
    assert!(UndoLog::decode(&bytes, file::disk_state(&path), 1 << 20).is_none());
    // Unknown disk state cannot verify anything.
    assert!(UndoLog::decode(&bytes, DiskState::Unknown, 1 << 20).is_none());
    // Bad version, truncation, and garbage are ignored.
    let mut bad_version = bytes.clone();
    bad_version[8] = 2;
    assert!(UndoLog::decode(&bad_version, disk, 1 << 20).is_none());
    assert!(UndoLog::decode(&bytes[..bytes.len() - 1], disk, 1 << 20).is_none());
    assert!(UndoLog::decode(b"junk", disk, 1 << 20).is_none());
}

#[test]
fn overflowing_nanos_are_rejected_not_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    file::save(&path, b"abc").unwrap();
    let disk = file::disk_state(&path);
    let mut bytes = sample_log().encode(disk).unwrap();
    // nanos sits at 17..21 (after magic, version, and secs).
    bytes[17..21].copy_from_slice(&2_000_000_000u32.to_le_bytes());
    assert!(UndoLog::decode(&bytes, disk, 1 << 20).is_none());
}
