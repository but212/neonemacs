//! C-02, C-03, I-24: Async shell task queue execution without blocking.

use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
};
use neonemacs::core::Config;
use neonemacs::core::Editor;
use neonemacs::core::ShellSpec;
use neonemacs::core::effect::Event;
use neonemacs::core::parse_keys;
use neonemacs::shell::lua;
use neonemacs::shell::session::{ActiveShell, ShellSession};
use neonemacs::shell::{ShellOutcome, handle_interactive, spawn_shell};
use std::time::{Duration, Instant};

#[test]
fn an_unrunnable_shell_program_reports_an_error() {
    let shell = ShellSpec {
        program: "neonemacs-no-such-shell".to_owned(),
        switch: "-c".to_owned(),
    };
    let task = spawn_shell("echo x".to_owned(), shell);
    let res = task
        .recv_timeout(Duration::from_secs(5))
        .expect("background shell finishes in time");
    let err = res.expect_err("the program does not exist");
    assert!(err.starts_with("Cannot run command"), "{err}");
}

#[test]
fn async_shell_spawns_and_receives_output() {
    let task = spawn_shell("echo async_test_ok".to_owned(), ShellSpec::default());
    // Wait for the background thread to finish
    let res = task
        .recv_timeout(Duration::from_secs(5))
        .expect("background shell finishes in time");
    let output = res.expect("command succeeded");
    assert!(output.contains("async_test_ok"));
}

#[test]
fn handle_interactive_dispatches_async_shell_without_blocking() {
    let ed = Editor::new(Config::default());
    // Type M-x shell-command RET echo hello RET
    let mx = parse_keys("M-x").unwrap()[0];
    let (mut ed, _) = handle_interactive(ed, None, Event::Key(mx), false);

    for c in "shell-command\n".chars() {
        let code = if c == '\n' {
            neonemacs::core::Code::Enter
        } else {
            neonemacs::core::Code::Char(c)
        };
        let (e, _) = handle_interactive(
            ed,
            None,
            Event::Key(neonemacs::core::Key::plain(code)),
            false,
        );
        ed = e;
    }

    for c in "echo non_blocking".chars() {
        let (e, _) = handle_interactive(
            ed,
            None,
            Event::Key(neonemacs::core::Key::plain(neonemacs::core::Code::Char(c))),
            false,
        );
        ed = e;
    }

    // Press Enter to execute
    let (final_ed, outcome) = handle_interactive(
        ed,
        None,
        Event::Key(neonemacs::core::Key::plain(neonemacs::core::Code::Enter)),
        false,
    );

    let ShellOutcome::Spawned(task) = outcome else {
        panic!("Expected ShellOutcome::Spawned")
    };
    assert_eq!(final_ed.message(), "[Running...]");

    let result = task
        .recv_timeout(Duration::from_secs(5))
        .expect("command completes");
    assert!(result.unwrap().contains("non_blocking"));
}

#[test]
fn handle_interactive_rejects_concurrent_shell_command_when_busy() {
    let ed = Editor::new(Config::default());
    let mx = parse_keys("M-x").unwrap()[0];
    let (mut ed, _) = handle_interactive(ed, None, Event::Key(mx), false);

    for c in "shell-command\n".chars() {
        let code = if c == '\n' {
            neonemacs::core::Code::Enter
        } else {
            neonemacs::core::Code::Char(c)
        };
        let (e, _) = handle_interactive(
            ed,
            None,
            Event::Key(neonemacs::core::Key::plain(code)),
            false,
        );
        ed = e;
    }

    for c in "echo blocked".chars() {
        let (e, _) = handle_interactive(
            ed,
            None,
            Event::Key(neonemacs::core::Key::plain(neonemacs::core::Code::Char(c))),
            false,
        );
        ed = e;
    }

    // Press Enter while shell_busy is TRUE
    let (rejected_ed, outcome) = handle_interactive(
        ed,
        None,
        Event::Key(neonemacs::core::Key::plain(neonemacs::core::Code::Enter)),
        true,
    );

    // Must NOT spawn a new task, must return BusyRejected, and format message
    assert!(matches!(outcome, ShellOutcome::BusyRejected));
    assert_eq!(
        rejected_ed.message(),
        "[Running...] A shell command is already running"
    );
}

#[test]
fn active_shell_tracks_spinner_and_typed_notice() {
    let task = spawn_shell("echo hi".to_owned(), ShellSpec::default());
    let mut active = ActiveShell::new(task);

    // Initially plain spinner
    let s1 = active.format_status();
    assert!(s1.starts_with("[Running... /]"));

    // Set notice
    active.set_already_running();
    let s2 = active.format_status();
    assert!(s2.starts_with("[Running... -] A shell command is already running"));
}

#[test]
fn shell_session_end_to_end_async_lifecycle_and_isearch_protection() {
    let ed = Editor::new(Config::default());
    let lua = lua::new_state().unwrap();
    let mut session = ShellSession::new(lua::bootstrap(ed, &lua), lua);
    let t0 = Instant::now();

    let press = |code: KeyCode, modifiers: KeyModifiers| {
        TermEvent::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    };

    // Type buffer content, then launch `M-x shell-command RET echo session_async_ok RET`
    for c in "hello world".chars() {
        session.handle_event_at(&press(KeyCode::Char(c), KeyModifiers::NONE), t0);
    }
    session.handle_event_at(&press(KeyCode::Char('x'), KeyModifiers::ALT), t0);
    for c in "shell-command".chars() {
        session.handle_event_at(&press(KeyCode::Char(c), KeyModifiers::NONE), t0);
    }
    session.handle_event_at(&press(KeyCode::Enter, KeyModifiers::NONE), t0);
    for c in "echo session_async_ok".chars() {
        session.handle_event_at(&press(KeyCode::Char(c), KeyModifiers::NONE), t0);
    }
    session.handle_event_at(&press(KeyCode::Enter, KeyModifiers::NONE), t0);
    assert_eq!(session.ed.message(), "[Running...]");

    // Move to beginning of buffer (`C-a`) and start I-search (`C-s`) while shell is running;
    // tick must not overwrite I-search echo even if the background command completes mid-search.
    session.handle_event_at(&press(KeyCode::Char('a'), KeyModifiers::CONTROL), t0);
    session.handle_event_at(&press(KeyCode::Char('s'), KeyModifiers::CONTROL), t0);
    session.handle_event_at(&press(KeyCode::Char('w'), KeyModifiers::NONE), t0);
    assert_eq!(session.ed.message(), "I-search: w");
    session.tick_at(t0 + Duration::from_millis(50));
    assert_eq!(session.ed.message(), "I-search: w");

    // Exit I-search with Enter -> back to Normal mode
    session.handle_event_at(&press(KeyCode::Enter, KeyModifiers::NONE), t0);

    // Poll tick until background command completes
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        session.tick_at(Instant::now());
        if session.ed.message() == "session_async_ok" {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(session.ed.message(), "session_async_ok");
}
