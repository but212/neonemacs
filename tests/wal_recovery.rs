//! C-04, C-05, C-06, I-23, I-25: Lightweight WAL (Write-Ahead Log) crash recovery.

use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
};
use neonemacs::core::commands::request_open;
use neonemacs::core::config::Config;
use neonemacs::core::editor::Editor;
use neonemacs::core::effect::Event;
use neonemacs::core::keymap::parse_keys;
use neonemacs::shell::file;
use neonemacs::shell::lua;
use neonemacs::shell::session::ShellSession;
use neonemacs::shell::wal;
use neonemacs::shell::{handle, perform_all};
use std::fs;
use std::time::{Duration, Instant};

#[test]
fn regular_save_cleans_up_existing_wal() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("doc.txt");
    fs::write(&file_path, "initial text").unwrap();

    let disk = file::disk_state(&file_path);
    wal::write_wal(&file_path, disk, b"unsaved draft").unwrap();
    assert!(wal::has_wal(&file_path));

    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, file_path.clone(), None, true, Vec::new());
    let ed = perform_all(ed, None, fx);

    let keys = parse_keys("C-x C-s").unwrap();
    let mut current_ed = ed;
    for k in keys {
        current_ed = handle(current_ed, None, Event::Key(k));
    }

    assert!(
        !wal::has_wal(&file_path),
        "WAL file should be removed upon regular save"
    );
}

#[test]
fn recovery_from_wal_restores_unsaved_content() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("code.rs");
    fs::write(&file_path, "fn old() {}").unwrap();

    let disk = file::disk_state(&file_path);
    wal::write_wal(&file_path, disk, b"fn new() { // recovered }\n").unwrap();
    assert!(wal::has_wal(&file_path));

    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, file_path.clone(), None, true, Vec::new());
    let mut ed = perform_all(ed, None, fx);

    if wal::has_wal(&file_path) {
        let wal_bytes = wal::read_wal(&file_path, disk).unwrap();
        ed = ed.with_current_buffer(|mut buf| {
            buf.text = neonemacs::core::text::Text::from_bytes(&wal_bytes);
            buf.modified = true;
            buf
        });
    }

    assert_eq!(ed.buffer().text.to_bytes(), b"fn new() { // recovered }\n");
    assert!(ed.buffer().modified);
}

#[test]
fn wal_recovers_empty_zero_byte_file() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("empty.txt");
    fs::write(&file_path, "non-empty initial text").unwrap();

    let disk = file::disk_state(&file_path);
    // Persist 0-byte (all text deleted) to WAL
    wal::write_wal(&file_path, disk, b"").unwrap();
    assert!(wal::has_wal(&file_path), "0-byte WAL must be recognized via NEONWAL1 header");

    let wal_bytes = wal::read_wal(&file_path, disk).unwrap();
    assert_eq!(wal_bytes, b"");
}

#[test]
fn wal_rejects_recovery_if_original_file_modified_externally() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("modified.txt");
    fs::write(&file_path, "original text").unwrap();

    let original_disk = file::disk_state(&file_path);
    wal::write_wal(&file_path, original_disk, b"unsaved edits").unwrap();
    assert!(wal::has_wal(&file_path));

    // External process modifies file (simulating git pull or another editor)
    std::thread::sleep(Duration::from_millis(20));
    fs::write(&file_path, "external change from git").unwrap();

    let new_disk = file::disk_state(&file_path);
    assert_ne!(original_disk, new_disk);

    // Stale WAL must be rejected due to disk state mismatch
    assert!(wal::read_wal(&file_path, new_disk).is_err());
}

#[test]
fn revert_buffer_cleans_up_wal() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("revert.txt");
    fs::write(&file_path, "disk content").unwrap();

    let disk = file::disk_state(&file_path);
    wal::write_wal(&file_path, disk, b"unwanted edits").unwrap();
    assert!(wal::has_wal(&file_path));

    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, file_path.clone(), None, true, Vec::new());
    let ed = perform_all(ed, None, fx);

    // Revert buffer (M-x revert-buffer RET)
    let (ed, fx_revert) = neonemacs::core::commands::table::find("revert-buffer").unwrap()(ed, Vec::new());
    let _ = perform_all(ed, None, fx_revert);

    assert!(!wal::has_wal(&file_path), "revert-buffer must delete WAL file (I-25)");
}

#[test]
fn typing_and_idle_wal_recording() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("notes.txt");
    fs::write(&file_path, "line1\n").unwrap();

    let ed = Editor::new(Config::default());
    let lua = lua::new_state().unwrap();
    let fx = request_open(&ed, file_path.clone(), None, true, Vec::new());
    let ed = perform_all(ed, Some(&lua), fx);
    let mut session = ShellSession::new(ed, lua);
    let t0 = Instant::now();

    for c in "new text".chars() {
        session.handle_event_at(
            TermEvent::Key(KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }),
            t0,
        );
    }

    assert!(session.ed.buffer().modified);

    session.tick_at(t0 + Duration::from_millis(999));
    assert!(!wal::has_wal(&file_path));

    session.tick_at(t0 + Duration::from_millis(1000));
    assert!(wal::has_wal(&file_path));

    let disk = file::disk_state(&file_path);
    let recovered = wal::read_wal(&file_path, disk).unwrap();
    assert_eq!(recovered, b"new textline1\n");
}
