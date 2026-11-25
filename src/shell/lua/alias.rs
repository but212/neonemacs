//! Lua `key_alias` and `key_unalias`: which characters stand for an ASCII key (I-46, I-48).

use super::mutate_editor;
use crate::core::{AliasTarget, Editor, parse_single_char};
use mlua::{Scope, Table};
use std::cell::RefCell;

/// The one character in `s`.
fn single_char(s: &str) -> mlua::Result<char> {
    parse_single_char(s)
        .ok_or_else(|| mlua::Error::RuntimeError(format!("key alias must be one character: '{s}'")))
}

pub(super) fn install_alias_api<'lua, 'scope>(
    scope: &'lua Scope<'lua, 'scope>,
    g: &Table,
    cell: &'scope RefCell<Option<Editor>>,
) -> mlua::Result<()> {
    g.set(
        "key_alias",
        scope.create_function(|_, (from, to): (String, String)| {
            let from = single_char(&from)?;
            let to = AliasTarget::parse(&to).map_err(mlua::Error::RuntimeError)?;
            mutate_editor(cell, |ed| ed.with_key_aliases(|a| a.alias(from, to)));
            Ok(())
        })?,
    )?;
    g.set(
        "key_unalias",
        scope.create_function(|_, from: String| {
            let from = single_char(&from)?;
            mutate_editor(cell, |ed| ed.with_key_aliases(|a| a.unalias(from)));
            Ok(())
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{bootstrap, new_state, test_script as script};
    use crate::core::Config;
    use crate::core::Editor;
    use crate::core::{Code, Key};

    #[test]
    fn bundled_keymap_aliases_the_whole_two_set_layout() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let a = ed.key_aliases();
        let table = [
            ("ㅂㅈㄷㄱㅅㅛㅕㅑㅐㅔ", "qwertyuiop"),
            ("ㅁㄴㅇㄹㅎㅗㅓㅏㅣ", "asdfghjkl"),
            ("ㅋㅌㅊㅍㅠㅜㅡ", "zxcvbnm"),
            ("ㅃㅉㄸㄲㅆㅒㅖ", "QWERTOP"),
        ];
        let mut count = 0;
        for (jamo, ascii) in table {
            assert_eq!(jamo.chars().count(), ascii.len());
            for (j, c) in jamo.chars().zip(ascii.chars()) {
                assert_eq!(a.get(j), Some(c), "{j}");
                count += 1;
            }
        }
        assert_eq!(count, 33);
    }

    #[test]
    fn bootstrapped_editor_runs_ctl_x_and_meta_commands_with_jamo() {
        use crate::core::Mode;
        use crate::test_support::press;
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let ed = press(press(ed, Key::ctrl('x')), Key::plain(Code::Char('ㅠ')));
        assert!(matches!(ed.mode(), Mode::Prompt(_)), "C-x ㅠ ran C-x b");
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let ed = press(ed, Key::plain(Code::Char('ㅌ')).with_meta());
        assert!(matches!(ed.mode(), Mode::Prompt(_)), "Alt+ㅌ ran M-x");
    }

    #[test]
    fn answer_alias_is_gone() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let (_, r) = script(&lua, ed, r#"answer_alias("н", "y")"#);
        assert!(r.is_err());
    }

    #[test]
    fn bootstrapped_editor_answers_a_question_with_jamo() {
        use crate::core::{Confirm, Mode};
        use crate::test_support::{press, text, typ};
        let lua = new_state().unwrap();
        let ed = typ(bootstrap(Editor::new(Config::default()), &lua), "x");
        let id = ed.window().buffer;
        let ed = ed.confirm(Confirm::KillModified(id), "Kill? (y or n)");
        let ed = press(ed, Key::plain(Code::Char('ㅛ')));
        assert_eq!(ed.mode(), &Mode::normal());
        assert_eq!(text(&ed), "", "ㅛ confirms after bootstrap");
    }

    #[test]
    fn key_alias_registers_replaces_and_unaliases() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let (ed, r) = script(
            &lua,
            ed,
            r#"key_alias("н", "y"); key_alias("ㅛ", "!"); key_unalias("ㅂ"); key_unalias("z")"#,
        );
        r.unwrap();
        let a = ed.key_aliases();
        assert_eq!(a.get('н'), Some('y'));
        assert_eq!(a.get('ㅛ'), Some('!'), "re-alias replaces");
        assert_eq!(a.get('ㅂ'), None, "default alias removed");
        assert_eq!(a.get('ㅜ'), Some('n'), "others untouched");
    }

    #[test]
    fn key_alias_rejects_bad_input_and_changes_nothing() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let before = ed.key_aliases().clone();
        for src in [
            r#"key_alias("ab", "y")"#,
            r#"key_alias("", "y")"#,
            r#"key_alias("н", "")"#,
            r#"key_alias("н", " ")"#,
            r#"key_alias("н", "ㅛ")"#,
            r#"key_alias("н", "yy")"#,
            r#"key_unalias("ab")"#,
            r#"key_unalias("")"#,
        ] {
            let (next, r) = script(&lua, ed.clone(), src);
            assert!(r.is_err(), "{src}");
            assert_eq!(next.key_aliases(), &before, "{src}");
        }
    }
}
