//! C-04, C-05, C-06, I-23, I-25: Lightweight WAL (Write-Ahead Log) crash recovery.

use neonemacs::core::commands::request_open;
use neonemacs::core::config::Config;
use neonemacs::core::editor::Editor;
use neonemacs::core::effect::Event;
use neonemacs::core::keymap::{Code, Key, parse_keys};
use neonemacs::shell::wal;
use neonemacs::shell::{handle, perform_all};
use std::fs;

#[test]
fn regular_save_cleans_up_existing_wal() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("doc.txt");
    fs::write(&file, "initial text").unwrap();

    // Create an artificial WAL file simulating unsaved crash
    wal::write_wal(&file, b"unsaved draft").unwrap();
    assert!(wal::has_wal(&file));

    // Open file in editor
    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, file.clone(), None, true, Vec::new());
    let ed = perform_all(ed, None, fx);

    // Perform regular save C-x C-s
    let keys = parse_keys("C-x C-s").unwrap();
    let mut current_ed = ed;
    for k in keys {
        current_ed = handle(current_ed, None, Event::Key(k));
    }

    // After successful save, WAL must be deleted (I-25)
    assert!(
        !wal::has_wal(&file),
        "WAL file should be removed upon regular save"
    );
}

#[test]
fn recovery_from_wal_restores_unsaved_content() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("code.rs");
    fs::write(&file, "fn old() {}").unwrap();

    // Unsaved crash occurred, leaving WAL with new edits
    wal::write_wal(&file, b"fn new() { // recovered }\n").unwrap();
    assert!(wal::has_wal(&file));

    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, file.clone(), None, true, Vec::new());
    let mut ed = perform_all(ed, None, fx);

    // Simulate startup check (as in main.rs open_files)
    if wal::has_wal(&file) {
        let wal_bytes = wal::read_wal(&file).unwrap();
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
fn typing_and_idle_wal_recording() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.txt");
    fs::write(&file, "line1\n").unwrap();

    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, file.clone(), None, true, Vec::new());
    let mut ed = perform_all(ed, None, fx);

    // Type "new text"
    for c in "new text".chars() {
        ed = handle(ed, None, Event::Key(Key::plain(Code::Char(c))));
    }

    assert!(ed.buffer().modified);

    // Simulate the Idle flush (as in main.rs 1s timer)
    wal::write_wal(&file, &ed.buffer().text.to_bytes()).unwrap();
    assert!(wal::has_wal(&file));

    let recovered = wal::read_wal(&file).unwrap();
    assert_eq!(recovered, b"new textline1\n");
}
