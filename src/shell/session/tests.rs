use super::screen::print_styled_row;
use super::*;
use crate::core::Config;
use crate::core::Doc;
use crate::core::Editor;
use crate::core::TokenKind;
use crate::shell::lua;
use crate::shell::startup::InitSource;
use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseEvent,
    MouseEventKind,
};
use crossterm::queue;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
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

    for ev in ignored_events {
        session.handle_event(&ev);
    }

    assert_eq!(*session.ed.buffer().text(), Doc::from_bytes(b""));
    assert!(!session.ed.buffer().modified());
    assert!(!session.is_quit());

    // Only KeyEventKind::Press with a mapped key takes effect
    session.handle_event(&press(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(*session.ed.buffer().text(), Doc::from_bytes(b"x"));
}

#[test]
fn spinner_tick_wrapping_and_prompt_mode_echo_preservation() {
    let mut session = make_session();
    let (tx, rx) = mpsc::channel();
    let mut active = ActiveShell::new(rx);
    active.tick = usize::MAX;
    session.active_shell = Some(active);

    // 1. usize::MAX wrapping does not panic
    session.tick();
    assert_eq!(session.ed.message(), "[Running... |]");

    // 2. Entering M-x prompt while shell is running must NOT be overwritten by tick()
    session.handle_event(&press(KeyCode::Char('x'), KeyModifiers::ALT));
    assert!(matches!(session.ed.mode(), Mode::Prompt(_)));
    session.handle_event(&press(KeyCode::Char('g'), KeyModifiers::NONE));
    assert_eq!(session.ed.message(), "M-x g");

    // Tick multiple times while user is typing in prompt: prompt message stays intact
    session.tick();
    session.tick();
    assert_eq!(session.ed.message(), "M-x g");

    // 3. Cancel prompt with C-g -> back to Mode::Normal -> next tick resumes spinner
    session.handle_event(&press(KeyCode::Char('g'), KeyModifiers::CONTROL));
    assert!(matches!(session.ed.mode(), Mode::Normal { .. }));
    session.tick();
    assert!(session.ed.message().starts_with("[Running... "));

    drop(tx);
}

#[test]
fn spinner_never_overwrites_a_message_it_did_not_write() {
    let mut session = make_session();
    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    session.active_shell = Some(ActiveShell::new(rx));

    session.tick();
    assert_eq!(session.ed.message(), "[Running... /]");

    // A message from anything else, here the echo of a Lua `message` call, stays until replaced.
    session.ed = session.ed.set_message("Wrote notes.txt");
    session.tick();
    session.tick();
    assert_eq!(session.ed.message(), "Wrote notes.txt");

    // An empty echo area gets the spinner back.
    session.ed = session.ed.clear_message();
    session.tick();
    assert!(session.ed.message().starts_with("[Running... "));

    drop(tx);
}

#[test]
fn background_shell_disconnect_clears_active_shell_and_reports_error() {
    let mut session = make_session();
    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    session.active_shell = Some(ActiveShell::new(rx));
    session.ed = session.ed.set_message("[Running... |]");

    // Simulate worker thread panic / abrupt channel drop without sending
    drop(tx);

    session.tick();
    assert!(session.active_shell.is_none());
    assert_eq!(
        session.ed.message(),
        "Shell command terminated unexpectedly"
    );
}

#[test]
fn busy_rejection_attaches_notice_and_completion_delivers_multiline_output() {
    let mut session = make_session();
    let (tx, rx) = mpsc::channel();
    session.active_shell = Some(ActiveShell::new(rx));

    // Attempt to run a second shell command while first is active: M-x shell-command RET echo 2 RET
    session.handle_event(&press(KeyCode::Char('x'), KeyModifiers::ALT));
    for c in "shell-command".chars() {
        session.handle_event(&press(KeyCode::Char(c), KeyModifiers::NONE));
    }
    session.handle_event(&press(KeyCode::Enter, KeyModifiers::NONE));
    for c in "echo 2".chars() {
        session.handle_event(&press(KeyCode::Char(c), KeyModifiers::NONE));
    }
    session.handle_event(&press(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        session.ed.message(),
        "[Running...] A shell command is already running"
    );

    // Next tick keeps the AlreadyRunning notice while advancing the spinner
    session.tick();
    assert!(
        session
            .ed
            .message()
            .ends_with("] A shell command is already running")
    );

    // Complete the first task with multiline output -> creates *Shell Command Output* buffer
    tx.send(Ok("line1\nline2\n".to_owned())).unwrap();
    session.tick();
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
            glyph: "".into(),
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

    // Multi-line content: row 1 must also be cached on second render (catches row * cols -> row / cols)
    session.ed = session
        .ed
        .with_current_buffer(|b| b.with_text(Doc::from_bytes(b"let x = 42;\nsecond line")));
    let mut out_ml1 = Vec::new();
    session.render_sized(&mut out_ml1, 20, 4).unwrap();
    let s_ml1 = String::from_utf8(out_ml1).unwrap();
    assert!(s_ml1.contains("second line"));

    let mut out_ml2 = Vec::new();
    session.render_sized(&mut out_ml2, 20, 4).unwrap();
    let s_ml2 = String::from_utf8(out_ml2).unwrap();
    assert!(!s_ml2.contains("second line"));

    // Height-only change (20x4 -> 20x3) with identical top rows still invalidates cache (catches != -> ==)
    let mut out_height = Vec::new();
    session.render_sized(&mut out_height, 20, 3).unwrap();
    let s_height = String::from_utf8(out_height).unwrap();
    assert!(s_height.contains("let x = 42;"));

    session.ed = session
        .ed
        .with_current_buffer(|b| b.with_text(Doc::from_bytes(b"let x = 42;")));
    let mut out_reset = Vec::new();
    session.render_sized(&mut out_reset, 20, 3).unwrap();

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

#[test]
fn suspend_key_and_full_redraw_behavior() {
    let mut session = make_session();
    let mut out = Vec::new();
    session.render_sized(&mut out, 20, 4).unwrap();
    assert!(!session.prev_rows.is_empty());

    session.prev_rows.clear();
    out.clear();
    session.render_sized(&mut out, 20, 4).unwrap();
    assert!(
        !out.is_empty(),
        "clearing prev_rows must trigger a full redraw"
    );

    session.handle_event(&press(KeyCode::Char('z'), KeyModifiers::CONTROL));
    #[cfg(unix)]
    assert!(session.pending_suspend);
    #[cfg(not(unix))]
    assert_eq!(session.ed.message(), "Suspend is only supported on Unix");
}

#[test]
fn idle_backup_enqueues_on_change_and_clears_when_saved_or_killed() {
    use crate::core::Eol;
    use crate::shell::recovery::parse_recovery_file;
    use std::time::Duration;

    let rec_dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let file_path = work_dir.path().join("note.txt");
    std::fs::write(&file_path, b"init\n").unwrap();

    let lua = lua::new_state().unwrap();
    let ed = lua::bootstrap(Editor::new(Config::default()), &lua);
    let mut session = ShellSession::with_recovery_dir(ed, lua, Some(rec_dir.path().to_path_buf()));
    session.idle_backup_delay = Duration::from_secs(60);

    // Open file and modify it
    let path_str = file_path.to_string_lossy().into_owned();
    session.ed = session.ed.with_current_buffer(|b| {
        b.set_name("note.txt")
            .set_backing(crate::core::Backing::File {
                path: crate::core::FilePath::resolve("", &path_str),
                disk: crate::core::DiskState::Unknown,
            })
            .with_text(Doc::from_bytes(b"init\n"))
    });
    session.handle_event(&press(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(session.ed.buffer().modified());

    // Before idle_backup_delay elapses, tick must not enqueue backup
    session.tick();
    assert!(session.tracked_backups.is_empty());

    // After idle_backup_delay elapses, tick enqueues backup
    session.idle_backup_delay = Duration::ZERO;
    session.tick();
    assert_eq!(session.tracked_backups.len(), 1);

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut rec_file = None;
    while std::time::Instant::now() < deadline {
        if let Some(entry) = std::fs::read_dir(rec_dir.path())
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().ends_with(".rec"))
        {
            rec_file = Some(entry.path());
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let rec_file = rec_file.expect("recovery file should be written");
    let raw = std::fs::read(&rec_file).unwrap();
    let (meta, _) = parse_recovery_file(&raw).unwrap();
    assert_eq!(meta.eol, Eol::Lf);

    // Changing EOL alone must enqueue a fresh backup
    session.ed = session.ed.with_current_buffer(|b| b.set_eol(Eol::Crlf));
    session.tick();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if let Ok(bytes) = std::fs::read(&rec_file)
            && let Ok((m, _)) = parse_recovery_file(&bytes)
            && m.eol == Eol::Crlf
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let (meta2, _) = parse_recovery_file(&std::fs::read(&rec_file).unwrap()).unwrap();
    assert_eq!(meta2.eol, Eol::Crlf);

    // Marking buffer clean (as after save) removes tracked backup and deletes .rec file
    session.ed = session.ed.with_current_buffer(|b| b.mark_clean());
    session.sync_clean_and_killed_buffers();
    assert!(session.tracked_backups.is_empty());

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if !rec_file.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!rec_file.exists());
}
