use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
};
use neonemacs::core::{Backing, Config, Doc, Editor, FilePath};
use neonemacs::shell::lua;
use neonemacs::shell::recovery::{parse_recovery_file, recovery_file_name};
use neonemacs::shell::session::ShellSession;
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

#[cfg(unix)]
#[test]
fn pty_sighup_triggers_emergency_dump() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Write;

    let bin = env!("CARGO_BIN_EXE_neonemacs");
    let state_dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let target = work_dir.path().join("unsaved.txt");
    fs::write(&target, b"original").unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(bin);
    cmd.arg(&target);
    cmd.env("XDG_STATE_HOME", state_dir.path());
    cmd.env("XDG_CONFIG_HOME", state_dir.path());

    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let pid = child.process_id().expect("child pid");
    let mut writer = pair.master.take_writer().unwrap();

    thread::sleep(Duration::from_millis(200));
    writer.write_all(b"Z").unwrap();
    writer.flush().unwrap();
    thread::sleep(Duration::from_millis(200));

    kill(Pid::from_raw(pid as i32), Signal::SIGHUP).unwrap();
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "expected clean exit on SIGHUP: {status:?}"
    );
    assert_eq!(fs::read(&target).unwrap(), b"original");

    let rec_dir = state_dir.path().join("neonemacs").join("recovery");
    let entries: Vec<_> = fs::read_dir(&rec_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".rec"))
        .collect();
    assert_eq!(entries.len(), 1);
    let raw = fs::read(entries[0].path()).unwrap();
    let (_, body) = parse_recovery_file(&raw).unwrap();
    assert_eq!(body, b"Zoriginal");
}
