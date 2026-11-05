//! Key interpretation and command dispatching (§3, §4).
//! Decouples routing and command execution lifecycle from Editor state management.

use crate::core::commands;
use crate::core::editor::{Editor, Mode, Prefix, Question};
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

/// The answer character a key means: case folds, and the Korean two-set jamo on the keys
/// `y`, `n`, `q` count as those letters. Used only where a modal question is answered (I-46).
#[must_use]
pub fn answer_char(key: Key) -> Option<char> {
    Some(match key.text()? {
        'ㅛ' => 'y',
        'ㅜ' => 'n',
        'ㅂ' => 'q',
        c => c.to_ascii_lowercase(),
    })
}

/// Answers a yes-or-no question. `y` confirms; `n`, `C-g`, and `Esc` cancel. Any other key
/// leaves the question open and repeats it (I-46).
#[must_use]
pub fn answer(mut ed: Editor, question: Question, key: Key, fx: Effects) -> (Editor, Effects) {
    let choice = answer_char(key);
    if choice == Some('y') {
        ed = ed.set_mode(Mode::normal());
        return commands::confirmed(ed, question.kind, fx);
    }
    ed = if choice == Some('n') || key == Key::ctrl('g') || key == Key::plain(Code::Esc) {
        ed.set_mode(Mode::normal()).set_message("Quit")
    } else {
        ed.set_message(format!("Please answer y or n. {}", question.text))
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::editor::Confirm;
    use crate::test_support::{editor, press, text, typ};

    fn asking() -> Editor {
        let ed = typ(editor(), "x");
        let id = ed.window().buffer;
        ed.confirm(Confirm::KillModified(id), "Kill? (y or n)")
    }

    #[test]
    fn answer_folds_case_and_korean_jamo() {
        for c in ['y', 'Y', 'ㅛ'] {
            let ed = press(asking(), Key::plain(Code::Char(c)));
            assert_eq!(ed.mode(), &Mode::normal(), "{c}");
            assert_eq!(text(&ed), "", "{c} confirms: the modified buffer is killed");
        }
        for c in ['n', 'N', 'ㅜ'] {
            let ed = press(asking(), Key::plain(Code::Char(c)));
            assert_eq!(ed.mode(), &Mode::normal(), "{c}");
            assert_eq!(text(&ed), "x", "{c} cancels");
            assert_eq!(ed.message(), "Quit");
        }
    }

    #[test]
    fn answer_cancels_on_esc_and_ctrl_g_with_one_message() {
        for key in [Key::plain(Code::Esc), Key::ctrl('g')] {
            let ed = press(asking(), key);
            assert_eq!(ed.mode(), &Mode::normal());
            assert_eq!(text(&ed), "x");
            assert_eq!(ed.message(), "Quit");
        }
    }

    #[test]
    fn other_keys_keep_the_question_and_say_so() {
        for key in [Key::plain(Code::Enter), Key::plain(Code::Char('x'))] {
            let ed = press(asking(), key);
            assert!(matches!(ed.mode(), Mode::Confirm(_)));
            assert_eq!(ed.message(), "Please answer y or n. Kill? (y or n)");
            // A second wrong key does not stack the hint.
            let ed = press(ed, Key::plain(Code::Char('z')));
            assert_eq!(ed.message(), "Please answer y or n. Kill? (y or n)");
        }
    }
}
