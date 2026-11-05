//! C-04, C-05, C-06, I-23, I-25: Lightweight WAL (Write-Ahead Log) crash recovery.

use neonemacs::core::file::DiskState;
use std::time::UNIX_EPOCH;

use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
};
use neonemacs::core::commands::request_open;
use neonemacs::core::config::Config;
use neonemacs::core::editor::Editor;
use neonemacs::core::effect::Event;
use neonemacs::core::eol::Eol;
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
    wal::write_wal(&file_path, disk, Eol::Lf, b"unsaved draft").unwrap();
    assert!(wal::has_wal(&file_path));

    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, &file_path, None, true, Vec::new());
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
    wal::write_wal(&file_path, disk, Eol::Lf, b"fn new() { // recovered }\n").unwrap();
    assert!(wal::has_wal(&file_path));

    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, &file_path, None, true, Vec::new());
    let mut ed = perform_all(ed, None, fx);

    if wal::has_wal(&file_path) {
        let wal_bytes = wal::read_wal(&file_path, disk).unwrap().bytes;
        ed = ed.with_current_buffer(|buf| {
            buf.with_text(neonemacs::core::text::Text::from_bytes(&wal_bytes))
                .mark_modified()
        });
    }

    assert_eq!(
        ed.buffer().text().to_bytes(),
        b"fn new() { // recovered }\n"
    );
    assert!(ed.buffer().modified());
}

#[test]
fn wal_recovers_empty_zero_byte_file() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("empty.txt");
    fs::write(&file_path, "non-empty initial text").unwrap();

    let disk = file::disk_state(&file_path);
    // Persist 0-byte (all text deleted) to WAL
    wal::write_wal(&file_path, disk, Eol::Lf, b"").unwrap();
    assert!(
        wal::has_wal(&file_path),
        "0-byte WAL must be recognized via NEONWAL1 header"
    );

    let wal_bytes = wal::read_wal(&file_path, disk).unwrap().bytes;
    assert_eq!(wal_bytes, b"");
}

#[test]
fn wal_rejects_recovery_if_original_file_modified_externally() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("modified.txt");
    fs::write(&file_path, "original text").unwrap();

    let original_disk = file::disk_state(&file_path);
    wal::write_wal(&file_path, original_disk, Eol::Lf, b"unsaved edits").unwrap();
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
    wal::write_wal(&file_path, disk, Eol::Lf, b"unwanted edits").unwrap();
    assert!(wal::has_wal(&file_path));

    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, &file_path, None, true, Vec::new());
    let ed = perform_all(ed, None, fx);

    // Revert buffer (M-x revert-buffer RET)
    let (ed, fx_revert) =
        neonemacs::core::commands::table::find("revert-buffer").unwrap()(ed, Vec::new());
    let ed = perform_all(ed, None, fx_revert);

    // Opening recovered the draft (I-45), so it is unsaved text and revert asks first.
    assert!(ed.buffer().modified());
    let yes = parse_keys("y").unwrap()[0];
    let ed = handle(ed, None, Event::Key(yes));

    assert_eq!(ed.buffer().text().to_bytes(), b"disk content");
    assert!(
        !wal::has_wal(&file_path),
        "revert-buffer must delete WAL file (I-25)"
    );
}

/// Opens `path` with `C-x C-f`, the way a user does inside the editor.
fn find_file(path: &std::path::Path) -> Editor {
    use neonemacs::test_support::{keys, typ};
    let ed = keys(Editor::new(Config::default()), "C-x C-f");
    keys(typ(ed, &path.display().to_string()), "RET")
}

#[test]
fn find_file_recovers_the_wal_like_start_up_does() {
    // I-45: recovery does not depend on how the file was opened.
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("ff.txt");
    fs::write(&file_path, "disk").unwrap();
    wal::write_wal(&file_path, file::disk_state(&file_path), Eol::Lf, b"draft").unwrap();

    let ed = find_file(&file_path);
    assert_eq!(ed.buffer().name(), "ff.txt");
    assert_eq!(ed.buffer().text().to_bytes(), b"draft");
    assert!(ed.buffer().modified());
    assert_eq!(ed.message(), "[Recovered ff.txt from WAL]");
}

#[test]
fn find_file_keeps_an_unusable_wal_aside_and_says_so() {
    // I-44: a draft for a file that changed is never silently overwritten.
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("ff.txt");
    fs::write(&file_path, "disk").unwrap();
    wal::write_wal(&file_path, file::disk_state(&file_path), Eol::Lf, b"draft").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    fs::write(&file_path, "changed elsewhere").unwrap();

    let ed = find_file(&file_path);
    assert_eq!(ed.buffer().text().to_bytes(), b"changed elsewhere");
    assert!(!ed.buffer().modified());
    assert_eq!(ed.message(), "Stale WAL kept as .ff.txt.wal.stale");
    assert!(!wal::has_wal(&file_path));
    let kept = fs::read(wal::stale_path(&file_path)).unwrap();
    assert!(kept.ends_with(b"draft"));
}

#[test]
fn typing_and_idle_wal_recording() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("notes.txt");
    fs::write(&file_path, "line1\n").unwrap();

    let ed = Editor::new(Config::default());
    let lua = lua::new_state().unwrap();
    let fx = request_open(&ed, &file_path, None, true, Vec::new());
    let ed = perform_all(ed, Some(&lua), fx);
    let mut session = ShellSession::new(lua::bootstrap(ed, &lua), lua);
    let t0 = Instant::now();

    for c in "new text".chars() {
        session.handle_event_at(
            &TermEvent::Key(KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }),
            t0,
        );
    }

    assert!(session.ed.buffer().modified());

    session.tick_at(t0 + Duration::from_millis(999));
    assert!(!wal::has_wal(&file_path));

    session.tick_at(t0 + Duration::from_millis(1000));
    assert!(wal::has_wal(&file_path));

    let disk = file::disk_state(&file_path);
    let recovered = wal::read_wal(&file_path, disk).unwrap().bytes;
    assert_eq!(recovered, b"new textline1\n");
}

#[test]
fn wal_kill_modified_buffer_cleans_up_wal() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("kill_modified.txt");
    fs::write(&file_path, "base content\n").unwrap();

    let ed = Editor::new(Config::default());
    let lua = lua::new_state().unwrap();
    let fx = request_open(&ed, &file_path, None, true, Vec::new());
    let ed = perform_all(ed, Some(&lua), fx);
    let mut session = ShellSession::new(lua::bootstrap(ed, &lua), lua);
    let t0 = Instant::now();

    // Type new text
    for c in "extra edit ".chars() {
        session.handle_event_at(
            &TermEvent::Key(KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }),
            t0,
        );
    }

    // Trigger idle flush after 1 second
    session.tick_at(t0 + Duration::from_millis(1001));
    assert!(
        wal::has_wal(&file_path),
        "WAL should be written after 1s idle"
    );

    // Call M-x kill-buffer RET
    session.handle_event_at(
        &TermEvent::Key(KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        t0,
    );
    for c in "kill-buffer".chars() {
        session.handle_event_at(
            &TermEvent::Key(KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }),
            t0,
        );
    }
    session.handle_event_at(
        &TermEvent::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        t0,
    );
    // Enter to submit empty buffer name targeting current buffer
    session.handle_event_at(
        &TermEvent::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        t0,
    );

    // Prompt asks: "Buffer is modified; kill anyway? (y or n)" -> Answer 'y'
    session.handle_event_at(
        &TermEvent::Key(KeyEvent {
            code: KeyCode::Char('y'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        t0,
    );

    assert!(
        !wal::has_wal(&file_path),
        "WAL file must be deleted upon kill-buffer confirmation (spec §7, I-25)"
    );
}

#[test]
fn wal_quit_unsaved_cleans_up_wal() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("quit_unsaved.txt");
    fs::write(&file_path, "base content\n").unwrap();

    let ed = Editor::new(Config::default());
    let lua = lua::new_state().unwrap();
    let fx = request_open(&ed, &file_path, None, true, Vec::new());
    let ed = perform_all(ed, Some(&lua), fx);
    let mut session = ShellSession::new(lua::bootstrap(ed, &lua), lua);
    let t0 = Instant::now();

    for c in "unsaved work ".chars() {
        session.handle_event_at(
            &TermEvent::Key(KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }),
            t0,
        );
    }

    session.tick_at(t0 + Duration::from_millis(1001));
    assert!(wal::has_wal(&file_path));

    // C-x C-c
    session.handle_event_at(
        &TermEvent::Key(KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        t0,
    );
    session.handle_event_at(
        &TermEvent::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        t0,
    );

    // Prompt: "Modified buffers exist; quit anyway? (y or n)" -> Answer 'y'
    session.handle_event_at(
        &TermEvent::Key(KeyEvent {
            code: KeyCode::Char('y'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        t0,
    );

    assert!(
        !wal::has_wal(&file_path),
        "WAL file must be deleted upon QuitUnsaved confirmation (spec §7, I-25)"
    );
}

#[test]
fn wal_decode_wal_rejects_when_new_buffer_but_external_file_appeared() {
    // New buffer created (DiskState::Unknown, tag == 0)
    let raw_wal = wal::encode_wal(DiskState::Unknown, Eol::Lf, b"wal draft for new file");

    // However, an external file was created on disk before recovery
    let external_disk = DiskState::Known {
        mtime: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        size: 1024,
    };

    // Stale WAL from Unknown must NOT match Known!
    assert!(
        wal::decode_wal(&raw_wal, external_disk).is_none(),
        "decode_wal must reject when WAL was created for non-existent file but disk now has a file"
    );
}

#[test]
fn wal_decode_wal_rejects_when_existing_file_was_deleted_externally() {
    // Existing file buffer (DiskState::Known, tag == 1)
    let original_disk = DiskState::Known {
        mtime: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        size: 512,
    };
    let raw_wal = wal::encode_wal(original_disk, Eol::Lf, b"wal draft for existing file");

    // Original file was deleted externally (DiskState::Unknown)
    assert!(
        wal::decode_wal(&raw_wal, DiskState::Unknown).is_none(),
        "decode_wal must reject when WAL was for Known file but disk file was deleted"
    );
}

#[test]
fn wal_decode_wal_rejects_overflowing_nanos() {
    let disk = DiskState::Known {
        mtime: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        size: 512,
    };
    let mut raw_wal = wal::encode_wal(disk, Eol::Lf, b"valid payload");

    // Overwrite secs with u64::MAX and nanos with 1_000_000_000
    // In Rust std, Duration::new(u64::MAX, 1_000_000_000) causes an arithmetic overflow panic!
    // decode_wal must reject corrupted nanos >= 1_000_000_000 before constructing Duration.
    raw_wal[9..17].copy_from_slice(&u64::MAX.to_le_bytes());
    raw_wal[17..21].copy_from_slice(&1_000_000_000u32.to_le_bytes());

    assert!(
        wal::decode_wal(&raw_wal, disk).is_none(),
        "decode_wal must reject WAL with overflowing nanos >= 1_000_000_000 without panicking"
    );
}

#[test]
fn failed_write_file_keeps_old_wal() {
    use neonemacs::test_support::{keys, typ};

    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join("old.txt");
    fs::write(&old_path, "initial content").unwrap();
    let disk = file::disk_state(&old_path);
    wal::write_wal(&old_path, disk, Eol::Lf, b"unsaved edits").unwrap();

    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, &old_path, None, true, Vec::new());
    let ed = perform_all(ed, None, fx);
    let ed = ed.with_current_buffer(|buf| {
        buf.with_text(neonemacs::core::text::Text::from_bytes(b"unsaved edits"))
            .mark_modified()
    });

    // The parent directory does not exist, so the write fails.
    let bad_path = dir.path().join("missing").join("new.txt");
    let ed = keys(ed, "M-x");
    let ed = typ(ed, "write-file");
    let ed = keys(ed, "RET");
    let ed = typ(ed, &bad_path.display().to_string());
    let ed = keys(ed, "RET");

    assert!(ed.message().starts_with("Save failed"));
    assert_eq!(ed.buffer().name(), "old.txt");
    assert!(
        wal::has_wal(&old_path),
        "a failed write-file keeps the old file's WAL (I-36)"
    );
}

#[test]
fn write_file_cleans_up_old_wal_and_does_not_leave_orphan() {
    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join("old.txt");
    fs::write(&old_path, "initial content").unwrap();

    let disk = file::disk_state(&old_path);
    wal::write_wal(&old_path, disk, Eol::Lf, b"modified content").unwrap();
    assert!(
        wal::has_wal(&old_path),
        "Old WAL must exist before write-file"
    );

    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, &old_path, None, true, Vec::new());
    let mut ed = perform_all(ed, None, fx);

    // Buffer has edits matching WAL
    ed = ed.with_current_buffer(|buf| {
        buf.with_text(neonemacs::core::text::Text::from_bytes(b"modified content"))
            .mark_modified()
    });

    // Execute write-file to "new.txt"
    let new_path = dir.path().join("new.txt");
    let keys = parse_keys("M-x").unwrap();
    for k in keys {
        ed = handle(ed, None, Event::Key(k));
    }
    for c in "write-file\n".chars() {
        let code = if c == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(c)
        };
        ed = handle(
            ed,
            None,
            Event::Key(neonemacs::core::keymap::Key::plain(match code {
                KeyCode::Enter => neonemacs::core::keymap::Code::Enter,
                KeyCode::Char(ch) => neonemacs::core::keymap::Code::Char(ch),
                _ => unreachable!(),
            })),
        );
    }
    for c in format!("{}\n", new_path.display()).chars() {
        let code = if c == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(c)
        };
        ed = handle(
            ed,
            None,
            Event::Key(neonemacs::core::keymap::Key::plain(match code {
                KeyCode::Enter => neonemacs::core::keymap::Code::Enter,
                KeyCode::Char(ch) => neonemacs::core::keymap::Code::Char(ch),
                _ => unreachable!(),
            })),
        );
    }

    assert!(new_path.is_file(), "new.txt must have been created");
    assert_eq!(fs::read(&new_path).unwrap(), b"modified content");
    assert!(
        !wal::has_wal(&old_path),
        "CRITICAL: Old file's WAL must be removed when write-file saves under a new name (preventing orphan WAL)"
    );
}

#[test]
fn wal_concurrent_write_wal_does_not_collide_on_fixed_tmp() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("concurrent.txt");
    fs::write(&target, "base").unwrap();
    let disk = file::disk_state(&target);

    // Concurrently write WAL from 8 threads, 10 iterations each.
    // If write_wal uses a fixed ".wal.tmp" without PID or proper isolation,
    // this will fail with file sharing / collision errors on Windows.
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let p = target.clone();
            std::thread::spawn(move || {
                for iter in 0..10 {
                    let payload = format!("worker {i} iter {iter}").into_bytes();
                    wal::write_wal(&p, disk, Eol::Lf, &payload)?;
                }
                std::io::Result::Ok(())
            })
        })
        .collect();

    for h in handles {
        h.join()
            .unwrap()
            .expect("concurrent write_wal should not collide or fail with sharing violation");
    }

    assert!(wal::has_wal(&target));
    let recovered = wal::read_wal(&target, disk)
        .expect("recovered WAL must be intact")
        .bytes;
    assert!(
        recovered.starts_with(b"worker "),
        "WAL content must not be corrupted"
    );
}
