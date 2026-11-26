//! Characterization of `Editor` state through public accessors (plan editor-state-boundary, C-01..C-07).
//! The assertions read only the facade, so they keep holding while the fields are regrouped.

use neonemacs::core::Backing;
use neonemacs::core::BufferId;
use neonemacs::core::Config;
use neonemacs::core::Doc;
use neonemacs::core::{Code, Key};
use neonemacs::core::{Confirm, Editor, LastCommand, Mode, Prefix, Window};
use neonemacs::test_support::{editor, keys, press, run_command, text, typ};

#[derive(Debug, PartialEq)]
struct Snapshot {
    buffers: Vec<(BufferId, neonemacs::core::Doc, bool)>,
    windows: Vec<Window>,
    current: Window,
    mode: Mode,
    message: String,
    kill_ring: Vec<neonemacs::core::Doc>,
    last: LastCommand,
    prev: LastCommand,
    quit: bool,
    config: Config,
}

fn snapshot(ed: &Editor) -> Snapshot {
    Snapshot {
        buffers: ed
            .buffers()
            .iter()
            .map(|b| (b.id(), b.text().clone(), b.modified()))
            .collect(),
        windows: ed.windows().windows().into_iter().copied().collect(),
        current: *ed.window(),
        mode: ed.mode().clone(),
        message: ed.message().to_owned(),
        kill_ring: ed.kill_ring().entries().to_vec(),
        last: ed.last(),
        prev: ed.prev(),
        quit: ed.quit(),
        config: ed.config().clone(),
    }
}

const fn normal() -> Mode {
    Mode::Normal {
        prefix: Prefix::None,
    }
}

#[test]
fn mixed_session_snapshot() {
    // C-01: typing, a prefix that quits, undo, and typing again.
    let ed = typ(editor(), "hello world");
    let s = snapshot(&ed);
    assert_eq!(text(&ed), "hello world");
    assert_eq!(s.current.point, 11);
    assert_eq!(s.mode, normal());

    let ed = keys(ed, "C-x C-g");
    let s = snapshot(&ed);
    assert_eq!(s.mode, normal());
    assert_eq!(s.message, "Quit");

    let ed = keys(ed, "C-/");
    assert_eq!(text(&ed), "");

    let ed = typ(ed, "ab");
    let s = snapshot(&ed);
    assert_eq!(text(&ed), "ab");
    assert_eq!(s.current.point, 2);
    assert!(!s.quit);
}

#[test]
fn kill_chain_across_commands() {
    // C-02: consecutive kills share one entry; a command between them starts a new one.
    let ed = typ(editor(), "aaa bbb ccc").move_point(0);

    let ed = run_command(ed, |e, fx| (e.kill_range(0, 4), fx));
    let ed = run_command(ed, |e, fx| (e.kill_range(0, 4), fx));
    assert_eq!(
        snapshot(&ed).kill_ring,
        vec![neonemacs::core::Doc::from_bytes(b"aaa bbb ")]
    );
    assert_eq!(text(&ed), "ccc");

    let ed = run_command(ed, |e, fx| (e.self_insert('x'), fx));
    assert_eq!(text(&ed), "xccc");

    let ed = run_command(ed, |e, fx| (e.kill_range(1, 4), fx));
    let kill_ring = snapshot(&ed).kill_ring;
    assert_eq!(kill_ring.len(), 2);
    assert_eq!(
        kill_ring.last(),
        Some(&neonemacs::core::Doc::from_bytes(b"ccc"))
    );
    assert_eq!(text(&ed), "x");
}

#[test]
fn kill_retargets_windows() {
    // C-04: a window showing a killed buffer moves to another; killing the last buffer leaves *scratch*.
    let (ed, b2) = editor().add_buffer("b2", Doc::from_bytes(b"x"), Backing::None);
    let ed = ed.with_window(|w| Window { buffer: b2, ..w });
    let ed = ed.kill_buffer_id(b2);
    assert_ne!(ed.window().buffer, b2);
    assert_eq!(ed.buffers().len(), 1);

    let id = ed.window().buffer;
    let ed = ed.kill_buffer_id(id);
    assert_eq!(ed.buffers().len(), 1);
    assert_eq!(ed.buffer().name(), "*scratch*");
    assert_eq!(text(&ed), "");
}

#[test]
fn settings_round_trip() {
    // C-05: settings and command registration change only their own state.
    let ed = editor();
    let before = snapshot(&ed);
    let config = Config {
        fill_column: 99,
        ..Config::default()
    };
    let ed = ed.set_config(config.clone()).register_command("my-cmd");
    assert_eq!(ed.config().fill_column, 99);
    assert!(ed.commands().contains("my-cmd"));
    assert_eq!(snapshot(&ed), Snapshot { config, ..before });
}

#[test]
fn confirm_round_trip() {
    // C-07: `n` and C-g cancel and keep the buffer; `y` confirms.
    let ed = typ(editor(), "x");
    let id = ed.window().buffer;

    let ed = ed.confirm(Confirm::KillModified(id), "Kill modified buffer? (y or n)");
    assert!(matches!(snapshot(&ed).mode, Mode::Confirm { .. }));
    let ed = press(ed, Key::plain(Code::Char('n')));
    assert_eq!(snapshot(&ed).mode, normal());
    assert_eq!(snapshot(&ed).message, "Quit");
    assert_eq!(text(&ed), "x");

    let ed = ed.confirm(Confirm::KillModified(id), "Kill modified buffer? (y or n)");
    let ed = press(ed, Key::ctrl('g'));
    assert_eq!(snapshot(&ed).mode, normal());
    assert_eq!(text(&ed), "x");

    let ed = ed.confirm(Confirm::KillModified(id), "Kill modified buffer? (y or n)");
    let ed = press(ed, Key::plain(Code::Char('y')));
    assert_eq!(snapshot(&ed).mode, normal());
    assert_eq!(text(&ed), "", "`y` kills the buffer, leaving *scratch*");
}
