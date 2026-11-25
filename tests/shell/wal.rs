//! C-04, C-05, C-06, I-23, I-25: Lightweight WAL (Write-Ahead Log) crash recovery.

use neonemacs::core::DiskState;
use std::time::UNIX_EPOCH;

use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
};
use neonemacs::core::Config;
use neonemacs::core::Doc;
use neonemacs::core::Editor;
use neonemacs::core::Eol;
use neonemacs::core::effect::Event;
use neonemacs::core::parse_keys;
use neonemacs::core::request_open;
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
    let ed = perform_all(ed, None, fx);

    assert_eq!(
        *ed.buffer().text(),
        neonemacs::core::Doc::from_bytes(b"fn new() { // recovered }\n")
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

    let recovered = wal::read_wal(&file_path, disk).unwrap();
    assert_eq!(recovered.doc, Doc::default());
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
    let ed = handle(ed, None, Event::Run("revert-buffer".to_owned()));

    // Opening recovered the draft (I-45), so it is unsaved text and revert asks first.
    assert!(ed.buffer().modified());
    let yes = parse_keys("y").unwrap()[0];
    let ed = handle(ed, None, Event::Key(yes));

    assert_eq!(
        *ed.buffer().text(),
        neonemacs::core::Doc::from_bytes(b"disk content")
    );
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
    assert_eq!(
        *ed.buffer().text(),
        neonemacs::core::Doc::from_bytes(b"draft")
    );
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
    assert_eq!(
        *ed.buffer().text(),
        neonemacs::core::Doc::from_bytes(b"changed elsewhere")
    );
    assert!(!ed.buffer().modified());
    assert_eq!(ed.message(), "Stale WAL kept as .ff.txt.wal.stale");
    assert!(!wal::has_wal(&file_path));
    let kept = fs::read(wal::stale_path(&file_path)).unwrap();
    assert!(kept.ends_with(b"draft"));
}

fn press(session: &mut ShellSession, code: KeyCode, modifiers: KeyModifiers, at: Instant) {
    session.handle_event_at(
        &TermEvent::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        at,
    );
}

fn scratch_session(slot: Option<std::path::PathBuf>) -> ShellSession {
    let lua = lua::new_state().unwrap();
    let ed = lua::bootstrap(Editor::new(Config::default()), &lua).with_scratch_slot(slot);
    ShellSession::new(ed, lua)
}

fn type_x(session: &mut ShellSession, at: Instant) {
    press(session, KeyCode::Char('x'), KeyModifiers::NONE, at);
}

#[test]
fn scratch_is_flushed_to_its_slot_after_one_idle_second() {
    let dir = tempfile::tempdir().unwrap();
    let slot = dir.path().join("state").join("scratch.wal");
    let mut session = scratch_session(Some(slot.clone()));
    let t0 = Instant::now();
    type_x(&mut session, t0);

    session.tick_at(t0 + Duration::from_millis(999));
    assert!(!slot.exists());
    session.tick_at(t0 + Duration::from_millis(1000));
    assert!(matches!(
        wal::probe_file(&slot, DiskState::Unknown),
        neonemacs::core::effect::WalProbe::Valid(r) if r.doc == Doc::from_bytes(b"x")
    ));
    assert!(!session.ed.buffer().is_wal_dirty());
}

#[test]
fn scratch_without_a_slot_writes_nothing_and_stops_retrying() {
    let mut session = scratch_session(None);
    let t0 = Instant::now();
    type_x(&mut session, t0);
    session.tick_at(t0 + Duration::from_secs(2));
    assert!(!session.ed.buffer().is_wal_dirty());
}

#[test]
fn slot_is_dropped_when_the_scratch_is_clean_again() {
    let dir = tempfile::tempdir().unwrap();
    let slot = dir.path().join("scratch.wal");
    let mut session = scratch_session(Some(slot.clone()));
    let t0 = Instant::now();
    type_x(&mut session, t0);
    session.tick_at(t0 + Duration::from_secs(1));
    assert!(slot.exists());

    // Undo back to the empty scratch: nothing is left to protect (I-43).
    press(&mut session, KeyCode::Char('_'), KeyModifiers::CONTROL, t0);
    assert!(!session.ed.buffer().modified());
    assert!(!slot.exists());
}

#[test]
fn slot_is_dropped_when_the_user_quits_and_discards() {
    let dir = tempfile::tempdir().unwrap();
    let slot = dir.path().join("scratch.wal");
    let mut session = scratch_session(Some(slot.clone()));
    let t0 = Instant::now();
    type_x(&mut session, t0);
    session.tick_at(t0 + Duration::from_secs(1));
    assert!(slot.exists());

    press(&mut session, KeyCode::Char('x'), KeyModifiers::CONTROL, t0);
    press(&mut session, KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
    assert!(!session.is_quit(), "unsaved scratch asks first");
    assert!(slot.exists(), "no discard yet");
    press(&mut session, KeyCode::Char('y'), KeyModifiers::NONE, t0);
    assert!(session.is_quit());
    assert!(!slot.exists());
}

#[test]
fn slot_is_dropped_on_quit_even_if_a_file_wal_failed_in_the_same_flush() {
    // I-43: a flush that wrote the slot and failed on another buffer still tracks the slot.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f.txt");
    fs::write(&file, "init").unwrap();
    fs::create_dir(wal::wal_path(&file)).unwrap();
    let slot = dir.path().join("scratch.wal");
    let mut session = scratch_session(Some(slot.clone()));
    let t0 = Instant::now();
    type_x(&mut session, t0);
    let fx = request_open(&session.ed, &file, None, true, Vec::new());
    session.ed = perform_all(std::mem::take(&mut session.ed), None, fx);
    type_x(&mut session, t0);

    session.tick_at(t0 + Duration::from_secs(1));
    assert!(slot.exists());

    press(&mut session, KeyCode::Char('x'), KeyModifiers::CONTROL, t0);
    press(&mut session, KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
    press(&mut session, KeyCode::Char('y'), KeyModifiers::NONE, t0);
    assert!(session.is_quit());
    assert!(!slot.exists());
}

#[test]
fn an_existing_slot_survives_until_the_scratch_is_clean() {
    // A crash left the slot; the next run restores it and must not delete it while unsaved.
    let dir = tempfile::tempdir().unwrap();
    let slot = dir.path().join("scratch.wal");
    wal::write_scratch(&slot, Eol::Lf, b"draft").unwrap();
    let lua = lua::new_state().unwrap();
    let ed =
        lua::bootstrap(Editor::new(Config::default()), &lua).with_scratch_slot(Some(slot.clone()));
    let ed = neonemacs::shell::startup::restore_scratch(ed);
    let mut session = ShellSession::new(ed, lua);
    let t0 = Instant::now();
    type_x(&mut session, t0);
    assert!(slot.exists());
    assert_eq!(
        *session.ed.buffer().text(),
        neonemacs::core::Doc::from_bytes(b"xdraft")
    );
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
    let recovered = wal::read_wal(&file_path, disk).unwrap();
    assert_eq!(recovered.doc, Doc::from_bytes(b"new textline1\n"));
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
        buf.with_text(neonemacs::core::Doc::from_bytes(b"unsaved edits"))
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
        buf.with_text(neonemacs::core::Doc::from_bytes(b"modified content"))
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
            Event::Key(neonemacs::core::Key::plain(match code {
                KeyCode::Enter => neonemacs::core::Code::Enter,
                KeyCode::Char(ch) => neonemacs::core::Code::Char(ch),
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
            Event::Key(neonemacs::core::Key::plain(match code {
                KeyCode::Enter => neonemacs::core::Code::Enter,
                KeyCode::Char(ch) => neonemacs::core::Code::Char(ch),
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
    let recovered = wal::read_wal(&target, disk).expect("recovered WAL must be intact");
    assert!(
        recovered.doc.slice(0..7).ok() == Some(Doc::from_bytes(b"worker ")),
        "WAL content must not be corrupted"
    );
}

fn delta_config() -> Config {
    use neonemacs::core::{Setting, Value};
    Config::default()
        .set(Setting::WalDeltas, Value::Bool(true))
        .unwrap()
}

#[test]
fn delta_wal_writes_are_opt_in_but_recovery_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("draft.txt");
    let open = |config| {
        let ed = Editor::new(config);
        let fx = request_open(&ed, &path, None, true, Vec::new());
        perform_all(ed, None, fx)
    };
    let wp = wal::wal_path(&path);
    for (config, magic) in [
        (Config::default(), wal::WAL_MAGIC),
        (delta_config(), wal::WAL_DELTA_MAGIC),
    ] {
        fs::write(&path, b"disk").unwrap();
        let ed = open(config).insert_at(4, b" draft");
        assert_eq!(
            ed.buffer().wal_deltas().is_some(),
            magic == wal::WAL_DELTA_MAGIC
        );
        let ed = handle(ed, None, Event::Idle);
        assert!(!ed.buffer().is_wal_dirty());
        let raw = fs::read(&wp).unwrap();
        assert!(raw.starts_with(magic));
        let reopened = open(Config::default());
        assert!(reopened.buffer().modified());
        assert_eq!(reopened.buffer().text(), ed.buffer().text());
        assert!(reopened.buffer().wal_deltas().is_none());
        fs::remove_file(&wp).unwrap();
    }
}

#[test]
fn recovered_delta_records_are_kept_for_the_next_flush() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("draft.txt");
    fs::write(&path, b"disk").unwrap();
    let open = || {
        let ed = Editor::new(delta_config());
        let fx = request_open(&ed, &path, None, true, Vec::new());
        perform_all(ed, None, fx)
    };
    let ed = handle(open().insert_at(4, b" draft"), None, Event::Idle);
    let reopened = open();
    assert_eq!(reopened.buffer().text(), ed.buffer().text());
    assert_eq!(reopened.buffer().wal_deltas(), ed.buffer().wal_deltas());
    assert!(!reopened.buffer().is_wal_dirty());

    let edited = handle(reopened.insert_at(10, b"!"), None, Event::Idle);
    assert_eq!(edited.buffer().wal_deltas().unwrap().1.len(), 2);
    assert!(
        fs::read(wal::wal_path(&path))
            .unwrap()
            .starts_with(wal::WAL_DELTA_MAGIC)
    );
    assert_eq!(*open().buffer().text(), Doc::from_bytes(b"disk draft!"));
}

#[test]
fn toggling_delta_tracking_drops_the_log_until_a_new_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("draft.txt");
    fs::write(&path, b"disk").unwrap();
    let ed = Editor::new(delta_config());
    let fx = request_open(&ed, &path, None, true, Vec::new());
    let ed = perform_all(ed, None, fx).insert_at(4, b" draft");
    assert_eq!(ed.buffer().wal_deltas().unwrap().1.len(), 1);

    let ed = ed.adopt_settings(Editor::new(Config::default()));
    assert!(ed.buffer().wal_deltas().is_none());
    let ed = ed.set_config(delta_config()).insert_at(10, b"!");
    assert!(ed.buffer().wal_deltas().is_none());
    let ed = handle(ed, None, Event::Idle);
    assert!(
        fs::read(wal::wal_path(&path))
            .unwrap()
            .starts_with(wal::WAL_MAGIC)
    );

    let ed = handle(ed, None, Event::Run("save-buffer".to_owned()));
    assert!(ed.buffer().wal_deltas().unwrap().1.is_empty());
    let ed = ed.set_config(Config::default());
    assert!(ed.buffer().wal_deltas().is_none());
    let ed = ed.set_config(delta_config());
    let ed = handle(ed, None, Event::Run("revert-buffer".to_owned()));
    assert!(ed.buffer().wal_deltas().unwrap().1.is_empty());
}

#[test]
fn neonwal2_recovery_then_edit_and_flush_preserves_recovered_draft() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("legacy.txt");
    fs::write(&file_path, "original").unwrap();

    let disk = file::disk_state(&file_path);
    wal::write_wal(&file_path, disk, Eol::Lf, b"recovered draft").unwrap();

    let lua = lua::new_state().unwrap();
    let ed = lua::bootstrap(Editor::new(Config::default()), &lua);
    let fx = request_open(&ed, &file_path, None, true, Vec::new());
    let ed = perform_all(ed, Some(&lua), fx);
    assert_eq!(
        *ed.buffer().text(),
        neonemacs::core::Doc::from_bytes(b"recovered draft")
    );
    assert!(ed.buffer().modified());

    let mut session = ShellSession::new(ed, lua);
    let t0 = Instant::now();
    press(&mut session, KeyCode::Char('X'), KeyModifiers::NONE, t0);
    session.tick_at(t0 + Duration::from_millis(1000));
    assert!(wal::has_wal(&file_path));
    assert!(!session.ed.buffer().is_wal_dirty());

    // Simulate crash and reopen: must recover the full edited draft, not `Xoriginal`.
    let reopened = Editor::new(Config::default());
    let fx = request_open(&reopened, &file_path, None, true, Vec::new());
    let reopened = perform_all(reopened, None, fx);
    assert_eq!(
        *reopened.buffer().text(),
        neonemacs::core::Doc::from_bytes(b"Xrecovered draft")
    );
    assert!(reopened.buffer().modified());
    assert_eq!(reopened.message(), "[Recovered legacy.txt from WAL]");

    // Toggling EOL without any text delta must also preserve the recovered draft on flush.
    let ed_eol = handle(reopened, None, Event::Run("toggle-eol".to_owned()));
    let ed_eol = handle(ed_eol, None, Event::Idle);
    assert!(!ed_eol.buffer().is_wal_dirty());

    let reopened2 = Editor::new(Config::default());
    let fx = request_open(&reopened2, &file_path, None, true, Vec::new());
    let reopened2 = perform_all(reopened2, None, fx);
    assert_eq!(
        *reopened2.buffer().text(),
        neonemacs::core::Doc::from_bytes(b"Xrecovered draft")
    );
    assert_eq!(reopened2.buffer().eol(), Eol::Crlf);
    assert!(reopened2.buffer().modified());
}

#[test]
fn lf_save_that_normalizes_on_reopen_keeps_snapshot_wal_after_undo() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mixed.txt");
    fs::write(&path, b"a\r\r\nb\r\n").unwrap();
    let open = || {
        let ed = Editor::new(delta_config());
        let fx = request_open(&ed, &path, None, true, Vec::new());
        perform_all(ed, None, fx)
    };
    let doc = neonemacs::core::Doc::from_bytes;
    let ed = open();
    assert_eq!(*ed.buffer().text(), doc(b"a\r\nb\n"));
    let ed = handle(ed, None, Event::Run("toggle-eol".to_owned()));
    let ed = handle(ed, None, Event::Run("save-buffer".to_owned()));
    assert_eq!(fs::read(&path).unwrap(), b"a\r\nb\n");
    assert!(!ed.buffer().modified());
    assert!(ed.buffer().wal_deltas().is_none());

    let ed = ed.insert_at(3, b"Y");
    let ed = handle(ed, None, Event::Run("undo".to_owned()));
    assert_eq!(*ed.buffer().text(), doc(b"a\r\nb\n"));
    assert!(!ed.buffer().modified());
    let ed = handle(ed.insert_at(3, b"X"), None, Event::Idle);
    assert!(!ed.buffer().is_wal_dirty());
    assert!(
        fs::read(wal::wal_path(&path))
            .unwrap()
            .starts_with(wal::WAL_MAGIC)
    );

    let ed = open();
    assert_eq!(*ed.buffer().text(), doc(b"a\r\nXb\n"));
    assert_eq!(ed.buffer().eol(), Eol::Lf);
    assert!(ed.buffer().modified());
}

#[test]
fn unapplicable_delta_wal_is_quarantined_as_stale_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("bad_delta.txt");
    fs::write(&file_path, "hello").unwrap();

    let disk = file::disk_state(&file_path);
    let oob_deltas = [neonemacs::core::Replace {
        offset: 99,
        delete_len: 1,
        insert: neonemacs::core::Doc::from_bytes(b"z"),
    }];
    let base = neonemacs::core::Doc::from_bytes(b"hello");
    let digest = neonemacs::core::wal_base_digest(&base);
    wal::write_wal_deltas(&file_path, disk, Eol::Lf, digest, &oob_deltas).unwrap();
    assert!(wal::has_wal(&file_path));

    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, &file_path, None, true, Vec::new());
    let ed = perform_all(ed, None, fx);
    assert_eq!(
        *ed.buffer().text(),
        neonemacs::core::Doc::from_bytes(b"hello")
    );
    assert!(!ed.buffer().modified());
    assert_eq!(ed.message(), "Stale WAL kept as .bad_delta.txt.wal.stale");
    assert!(!wal::has_wal(&file_path));
    assert!(wal::stale_path(&file_path).is_file());
}

#[test]
fn delta_wal_rejects_a_changed_base_with_identical_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("same_metadata.txt");
    fs::write(&path, b"abc").unwrap();
    let open = || {
        let ed = Editor::new(delta_config());
        let fx = request_open(&ed, &path, None, true, Vec::new());
        perform_all(ed, None, fx)
    };
    let disk = file::disk_state(&path);
    let neonemacs::core::DiskState::Known { mtime, .. } = disk else {
        panic!("file metadata missing")
    };
    let ed = handle(open().insert_at(3, b"X"), None, Event::Idle);
    assert!(!ed.buffer().is_wal_dirty());
    let wp = wal::wal_path(&path);
    let raw = fs::read(&wp).unwrap();
    assert!(raw.starts_with(wal::WAL_DELTA_MAGIC));

    fs::write(&path, b"def").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(file::disk_state(&path), disk);
    assert!(wal::read_wal(&path, disk).is_err());
    let reopened = open();
    assert_eq!(
        *reopened.buffer().text(),
        neonemacs::core::Doc::from_bytes(b"def")
    );
    assert!(!reopened.buffer().modified());
    assert_eq!(fs::read(wal::stale_path(&path)).unwrap(), raw);
    assert!(!wp.exists());
}

#[test]
fn reaching_the_delta_record_ceiling_falls_back_to_a_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("many_edits.txt");
    fs::write(&path, vec![b'a'; 128 * 1024]).unwrap();
    let open = || {
        let ed = Editor::new(delta_config());
        let fx = request_open(&ed, &path, None, true, Vec::new());
        perform_all(ed, None, fx)
    };
    let ed =
        (0..neonemacs::core::MAX_WAL_DELTA_RECORDS).fold(open(), |ed, _| ed.insert_at(0, b"x"));
    assert_eq!(
        ed.buffer().wal_deltas().unwrap().1.len(),
        neonemacs::core::MAX_WAL_DELTA_RECORDS
    );
    let ed = handle(ed.insert_at(0, b"Y"), None, Event::Idle);
    assert!(ed.buffer().wal_deltas().is_none());
    assert!(!ed.buffer().is_wal_dirty());
    assert!(
        fs::read(wal::wal_path(&path))
            .unwrap()
            .starts_with(wal::WAL_MAGIC)
    );
    assert_eq!(open().buffer().text(), ed.buffer().text());
}

#[test]
fn delta_wal_in_scratch_slot_is_quarantined_as_stale() {
    let dir = tempfile::tempdir().unwrap();
    let slot = dir.path().join("scratch.wal");
    let mut raw = wal::encode_wal_header(DiskState::Unknown, Eol::Lf).to_vec();
    raw[..8].copy_from_slice(wal::WAL_DELTA_MAGIC);
    raw.extend_from_slice(&neonemacs::core::wal_base_digest(
        &neonemacs::core::Doc::default(),
    ));
    raw.extend_from_slice(&0u64.to_le_bytes());
    raw.extend_from_slice(&0u64.to_le_bytes());
    raw.extend_from_slice(&1u64.to_le_bytes());
    raw.push(b'x');
    fs::write(&slot, &raw).unwrap();

    let ed = neonemacs::shell::startup::restore_scratch(
        Editor::new(Config::default()).with_scratch_slot(Some(slot.clone())),
    );
    assert!(!ed.buffer().modified());
    assert_eq!(ed.message(), "Stale WAL kept as scratch.wal.stale");
    assert!(!slot.exists());
    assert!(dir.path().join("scratch.wal.stale").is_file());
}
