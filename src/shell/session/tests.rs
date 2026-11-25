use super::screen::print_styled_row;
use super::*;
use crate::core::Config;
use crate::core::Confirm;
use crate::core::Doc;
use crate::core::Editor;
use crate::core::TokenKind;
use crate::core::request_open;
use crate::shell::ShellTask;
use crate::shell::lua;
use crate::shell::startup::InitSource;
use crate::shell::wal;
use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseEvent,
    MouseEventKind,
};
use crossterm::queue;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use std::fs;
use std::sync::mpsc;

fn make_session() -> ShellSession {
    let lua = lua::new_state().unwrap();
    ShellSession::new(lua::bootstrap(Editor::new(Config::default()), &lua), lua)
}

fn key_event(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> TermEvent {
    TermEvent::Key(KeyEvent {
        code,
        modifiers,
        kind,
        state: KeyEventState::NONE,
    })
}

fn press(code: KeyCode, modifiers: KeyModifiers) -> TermEvent {
    key_event(code, modifiers, KeyEventKind::Press)
}

#[test]
fn hostile_event_filtering_ignores_release_repeat_mouse_resize_and_unmapped_keys() {
    let mut session = make_session();
    let t0 = Instant::now();

    let ignored_events = [
        key_event(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ),
        key_event(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Repeat),
        press(KeyCode::F(12), KeyModifiers::NONE),
        press(KeyCode::CapsLock, KeyModifiers::NONE),
        TermEvent::Resize(120, 40),
        TermEvent::FocusGained,
        TermEvent::FocusLost,
        TermEvent::Paste("injected".to_owned()),
        TermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }),
    ];

    for (i, ev) in ignored_events.into_iter().enumerate() {
        session.handle_event_at(&ev, t0 + Duration::from_secs(i as u64 + 1));
    }

    assert_eq!(*session.ed.buffer().text(), Doc::from_bytes(b""));
    assert!(!session.ed.buffer().modified());
    assert!(session.wal_deadline.is_none());
    assert!(!session.is_quit());

    // Only KeyEventKind::Press with a mapped key takes effect
    session.handle_event_at(
        &press(KeyCode::Char('x'), KeyModifiers::NONE),
        t0 + Duration::from_secs(10),
    );
    assert_eq!(*session.ed.buffer().text(), Doc::from_bytes(b"x"));
}

#[test]
fn spinner_tick_wrapping_and_prompt_mode_echo_preservation() {
    let mut session = make_session();
    let (tx, rx) = mpsc::channel();
    let mut active = ActiveShell::new(ShellTask { receiver: rx });
    active.tick = usize::MAX;
    session.active_shell = Some(active);

    // 1. usize::MAX wrapping does not panic
    let t0 = Instant::now();
    session.tick_at(t0);
    assert_eq!(session.ed.message(), "[Running... |]");

    // 2. Entering M-x prompt while shell is running must NOT be overwritten by tick()
    session.handle_event_at(&press(KeyCode::Char('x'), KeyModifiers::ALT), t0);
    assert!(matches!(session.ed.mode(), Mode::Prompt(_)));
    session.handle_event_at(&press(KeyCode::Char('g'), KeyModifiers::NONE), t0);
    assert_eq!(session.ed.message(), "M-x g");

    // Tick multiple times while user is typing in prompt: prompt message stays intact
    session.tick_at(t0 + Duration::from_millis(50));
    session.tick_at(t0 + Duration::from_millis(100));
    assert_eq!(session.ed.message(), "M-x g");

    // 3. Cancel prompt with C-g -> back to Mode::Normal -> next tick resumes spinner
    session.handle_event_at(&press(KeyCode::Char('g'), KeyModifiers::CONTROL), t0);
    assert!(matches!(session.ed.mode(), Mode::Normal { .. }));
    session.tick_at(t0 + Duration::from_millis(150));
    assert!(session.ed.message().starts_with("[Running... "));

    drop(tx);
}

#[test]
fn spinner_never_overwrites_a_message_it_did_not_write() {
    let mut session = make_session();
    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    session.active_shell = Some(ActiveShell::new(ShellTask { receiver: rx }));
    let t0 = Instant::now();

    session.tick_at(t0);
    assert_eq!(session.ed.message(), "[Running... /]");

    // A message from anything else, here the echo of a Lua `message` call, stays until replaced.
    session.ed = session.ed.set_message("Wrote notes.txt");
    session.tick_at(t0 + Duration::from_millis(50));
    session.tick_at(t0 + Duration::from_millis(100));
    assert_eq!(session.ed.message(), "Wrote notes.txt");

    // An empty echo area gets the spinner back.
    session.ed = session.ed.clear_message();
    session.tick_at(t0 + Duration::from_millis(150));
    assert!(session.ed.message().starts_with("[Running... "));

    drop(tx);
}

#[test]
fn background_shell_disconnect_clears_active_shell_and_reports_error() {
    let mut session = make_session();
    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    session.active_shell = Some(ActiveShell::new(ShellTask { receiver: rx }));
    session.ed = session.ed.set_message("[Running... |]");

    // Simulate worker thread panic / abrupt channel drop without sending
    drop(tx);

    session.tick_at(Instant::now());
    assert!(session.active_shell.is_none());
    assert_eq!(
        session.ed.message(),
        "Shell command terminated unexpectedly"
    );
}

#[test]
fn busy_rejection_attaches_notice_and_completion_delivers_multiline_output() {
    let mut session = make_session();
    let t0 = Instant::now();
    let (tx, rx) = mpsc::channel();
    session.active_shell = Some(ActiveShell::new(ShellTask { receiver: rx }));

    // Attempt to run a second shell command while first is active: M-x shell-command RET echo 2 RET
    session.handle_event_at(&press(KeyCode::Char('x'), KeyModifiers::ALT), t0);
    for c in "shell-command".chars() {
        session.handle_event_at(&press(KeyCode::Char(c), KeyModifiers::NONE), t0);
    }
    session.handle_event_at(&press(KeyCode::Enter, KeyModifiers::NONE), t0);
    for c in "echo 2".chars() {
        session.handle_event_at(&press(KeyCode::Char(c), KeyModifiers::NONE), t0);
    }
    session.handle_event_at(&press(KeyCode::Enter, KeyModifiers::NONE), t0);

    assert_eq!(
        session.ed.message(),
        "[Running...] A shell command is already running"
    );

    // Next tick keeps the AlreadyRunning notice while advancing the spinner
    session.tick_at(t0 + Duration::from_millis(50));
    assert!(
        session
            .ed
            .message()
            .ends_with("] A shell command is already running")
    );

    // Complete the first task with multiline output -> creates *Shell Command Output* buffer
    tx.send(Ok("line1\nline2\n".to_owned())).unwrap();
    session.tick_at(t0 + Duration::from_millis(100));
    assert!(session.active_shell.is_none());
    assert!(
        session
            .ed
            .buffers()
            .iter()
            .any(|b| b.name() == "*Shell Command Output*"
                && *b.text() == Doc::from_bytes(b"line1\nline2"))
    );
}

#[test]
fn deterministic_wal_flush_boundaries_debounce_and_save_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("a.txt");
    let file_b = dir.path().join("b.txt");
    fs::write(&file_a, "init_a").unwrap();
    fs::write(&file_b, "init_b").unwrap();

    let mut session = make_session();
    let fx_a = request_open(&session.ed, &file_a, None, true, Vec::new());
    session.ed = perform_all(session.ed, Some(&session.lua), fx_a);
    let fx_b = request_open(&session.ed, &file_b, None, false, Vec::new());
    session.ed = perform_all(session.ed, Some(&session.lua), fx_b);

    let t0 = Instant::now();

    // 1. Non-modifying cursor key in clean file buffer does NOT arm WAL
    session.handle_event_at(&press(KeyCode::Right, KeyModifiers::NONE), t0);
    assert!(session.wal_deadline.is_none());

    // 2. Edit file_a at t0, then again at t0 + 600ms (debounce)
    session.handle_event_at(&press(KeyCode::Char('!'), KeyModifiers::NONE), t0);
    assert!(session.wal_deadline.is_some());

    let t_edit2 = t0 + Duration::from_millis(600);
    session.handle_event_at(&press(KeyCode::Char('?'), KeyModifiers::NONE), t_edit2);

    // At t_edit2 + 999ms (1599ms after t0): still 1ms short of 1s idle threshold
    session.tick_at(t_edit2 + Duration::from_millis(999));
    assert!(!wal::has_wal(&file_a));
    assert!(session.wal_deadline.is_some());

    // At exact t_edit2 + 1000ms: flushes only modified file_a, not clean file_b
    session.tick_at(t_edit2 + Duration::from_millis(1000));
    assert!(wal::has_wal(&file_a));
    assert!(!wal::has_wal(&file_b));
    assert!(session.wal_deadline.is_none());
    assert_eq!(
        wal::read_wal(&file_a, crate::shell::file::disk_state(&file_a))
            .unwrap()
            .doc,
        Doc::from_bytes(b"i!?nit_a")
    );

    // 3. Edit again, then save (C-x C-s) before 1s elapses -> disarms WAL flush & removes .wal
    let t_edit3 = t0 + Duration::from_secs(3);
    session.handle_event_at(&press(KeyCode::Char('#'), KeyModifiers::NONE), t_edit3);
    assert!(session.wal_deadline.is_some());

    session.handle_event_at(
        &press(KeyCode::Char('x'), KeyModifiers::CONTROL),
        t_edit3 + Duration::from_millis(100),
    );
    session.handle_event_at(
        &press(KeyCode::Char('s'), KeyModifiers::CONTROL),
        t_edit3 + Duration::from_millis(150),
    );
    assert!(!wal::has_wal(&file_a));
    assert!(session.wal_deadline.is_none());

    // Even after 2 seconds pass, tick_at must NOT recreate .wal for saved buffer
    session.tick_at(t_edit3 + Duration::from_secs(5));
    assert!(!wal::has_wal(&file_a));
}

#[test]
fn cursor_motion_does_not_retrigger_or_delay_wal_flush() {
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("cursor_test.txt");
    fs::write(&file_a, "init_text").unwrap();

    let mut session = make_session();
    let fx = request_open(&session.ed, &file_a, None, true, Vec::new());
    session.ed = perform_all(session.ed, Some(&session.lua), fx);

    let t0 = Instant::now();
    // 1. Edit buffer at t0
    session.handle_event_at(&press(KeyCode::Char('X'), KeyModifiers::NONE), t0);
    assert!(session.wal_deadline.is_some());

    // 2. Move cursor at t0 + 500ms: must NOT postpone the 1s flush timeout!
    session.handle_event_at(
        &press(KeyCode::Right, KeyModifiers::NONE),
        t0 + Duration::from_millis(500),
    );
    assert!(session.wal_deadline.is_some());

    // At t0 + 1000ms: flush must happen (not delayed by Right arrow!)
    session.tick_at(t0 + Duration::from_millis(1000));
    assert!(wal::has_wal(&file_a));
    assert!(session.wal_deadline.is_none());

    // 3. Move cursor at t0 + 2000ms: must NOT re-arm the WAL deadline or re-write WAL!
    session.handle_event_at(
        &press(KeyCode::Right, KeyModifiers::NONE),
        t0 + Duration::from_millis(2000),
    );
    assert!(
        session.wal_deadline.is_none(),
        "cursor movement on already-flushed buffer re-armed WAL flush!"
    );
}

#[test]
fn wal_write_failure_is_reported_and_retried() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("retry.txt");
    fs::write(&file, "init").unwrap();
    // A directory at the WAL path makes the atomic rename fail on every platform.
    let blocker = wal::wal_path(&file);
    fs::create_dir(&blocker).unwrap();

    let mut session = make_session();
    let fx = request_open(&session.ed, &file, None, true, Vec::new());
    session.ed = perform_all(session.ed, Some(&session.lua), fx);

    let t0 = Instant::now();
    session.handle_event_at(&press(KeyCode::Char('X'), KeyModifiers::NONE), t0);

    // The write fails: the user is told, and the buffer stays queued for another try.
    let t1 = t0 + Duration::from_millis(1000);
    session.tick_at(t1);
    assert!(session.ed.message().starts_with("WAL write failed: "));
    assert!(session.ed.message().contains("retry.txt"));
    assert!(session.wal_deadline.is_some());
    assert!(session.has_wal_dirty_file_buffer());

    // No retry before another second has passed.
    fs::remove_dir(&blocker).unwrap();
    session.tick_at(t1 + Duration::from_millis(999));
    assert!(!wal::has_wal(&file));

    session.tick_at(t1 + Duration::from_millis(1000));
    assert!(wal::has_wal(&file));
    assert!(session.wal_deadline.is_none());
    assert!(!session.has_wal_dirty_file_buffer());
}

#[test]
fn wal_write_failure_keeps_a_pending_confirmation_question() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ask.txt");
    fs::write(&file, "init").unwrap();
    fs::create_dir(wal::wal_path(&file)).unwrap();

    let mut session = make_session();
    let fx = request_open(&session.ed, &file, None, true, Vec::new());
    session.ed = perform_all(session.ed, Some(&session.lua), fx);

    let t0 = Instant::now();
    session.handle_event_at(&press(KeyCode::Char('X'), KeyModifiers::NONE), t0);
    session.ed = session.ed.confirm(
        Confirm::QuitUnsaved,
        "Modified buffers exist; quit? (y or n)",
    );

    session.tick_at(t0 + Duration::from_millis(1000));
    assert_eq!(
        session.ed.message(),
        "Modified buffers exist; quit? (y or n)"
    );
    // The failure is still retried.
    assert!(session.wal_deadline.is_some());
}

#[test]
fn print_styled_row_reverses_exactly_the_marked_cells() {
    use crossterm::style::{Attribute, SetAttribute};
    let cell = |glyph: &str, reverse: bool| crate::core::render::Cell {
        glyph: glyph.into(),
        style: TokenKind::Normal,
        reverse,
    };
    let mut out = Vec::new();
    print_styled_row(
        &mut out,
        &[
            cell("a", false),
            cell("b", true),
            cell("c", true),
            cell("d", false),
        ],
    )
    .unwrap();
    let mut expected = Vec::new();
    queue!(
        &mut expected,
        Print("a"),
        SetAttribute(Attribute::Reverse),
        Print("bc"),
        SetAttribute(Attribute::NoReverse),
        Print("d"),
        ResetColor
    )
    .unwrap();
    assert_eq!(out, expected);

    // A row that ends inside the region does not leak reverse video into the next row.
    let mut out = Vec::new();
    print_styled_row(&mut out, &[cell("a", false), cell("b", true)]).unwrap();
    let mut expected = Vec::new();
    queue!(
        &mut expected,
        Print("a"),
        SetAttribute(Attribute::Reverse),
        Print("b"),
        SetAttribute(Attribute::NoReverse),
        ResetColor
    )
    .unwrap();
    assert_eq!(out, expected);
}

#[test]
fn print_styled_row_aligns_wide_hangul_and_combining_marks_without_shifting_colors() {
    // Line: `"가" let`
    // Cell widths: `"`(1, String), `가`(2, String), `"`(1, String), ` `(1, Normal), `l`(1, Keyword), `e`(1, Keyword), `t`(1, Keyword)
    // Notice `styles` has 8 entries (2 cells for `가`), while `line` has 7 chars!
    let cells = [
        crate::core::render::Cell {
            glyph: "\"".into(),
            style: TokenKind::String,
            reverse: false,
        },
        crate::core::render::Cell {
            glyph: "가".into(),
            style: TokenKind::String,
            reverse: false,
        },
        crate::core::render::Cell {
            glyph: String::new(),
            style: TokenKind::String,
            reverse: false,
        },
        crate::core::render::Cell {
            glyph: "\"".into(),
            style: TokenKind::String,
            reverse: false,
        },
        crate::core::render::Cell {
            glyph: " ".into(),
            style: TokenKind::Normal,
            reverse: false,
        },
        crate::core::render::Cell {
            glyph: "l".into(),
            style: TokenKind::Keyword,
            reverse: false,
        },
        crate::core::render::Cell {
            glyph: "e".into(),
            style: TokenKind::Keyword,
            reverse: false,
        },
        crate::core::render::Cell {
            glyph: "t".into(),
            style: TokenKind::Keyword,
            reverse: false,
        },
    ];

    let mut out = Vec::new();
    print_styled_row(&mut out, &cells).unwrap();

    let mut expected = Vec::new();
    queue!(
        &mut expected,
        SetForegroundColor(Color::Green),
        Print("\"가\""),
        ResetColor,
        Print(" "),
        SetForegroundColor(Color::Cyan),
        Print("let"),
        ResetColor
    )
    .unwrap();
    assert_eq!(out, expected);

    // Combining mark (`e` + U+0301 acute accent) inside a Keyword followed by Number `1`:
    // `e\u{0301}` occupies 1 cell in `styles` (`Keyword`), `' '` occupies 1 cell (`Normal`), `'1'` occupies 1 cell (`Number`).
    let comb_cells = [
        crate::core::render::Cell {
            glyph: "e\u{0301}".into(),
            style: TokenKind::Keyword,
            reverse: false,
        },
        crate::core::render::Cell {
            glyph: " ".into(),
            style: TokenKind::Normal,
            reverse: false,
        },
        crate::core::render::Cell {
            glyph: "1".into(),
            style: TokenKind::Number,
            reverse: false,
        },
    ];
    let mut comb_out = Vec::new();
    print_styled_row(&mut comb_out, &comb_cells).unwrap();

    let mut comb_expected = Vec::new();
    queue!(
        &mut comb_expected,
        SetForegroundColor(Color::Cyan),
        Print("e\u{0301}"),
        ResetColor,
        Print(" "),
        SetForegroundColor(Color::Yellow),
        Print("1"),
        ResetColor
    )
    .unwrap();
    assert_eq!(comb_out, comb_expected);
}

#[test]
fn render_sized_caches_unchanged_rows_redraws_style_changes_and_handles_resize() {
    let mut session = make_session();
    session.ed = session.ed.with_current_buffer(|b| {
        b.set_name("plain.txt")
            .with_text(Doc::from_bytes(b"let x = 42;"))
    });

    // 1. Initial render draws all rows
    let mut out1 = Vec::new();
    session.render_sized(&mut out1, 20, 3).unwrap();
    let s1 = String::from_utf8(out1).unwrap();
    assert!(s1.contains("let x = 42;"));
    // plain.txt has no syntax color (no Cyan \x1b[38;5;14m or \x1b[36m)
    let mut cyan_probe = Vec::new();
    queue!(&mut cyan_probe, SetForegroundColor(Color::Cyan)).unwrap();
    let cyan_seq = String::from_utf8(cyan_probe).unwrap();
    assert!(!s1.contains(&cyan_seq));

    // 2. Second identical render emits NO row text, only cursor movement
    let mut out2 = Vec::new();
    session.render_sized(&mut out2, 20, 3).unwrap();
    let s2 = String::from_utf8(out2).unwrap();
    assert!(!s2.contains("let x = 42;"));

    // 3. Rename buffer to `code.rs` while keeping row 0 text identical ("let x = 42;")
    // Row 0 must be redrawn because `styles[0]` changed from Normal to Keyword/Number!
    session.ed = session.ed.with_current_buffer(|b| b.set_name("code.rs"));
    let mut out3 = Vec::new();
    session.render_sized(&mut out3, 20, 3).unwrap();
    let s3 = String::from_utf8(out3).unwrap();
    assert!(s3.contains("let"));
    assert!(s3.contains(&cyan_seq));

    // 4. Resize invalidates cache and forces full redraw even when editor state is unchanged
    let mut out4 = Vec::new();
    session.render_sized(&mut out4, 25, 4).unwrap();
    let s4 = String::from_utf8(out4).unwrap();
    assert!(s4.contains("let"));

    // 5. Degenerate 0x0 and 1x1 dimensions never panic
    let mut out_zero = Vec::new();
    session.render_sized(&mut out_zero, 0, 0).unwrap();
    session.render_sized(&mut out_zero, 1, 1).unwrap();
}

fn bound(session: &ShellSession, keys: &str) -> Option<String> {
    session
        .ed
        .keymap()
        .lookup(&crate::core::parse_key_seq(keys).unwrap())
        .map(str::to_owned)
}

fn reload(session: &mut ShellSession, src: &str) {
    session.reload_init(InitSource::Loaded(src.to_owned()));
}

#[test]
fn reload_init_replaces_settings_and_keeps_documents() {
    // I-58
    let mut session = make_session();
    session.handle_event(&press(KeyCode::Char('a'), KeyModifiers::NONE));
    session.handle_event(&press(KeyCode::Char('b'), KeyModifiers::NONE));

    reload(
        &mut session,
        "bind(\"C-x 9\", \"hello\")\nset(\"fill-column\", 85)\n\
         command(\"hello\", function() message(\"hi\") end)",
    );
    assert_eq!(session.ed.message(), "Loaded init.lua");
    assert_eq!(bound(&session, "C-x 9").as_deref(), Some("hello"));
    assert_eq!(session.ed.config().fill_column, 85);
    assert_eq!(*session.ed.buffer().text(), Doc::from_bytes(b"ab"));
    assert!(session.ed.buffer().modified());

    // The new Lua state holds the new command.
    session.handle_event(&press(KeyCode::Char('x'), KeyModifiers::CONTROL));
    session.handle_event(&press(KeyCode::Char('9'), KeyModifiers::NONE));
    assert_eq!(session.ed.message(), "hi");

    // A setting that the new file no longer makes is gone, as after a restart.
    reload(&mut session, "");
    assert_eq!(bound(&session, "C-x 9"), None);
    assert_eq!(session.ed.config().fill_column, 70);
    assert_eq!(
        bound(&session, "C-x 2").as_deref(),
        Some("split-window-below")
    );

    // The undo history survived both reloads.
    session.handle_event(&press(KeyCode::Char('/'), KeyModifiers::CONTROL));
    assert_eq!(*session.ed.buffer().text(), Doc::from_bytes(b""));

    session.reload_init(InitSource::Absent);
    assert_eq!(session.ed.message(), "No init.lua");
}

#[test]
fn a_failed_reload_keeps_the_previous_settings_and_commands() {
    // I-54
    let mut session = make_session();
    reload(
        &mut session,
        "bind(\"C-x 9\", \"hello\")\ncommand(\"hello\", function() message(\"hi\") end)",
    );

    reload(
        &mut session,
        "bind(\"C-x 8\", \"forward-char\")\nunbind(\"C-x 9\")\nerror(\"boom\")",
    );
    assert!(
        session.ed.message().starts_with("init.lua: ") && session.ed.message().contains("boom"),
        "{}",
        session.ed.message()
    );
    assert_eq!(bound(&session, "C-x 8"), None);
    assert_eq!(bound(&session, "C-x 9").as_deref(), Some("hello"));
    let log = session.ed.buffers().find_by_name("*init-log*").unwrap();
    assert!(
        String::from_utf8_lossy(&crate::test_support::doc_bytes(log.text()))
            .contains("init.lua:3:")
    );

    session.handle_event(&press(KeyCode::Char('x'), KeyModifiers::CONTROL));
    session.handle_event(&press(KeyCode::Char('9'), KeyModifiers::NONE));
    assert_eq!(session.ed.message(), "hi");

    session.reload_init(InitSource::Unreadable {
        reason: "cannot read x".to_owned(),
    });
    assert_eq!(session.ed.message(), "init.lua: cannot read x");
    assert_eq!(bound(&session, "C-x 9").as_deref(), Some("hello"));

    // A later success empties the log.
    reload(&mut session, "");
    let log = session.ed.buffers().find_by_name("*init-log*").unwrap();
    assert_eq!(*log.text(), Doc::from_bytes(b""));
}

#[test]
fn reload_init_from_lua_is_refused() {
    let mut session = make_session();
    reload(
        &mut session,
        "bind(\"C-x 9\", \"again\")\ncommand(\"again\", function() run(\"reload-init\") end)",
    );
    session.handle_event(&press(KeyCode::Char('x'), KeyModifiers::CONTROL));
    session.handle_event(&press(KeyCode::Char('9'), KeyModifiers::NONE));
    assert_eq!(session.ed.message(), "reload-init: not available from Lua");
    assert_eq!(bound(&session, "C-x 9").as_deref(), Some("again"));
}
