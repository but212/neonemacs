//! C-02, C-03, I-24: Async shell task queue execution without blocking.

use neonemacs::core::config::Config;
use neonemacs::core::editor::Editor;
use neonemacs::core::effect::Event;
use neonemacs::core::keymap::parse_keys;
use neonemacs::shell::session::{ActiveShell, ShellNotice};
use neonemacs::shell::{ShellOutcome, handle_interactive, spawn_shell};
use std::time::Duration;

#[test]
fn async_shell_spawns_and_receives_output() {
    let task = spawn_shell("echo async_test_ok".to_owned());
    // Wait for the background thread to finish
    let res = task
        .receiver
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
            neonemacs::core::keymap::Code::Enter
        } else {
            neonemacs::core::keymap::Code::Char(c)
        };
        let (e, _) = handle_interactive(
            ed,
            None,
            Event::Key(neonemacs::core::keymap::Key::plain(code)),
            false,
        );
        ed = e;
    }

    for c in "echo non_blocking".chars() {
        let (e, _) = handle_interactive(
            ed,
            None,
            Event::Key(neonemacs::core::keymap::Key::plain(
                neonemacs::core::keymap::Code::Char(c),
            )),
            false,
        );
        ed = e;
    }

    // Press Enter to execute
    let (final_ed, outcome) = handle_interactive(
        ed,
        None,
        Event::Key(neonemacs::core::keymap::Key::plain(
            neonemacs::core::keymap::Code::Enter,
        )),
        false,
    );

    let task = match outcome {
        ShellOutcome::Spawned(t) => t,
        _ => panic!("Expected ShellOutcome::Spawned"),
    };
    assert_eq!(final_ed.message, "[Running...]");

    let result = task
        .receiver
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
            neonemacs::core::keymap::Code::Enter
        } else {
            neonemacs::core::keymap::Code::Char(c)
        };
        let (e, _) = handle_interactive(
            ed,
            None,
            Event::Key(neonemacs::core::keymap::Key::plain(code)),
            false,
        );
        ed = e;
    }

    for c in "echo blocked".chars() {
        let (e, _) = handle_interactive(
            ed,
            None,
            Event::Key(neonemacs::core::keymap::Key::plain(
                neonemacs::core::keymap::Code::Char(c),
            )),
            false,
        );
        ed = e;
    }

    // Press Enter while shell_busy is TRUE
    let (rejected_ed, outcome) = handle_interactive(
        ed,
        None,
        Event::Key(neonemacs::core::keymap::Key::plain(
            neonemacs::core::keymap::Code::Enter,
        )),
        true,
    );

    // Must NOT spawn a new task, must return BusyRejected, and format message
    assert!(matches!(outcome, ShellOutcome::BusyRejected));
    assert_eq!(
        rejected_ed.message,
        "[Running...] A shell command is already running"
    );
}

#[test]
fn active_shell_tracks_spinner_and_typed_notice() {
    let task = spawn_shell("echo hi".to_owned());
    let mut active = ActiveShell::new(task);

    // Initially plain spinner
    let s1 = active.format_status();
    assert!(s1.starts_with("[Running... /]"));

    // Set notice
    active.set_notice(ShellNotice::AlreadyRunning);
    let s2 = active.format_status();
    assert!(s2.starts_with("[Running... -] A shell command is already running"));
}
