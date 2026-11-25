//! Key interpretation and command dispatching (§3, §4).
//! Decouples routing and command execution lifecycle from Editor state management.

use crate::commands;
use crate::editor::{Buffer, CANCEL_MESSAGE, Editor, Mode, Prefix, Question};
use crate::effect::{Effect, Effects};
use crate::keymap::{Code, Key, KeyAliases};
use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

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

/// The answer character a key means: an alias wins, else the character itself, then ASCII case
/// folds. Used only where a modal question is answered, never for self-insert (I-46, I-48).
#[must_use]
pub fn answer_char(aliases: &KeyAliases, key: Key) -> Option<char> {
    let c = key.text()?;
    let folded = {
        let mut lower = c.to_lowercase();
        lower.next().filter(|_| lower.next().is_none())
    };
    Some(
        aliases
            .get(c)
            .or_else(|| folded.and_then(|l| aliases.get(l)))
            .unwrap_or(c)
            .to_ascii_lowercase(),
    )
}

/// Answers a yes-or-no question. `y` confirms; `n`, `C-g`, and `Esc` cancel. Any other key
/// leaves the question open and repeats it (I-46).
#[must_use]
pub fn answer(mut ed: Editor, question: Question, key: Key, fx: Effects) -> (Editor, Effects) {
    let choice = answer_char(ed.key_aliases(), key);
    if choice == Some('y') {
        ed = ed.set_mode(Mode::normal());
        return commands::confirmed(ed, question.kind, fx);
    }
    ed = if choice == Some('n') || key == Key::ctrl('g') || key == Key::plain(Code::Esc) {
        ed.set_mode(Mode::normal()).set_message(CANCEL_MESSAGE)
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
        ed = ed
            .with_current_buffer(Buffer::deactivate_mark)
            .set_message(CANCEL_MESSAGE);
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
    if let Some(name) = lookup_binding(&ed, prefix, key, &keys) {
        run_named(ed, &name, fx)
    } else if let (Prefix::None, Some(c)) = (prefix, key.text()) {
        command(ed, fx, |e, fx| (e.self_insert(c), fx))
    } else {
        let seq: Vec<String> = keys.iter().map(|k| k.describe()).collect();
        ed = ed.set_message(format!("{} is undefined", seq.join(" ")));
        (ed, fx)
    }
}

/// The command bound to `keys`, whose last key is `key`. An explicit binding wins. Failing that,
/// a prefixed or modified `key` is looked up again through its alias. Plain text never is (I-48).
fn lookup_binding(ed: &Editor, prefix: Prefix, key: Key, keys: &[Key]) -> Option<String> {
    let keymap = ed.keymap();
    keymap.lookup(keys).map(str::to_owned).or_else(|| {
        if prefix == Prefix::None && key.text().is_some() {
            return None;
        }
        let alias = ed.key_aliases().rewrite(key)?;
        let mut aliased = keys.to_vec();
        *aliased.last_mut()? = if prefix == Prefix::Meta {
            alias.with_meta()
        } else {
            alias
        };
        keymap.lookup(&aliased).map(str::to_owned)
    })
}

/// Runs the command `name`. A built-in runs now. Any other name is sent to Lua as `RunLua`;
/// its result comes back as `LuaDone`.
#[must_use]
pub fn run_named(ed: Editor, name: &str, mut fx: Effects) -> (Editor, Effects) {
    let ed = if matches!(ed.last(), crate::editor::LastCommand::Insert { .. }) {
        ed.boundary()
    } else {
        ed
    };
    if let Some(run) = commands::find(name) {
        command(ed, fx, |e, fx| run.run(e, fx))
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
    use crate::editor::Confirm;
    use crate::keymap::AliasTarget;
    use crate::test_support::{editor, press, text, typ};

    fn asking() -> Editor {
        let ed = typ(editor(), "x");
        let id = ed.window().buffer;
        ed.confirm(Confirm::KillModified(id), "Kill? (y or n)")
    }

    fn to(s: &str) -> AliasTarget {
        AliasTarget::parse(s).unwrap()
    }

    fn jamo() -> Editor {
        asking().with_key_aliases(|a| a.alias('ㅛ', to("y")).alias('ㅜ', to("n")))
    }

    fn hangul() -> Editor {
        editor()
            .with_keymap(|k| k.bind_str("C-x b", "switch-to-buffer").unwrap())
            .with_key_aliases(|a| a.alias('ㅠ', to("b")).alias('ㅌ', to("x")))
    }

    fn ch(c: char) -> Key {
        Key::plain(Code::Char(c))
    }

    #[test]
    fn answer_folds_case() {
        for c in ['y', 'Y'] {
            let ed = press(asking(), Key::plain(Code::Char(c)));
            assert_eq!(ed.mode(), &Mode::normal(), "{c}");
            assert_eq!(text(&ed), "", "{c} confirms: the modified buffer is killed");
        }
        for c in ['n', 'N'] {
            let ed = press(asking(), Key::plain(Code::Char(c)));
            assert_eq!(ed.mode(), &Mode::normal(), "{c}");
            assert_eq!(text(&ed), "x", "{c} cancels");
            assert_eq!(ed.message(), "Quit");
        }
    }

    #[test]
    fn answer_uses_aliases_and_core_has_none_by_default() {
        let ed = press(jamo(), Key::plain(Code::Char('ㅛ')));
        assert_eq!(text(&ed), "", "aliased ㅛ confirms");
        let ed = press(jamo(), Key::plain(Code::Char('ㅜ')));
        assert_eq!(text(&ed), "x", "aliased ㅜ cancels");
        assert_eq!(ed.message(), "Quit");
        let ed = press(asking(), Key::plain(Code::Char('ㅛ')));
        assert!(matches!(ed.mode(), Mode::Confirm(_)), "no alias, no answer");
    }

    #[test]
    fn alias_matches_the_uppercase_form() {
        let ed = asking().with_key_aliases(|a| a.alias('н', to("y")));
        let ed = press(ed, Key::plain(Code::Char('Н')));
        assert_eq!(text(&ed), "", "Н confirms through the н alias");
    }

    #[test]
    fn alias_to_an_uppercase_target_still_answers() {
        let ed = asking().with_key_aliases(|a| a.alias('ㅛ', to("Y")));
        let ed = press(ed, ch('ㅛ'));
        assert_eq!(text(&ed), "", "the answer folds to y");

        let ed = asking().with_key_aliases(|a| a.alias('ㅃ', to("Q")));
        let ed = press(ed, ch('ㅃ'));
        assert_eq!(
            ed.message(),
            "Please answer y or n. Kill? (y or n)",
            "q is no yes/no answer"
        );
    }

    #[test]
    fn alias_does_not_change_self_insert() {
        let ed = editor().with_key_aliases(|a| a.alias('ㅛ', to("y")));
        assert_eq!(text(&typ(ed, "ㅛ")), "ㅛ", "I-48");
    }

    #[test]
    fn ctl_x_prefix_resolves_through_an_alias() {
        let ed = press(press(hangul(), Key::ctrl('x')), ch('ㅠ'));
        assert!(
            matches!(ed.mode(), Mode::Prompt(_)),
            "C-x ㅠ ran switch-to-buffer"
        );
    }

    #[test]
    fn meta_resolves_through_an_alias_after_esc_and_alt() {
        let ed = press(press(hangul(), Key::plain(Code::Esc)), ch('ㅌ'));
        assert!(matches!(ed.mode(), Mode::Prompt(_)), "Esc ㅌ ran M-x");
        let ed = press(hangul(), ch('ㅌ').with_meta());
        assert!(matches!(ed.mode(), Mode::Prompt(_)), "Alt+ㅌ ran M-x");
    }

    #[test]
    fn explicit_binding_wins_over_the_alias() {
        let ed = hangul().with_keymap(|k| k.bind_str("C-x ㅠ", "undo").unwrap());
        let ed = press(press(ed, Key::ctrl('x')), ch('ㅠ'));
        assert_eq!(
            ed.mode(),
            &Mode::normal(),
            "the explicit C-x ㅠ ran, not C-x b"
        );
    }

    #[test]
    fn unbound_alias_reports_the_original_key() {
        let ed = press(press(hangul(), Key::ctrl('x')), ch('ㅊ'));
        assert_eq!(ed.message(), "C-x ㅊ is undefined");
        let ed = hangul().with_key_aliases(|a| a.alias('ㅊ', to("c")));
        let ed = press(press(ed, Key::ctrl('x')), ch('ㅊ'));
        assert_eq!(ed.message(), "C-x ㅊ is undefined", "target unbound too");
    }

    #[test]
    fn alias_is_ignored_for_plain_text_after_a_failed_lookup() {
        let ed = hangul().with_keymap(|k| k.bind_str("b", "undo").unwrap());
        assert_eq!(text(&typ(ed, "ㅠ")), "ㅠ", "I-48: no lookup for plain text");
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
