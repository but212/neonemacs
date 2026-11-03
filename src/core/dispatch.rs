//! Key interpretation and command dispatching (§3, §4).
//! Decouples routing and command execution lifecycle from Editor state management.

use crate::core::commands;
use crate::core::editor::{Confirm, Editor, Mode, Prefix};
use crate::core::effect::{Effect, Effects};
use crate::core::keymap::{Code, Key};

/// Handles one key press: resolves prefixes, then runs a bound command or inserts text.
#[must_use]
pub fn handle_key(ed: Editor, key: Key, fx: Effects) -> (Editor, Effects) {
    match ed.mode() {
        Mode::Normal { prefix } => {
            let prefix = *prefix;
            handle_normal_key(ed, prefix, key, fx)
        }
        Mode::Confirm(question) => {
            let question = question.clone();
            answer(ed, question, key, fx)
        }
        Mode::Isearch(_) => commands::isearch_key(ed, key, fx),
        Mode::QueryReplace(_) => (commands::query_key(ed, key), fx),
        Mode::Prompt(_) => commands::prompt_key(ed, key, fx),
    }
}

/// Answers a yes-or-no question. `y` confirms, `n` and `C-g` cancel, other keys wait.
#[must_use]
pub fn answer(mut ed: Editor, question: Confirm, key: Key, fx: Effects) -> (Editor, Effects) {
    let yes = key.text() == Some('y');
    if yes || key.text() == Some('n') || key == Key::ctrl('g') {
        ed = ed.set_mode(Mode::normal());
        if yes {
            return commands::confirmed(ed, question, fx);
        }
        ed = ed.set_message("Cancelled");
    }
    (ed, fx)
}

/// Resolves prefix keys, looks up bindings, and either dispatches a command or self-inserts.
pub fn handle_normal_key(
    mut ed: Editor,
    prefix: Prefix,
    key: Key,
    fx: Effects,
) -> (Editor, Effects) {
    ed = ed.set_mode(Mode::normal());
    if key == Key::ctrl('g') {
        ed = ed.set_message("Quit");
        return (ed, fx);
    }
    let keys = match prefix {
        Prefix::None => {
            if key == Key::plain(Code::Esc) {
                ed = ed.set_mode(Mode::Normal {
                    prefix: Prefix::Meta,
                });
                return (ed, fx);
            }
            if key == Key::ctrl('x') {
                ed = ed.set_mode(Mode::Normal {
                    prefix: Prefix::CtlX,
                });
                return (ed, fx);
            }
            vec![key]
        }
        Prefix::CtlX => vec![Key::ctrl('x'), key],
        Prefix::Meta => vec![key.with_meta()],
    };
    if let Some(name) = ed.keymap().lookup(&keys).map(str::to_owned) {
        run_named(ed, &name, fx)
    } else if let (Prefix::None, Some(c)) = (prefix, key.text()) {
        command(ed, fx, |e, fx| (e.self_insert(c), fx))
    } else {
        let seq: Vec<String> = keys.iter().map(|k| k.describe()).collect();
        ed = ed.set_message(format!("{} is undefined", seq.join(" ")));
        (ed, fx)
    }
}

/// Runs the command `name`. A built-in runs now. Any other name is sent to Lua as `RunLua`;
/// its result comes back as `LuaDone`.
#[must_use]
pub fn run_named(ed: Editor, name: &str, mut fx: Effects) -> (Editor, Effects) {
    let ed = if matches!(ed.last(), crate::core::editor::LastCommand::Insert { .. }) {
        ed.boundary()
    } else {
        ed
    };
    if let Some(run) = commands::find(name) {
        command(ed, fx, run)
    } else {
        let ed = ed.begin_command();
        fx.push(Effect::RunLua {
            name: name.to_owned(),
        });
        (ed, fx)
    }
}

/// Runs one command. The undo group ends after every command that is not an insert.
pub fn command(
    ed: Editor,
    fx: Effects,
    run: impl FnOnce(Editor, Effects) -> (Editor, Effects),
) -> (Editor, Effects) {
    let (ed, fx) = run(ed.begin_command(), fx);
    (ed.end_command(), fx)
}
