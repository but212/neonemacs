//! C-02, C-03, I-24: Async shell task queue execution without blocking.

use neonemacs::core::config::Config;
use neonemacs::core::editor::Editor;
use neonemacs::core::effect::Event;
use neonemacs::core::keymap::parse_keys;
use neonemacs::shell::{handle_interactive, spawn_shell};
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
    let (mut ed, _) = handle_interactive(ed, None, Event::Key(mx));

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
        );
        ed = e;
    }

    // Press Enter to execute
    let (final_ed, task) = handle_interactive(
        ed,
        None,
        Event::Key(neonemacs::core::keymap::Key::plain(
            neonemacs::core::keymap::Code::Enter,
        )),
    );

    // Must return a background ShellTask and set message to [Running...]
    assert!(task.is_some(), "Expected async task spawned");
    assert_eq!(final_ed.message, "[Running...]");

    let result = task
        .unwrap()
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("command completes");
    assert!(result.unwrap().contains("non_blocking"));
}
