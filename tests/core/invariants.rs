//! Core invariants and anchor preservation tests (I-01, I-23, I-25).

use neonemacs::core::Config;
use neonemacs::core::Doc;
use neonemacs::core::Eol;
use neonemacs::core::{Backing, DiskState, FilePath};
use neonemacs::core::{Editor, Window};
use neonemacs::shell::file;
use neonemacs::shell::startup::{FileArg, open_files};
use neonemacs::shell::wal;
use std::fs;

#[test]
fn restore_content_must_snap_saved_point_and_mark() {
    let ed = Editor::new(Config::default());
    let path = "long_to_short.txt";
    let initial_text = Doc::from_bytes(b"line 1\nline 2\nline 3\nline 4\nline 5\nlong text line");
    let (mut ed, id) = ed.add_buffer(
        "test",
        initial_text,
        Backing::file(FilePath::resolve("", path), DiskState::Unknown),
    );

    // Set saved_point far down at offset 45 and mark at offset 40
    ed = ed.with_buffer(id, |b| b.set_saved_point(45).set_mark(Some(40)));

    // WAL recovers very short content: only 5 bytes
    let recovered_ed = ed.restore_buffer_content(path, Doc::from_bytes(b"short"), Eol::Lf, None);
    let buf = recovered_ed.buffer_by_id(id);

    assert_eq!(buf.text().total_bytes(), 5);
    assert!(
        buf.saved_point() <= buf.text().total_bytes(),
        "I-01 violation: saved_point ({}) exceeds restored text len ({})",
        buf.saved_point(),
        buf.text().total_bytes()
    );
    assert_eq!(
        buf.saved_point(),
        buf.text().floor_boundary(buf.saved_point()),
        "saved_point must be snapped to valid unit boundary"
    );
    assert!(
        buf.mark().unwrap() <= buf.text().total_bytes(),
        "I-01 violation: mark exceeds restored text len"
    );
}

#[test]
fn restore_content_snaps_active_window_and_applies_line() {
    let ed = Editor::new(Config::default());
    let path = "active_wal.txt";
    let initial_text = Doc::from_bytes(b"line 1\nline 2\nline 3\nline 4\nlong text line");
    let (ed, id) = ed.add_buffer(
        "test",
        initial_text,
        Backing::file(FilePath::resolve("", path), DiskState::Unknown),
    );
    let ed = neonemacs::core::switch_to(ed, id).with_window(|w| Window {
        point: 35,
        top: 14,
        ..w
    });

    let recovered =
        ed.restore_buffer_content(path, Doc::from_bytes(b"a\nb\nc\n"), Eol::Lf, Some(2));
    assert_eq!(recovered.buffer_by_id(id).saved_point(), 2);
    assert_eq!(recovered.window().point, 2);
    assert_eq!(recovered.window().top, 0);
}

#[test]
fn open_files_with_line_arg_and_shrunk_wal_preserves_i01() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("shrunk_on_wal.txt");
    // Original file has 30 lines
    let mut initial_content = String::new();
    for i in 1..=30 {
        use std::fmt::Write as _;
        let _ = writeln!(initial_content, "Line number {i} with some content");
    }
    fs::write(&file_path, &initial_content).unwrap();

    let disk = file::disk_state(&file_path);
    // WAL content has only 1 short line
    wal::write_wal(&file_path, disk, Eol::Lf, b"Only line 1\n").unwrap();

    let switched_ed = open_files(
        Editor::new(Config::default()),
        None,
        &[FileArg {
            path: file_path,
            line: Some(25),
        }],
    );

    // Verify that WAL content was indeed restored
    assert_eq!(
        *switched_ed.buffer().text(),
        Doc::from_bytes(b"Only line 1\n"),
        "WAL content must be restored into the buffer"
    );

    // Buffer's saved_point and window's point must be normalized to restored text length (I-01)
    let buf = switched_ed.buffer();
    assert!(
        buf.saved_point() <= buf.text().total_bytes(),
        "I-01 violation: buffer saved_point ({}) exceeds restored text len ({})",
        buf.saved_point(),
        buf.text().total_bytes()
    );
    assert!(
        switched_ed.window().point <= buf.text().total_bytes(),
        "I-01 violation: window point ({}) exceeds restored text len ({})",
        switched_ed.window().point,
        buf.text().total_bytes()
    );
}

#[test]
fn wal_recovery_clears_stale_undo_and_applies_line_offset() {
    use neonemacs::core::Edit;

    let ed = Editor::new(Config::default());
    let path = "f_wal_test.txt";
    let initial_text = Doc::from_bytes(b"line 1\nline 2\nline 3\n");
    let (ed, id) = ed.add_buffer(
        "test",
        initial_text,
        Backing::file(FilePath::resolve("", path), DiskState::Unknown),
    );

    // Stale undo history from before the crash: the last edit inserted "c" at 2
    let ed = ed.with_buffer(id, |b| {
        b.apply_edit(Edit::insert(2, b"c".as_slice()), |a| a, 1 << 20)
            .0
    });

    // Recover from WAL: "PRE_LINE\nline 1\nline 2\nline 3\n"
    let recovered_ed = ed.restore_buffer_content(
        path,
        Doc::from_bytes(b"PRE_LINE\nline 1\nline 2\nline 3\n"),
        Eol::Lf,
        None,
    );
    let buf = recovered_ed.buffer_by_id(id);

    // 1. Stale undo must be cleared!
    assert!(
        buf.undo().groups().is_empty(),
        "Stale undo log was retained after WAL recovery!"
    );

    // 2. Undo command must report "No further undo information" rather than corrupting recovered text
    let (switched_ed, fx) = recovered_ed.undo(Vec::new());
    assert_eq!(switched_ed.message(), "No further undo information");
    assert_eq!(fx, [] as [neonemacs::core::effect::Effect; 0]);
}
