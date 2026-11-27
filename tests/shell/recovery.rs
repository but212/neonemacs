use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
};
use neonemacs::core::{Backing, BufferId, Config, Doc, Editor, Eol, FilePath, Mode};
use neonemacs::shell::lua;
use neonemacs::shell::recovery::{
    RecoverySnapshot, parse_recovery_file, recovery_file_name, write_recovery_file,
};
use neonemacs::shell::session::ShellSession;
use neonemacs::shell::startup::{FileArg, open_files_with_recovery_dir};
use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

fn press(code: KeyCode, modifiers: KeyModifiers) -> TermEvent {
    TermEvent::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    cond()
}

fn make_file_session(
    file_path: &Path,
    rec_dir: &Path,
) -> (ShellSession, neonemacs::core::BufferId) {
    let lua = lua::new_state().unwrap();
    let ed = lua::bootstrap(Editor::new(Config::default()), &lua);
    let fp = FilePath::resolve("", file_path.to_str().unwrap());
    let disk = neonemacs::shell::file::disk_state(file_path);
    let ed = ed.with_current_buffer(|b| {
        b.set_name("note.txt")
            .set_backing(Backing::file(fp, disk))
            .with_text(Doc::from_bytes(b"init"))
    });
    let id = ed.buffer().id();
    let mut session = ShellSession::with_recovery_dir(ed, lua, Some(rec_dir.to_path_buf()));
    session.set_idle_backup_delay(Duration::from_millis(15));
    (session, id)
}

#[test]
fn idle_backup_writes_rec_and_removes_on_save_and_kill() {
    let orig_dir = tempfile::tempdir().unwrap();
    let rec_dir = tempfile::tempdir().unwrap();
    let target_file = orig_dir.path().join("note.txt");
    fs::write(&target_file, b"init").unwrap();

    let (mut session, id) = make_file_session(&target_file, rec_dir.path());
    session.handle_event(&press(KeyCode::Char('X'), KeyModifiers::NONE));
    assert!(session.ed.buffer_by_id(id).modified());

    thread::sleep(Duration::from_millis(30));
    session.tick();

    let expected_rec = rec_dir.path().join(recovery_file_name(
        std::process::id(),
        id,
        session.ed.buffer_by_id(id).file_path(),
    ));
    assert!(
        wait_until(Duration::from_secs(2), || expected_rec.exists()),
        "idle backup should create .rec file"
    );

    // I-62: original file remains untouched
    assert_eq!(fs::read(&target_file).unwrap(), b"init");

    let raw = fs::read(&expected_rec).unwrap();
    let (meta, body) = parse_recovery_file(&raw).unwrap();
    assert_eq!(meta.buffer_id, id);
    assert_eq!(body, b"Xinit");

    // I-65: saving (C-x C-s) clears modified and removes .rec file
    session.handle_event(&press(KeyCode::Char('x'), KeyModifiers::CONTROL));
    session.handle_event(&press(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(!session.ed.buffer_by_id(id).modified());
    assert!(
        wait_until(Duration::from_secs(2), || !expected_rec.exists()),
        "saving should remove .rec file"
    );

    // Edit again -> idle backup -> kill-buffer removes .rec file
    session.handle_event(&press(KeyCode::Char('Y'), KeyModifiers::NONE));
    thread::sleep(Duration::from_millis(30));
    session.tick();
    assert!(wait_until(Duration::from_secs(2), || expected_rec.exists()));

    // C-x k RET y (kill modified buffer)
    session.handle_event(&press(KeyCode::Char('x'), KeyModifiers::CONTROL));
    session.handle_event(&press(KeyCode::Char('k'), KeyModifiers::NONE));
    session.handle_event(&press(KeyCode::Enter, KeyModifiers::NONE));
    session.handle_event(&press(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(
        wait_until(Duration::from_secs(2), || !expected_rec.exists()),
        "killing buffer should remove .rec file"
    );
}

#[test]
fn open_files_prompts_and_restores_orphan_rec_files_sequentially() {
    let state_dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let file_a = work_dir.path().join("crash_a.txt");
    let file_b = work_dir.path().join("crash_b.txt");
    fs::write(&file_a, b"on-disk-a").unwrap();
    fs::write(&file_b, b"on-disk-b").unwrap();

    let resolved_a = FilePath::resolve("", file_a.to_str().unwrap());
    let resolved_b = FilePath::resolve("", file_b.to_str().unwrap());
    let rec_dir = state_dir.path().join("neonemacs").join("recovery");
    fs::create_dir_all(&rec_dir).unwrap();

    // Two orphan .rec files for crash_a.txt (older + newer) and one for crash_b.txt.
    let rec_a1 = write_recovery_file(
        &rec_dir,
        std::process::id().wrapping_add(1),
        &RecoverySnapshot {
            buffer_id: BufferId(1),
            path: Some(resolved_a.as_str().to_owned()),
            name: "crash_a.txt".to_owned(),
            doc: Doc::from_bytes(b"recovered-a"),
            eol: Eol::Lf,
        },
    )
    .unwrap();
    let rec_a2 = write_recovery_file(
        &rec_dir,
        std::process::id().wrapping_add(2),
        &RecoverySnapshot {
            buffer_id: BufferId(1),
            path: Some(resolved_a.as_str().to_owned()),
            name: "crash_a.txt".to_owned(),
            doc: Doc::from_bytes(b"recovered-a"),
            eol: Eol::Lf,
        },
    )
    .unwrap();
    let rec_b = write_recovery_file(
        &rec_dir,
        std::process::id().wrapping_add(3),
        &RecoverySnapshot {
            buffer_id: BufferId(2),
            path: Some(resolved_b.as_str().to_owned()),
            name: "crash_b.txt".to_owned(),
            doc: Doc::from_bytes(b"recovered-b"),
            eol: Eol::Lf,
        },
    )
    .unwrap();
    assert!(rec_a1.exists() && rec_a2.exists() && rec_b.exists());

    let lua = lua::new_state().unwrap();
    let ed = lua::bootstrap(Editor::new(Config::default()), &lua);
    let ed = open_files_with_recovery_dir(
        ed,
        Some(&lua),
        &[
            FileArg {
                path: file_a,
                line: None,
            },
            FileArg {
                path: file_b,
                line: None,
            },
        ],
        Some(&rec_dir),
    );
    assert!(matches!(ed.mode(), Mode::Confirm { .. }));
    assert!(ed.message().contains("crash_a.txt"));

    let mut session = ShellSession::with_recovery_dir(ed, lua, None);
    // Confirm first file: restores crash_a.txt, deletes both .rec files for it, and advances to crash_b.txt.
    session.handle_event(&press(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(!rec_a1.exists() && !rec_a2.exists());
    assert!(matches!(session.ed.mode(), Mode::Confirm { .. }));
    assert!(session.ed.message().contains("crash_b.txt"));

    // Decline second file: keeps on-disk-b, deletes rec_b, and returns to Normal mode.
    session.handle_event(&press(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(!rec_b.exists());
    assert!(matches!(session.ed.mode(), Mode::Normal { .. }));

    let buf_a = session
        .ed
        .buffers()
        .find_by_path(resolved_a.as_str())
        .unwrap();
    assert_eq!(*buf_a.text(), Doc::from_bytes(b"recovered-a"));
    assert!(buf_a.modified());

    let buf_b = session
        .ed
        .buffers()
        .find_by_path(resolved_b.as_str())
        .unwrap();
    assert_eq!(*buf_b.text(), Doc::from_bytes(b"on-disk-b"));
    assert!(!buf_b.modified());
}

#[test]
fn idle_backup_skips_scratch_buffer_without_path() {
    let rec_dir = tempfile::tempdir().unwrap();
    let lua = lua::new_state().unwrap();
    let ed = lua::bootstrap(Editor::new(Config::default()), &lua);
    let scratch_id = ed.buffer().id();
    assert!(ed.buffer_by_id(scratch_id).file_path().is_none());

    let mut session = ShellSession::with_recovery_dir(ed, lua, Some(rec_dir.path().to_path_buf()));
    session.set_idle_backup_delay(Duration::from_millis(15));
    session.handle_event(&press(KeyCode::Char('S'), KeyModifiers::NONE));
    assert!(session.ed.buffer_by_id(scratch_id).modified());

    thread::sleep(Duration::from_millis(30));
    session.tick();
    thread::sleep(Duration::from_millis(50));

    assert_eq!(fs::read_dir(rec_dir.path()).unwrap().count(), 0);
}
