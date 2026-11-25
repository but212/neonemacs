//! Lua configuration, keymap, and key-alias API: `get`, `set`, `bind`, `unbind`, `key_alias`, `key_unalias`.

use super::{editor_ref, mutate_editor};
use crate::core::{AliasTarget, Editor, Setting, Value, parse_key_seq, parse_single_char};
use mlua::{Lua, Scope, Table, Value as LuaValue};
use std::cell::RefCell;

fn single_char(s: &str) -> mlua::Result<char> {
    parse_single_char(s)
        .ok_or_else(|| mlua::Error::RuntimeError(format!("key alias must be one character: '{s}'")))
}

pub(super) fn install_settings_api<'lua, 'scope>(
    scope: &'lua Scope<'lua, 'scope>,
    g: &Table,
    cell: &'scope RefCell<Option<Editor>>,
) -> mlua::Result<()> {
    g.set(
        "get",
        scope.create_function(|lua, name: String| {
            let setting = Setting::parse(&name).map_err(mlua::Error::RuntimeError)?;
            to_lua(lua, editor_ref(cell).config().get(setting))
        })?,
    )?;
    g.set(
        "set",
        scope.create_function(|_, (name, value): (String, LuaValue)| {
            let setting = Setting::parse(&name).map_err(mlua::Error::RuntimeError)?;
            let value = from_lua(&value).map_err(mlua::Error::RuntimeError)?;
            let new_config = editor_ref(cell)
                .config()
                .set(setting, value)
                .map_err(mlua::Error::RuntimeError)?;
            mutate_editor(cell, |ed| ed.set_config(new_config));
            Ok(())
        })?,
    )?;
    g.set(
        "bind",
        scope.create_function(|_, (keys, name): (String, String)| {
            let seq = parse_key_seq(&keys).map_err(mlua::Error::RuntimeError)?;
            mutate_editor(cell, |ed| ed.with_keymap(|km| km.bind(seq, &name)));
            Ok(())
        })?,
    )?;
    g.set(
        "unbind",
        scope.create_function(|_, keys: String| {
            let seq = parse_key_seq(&keys).map_err(mlua::Error::RuntimeError)?;
            mutate_editor(cell, |ed| ed.with_keymap(|km| km.unbind(&seq)));
            Ok(())
        })?,
    )?;
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

fn to_lua(lua: &Lua, value: Value) -> mlua::Result<LuaValue> {
    Ok(match value {
        Value::Int(n) => LuaValue::Integer(n),
        Value::Bool(b) => LuaValue::Boolean(b),
        Value::Str(s) => LuaValue::String(lua.create_string(&s)?),
    })
}

fn from_lua(value: &LuaValue) -> Result<Value, String> {
    match value {
        LuaValue::Integer(n) => Ok(Value::Int(*n)),
        LuaValue::Boolean(b) => Ok(Value::Bool(*b)),
        LuaValue::String(s) => s
            .to_str()
            .map(|s| Value::Str(s.to_string()))
            .map_err(|_| "setting string must be valid UTF-8".to_owned()),
        other => Err(format!("unsupported setting value: {}", other.type_name())),
    }
}

#[cfg(test)]
mod tests {
    use crate::core::{
        Code, Config, Confirm, Edit, Editor, Eol, Key, Mode, Toggles, is_builtin, parse_keys,
    };
    use crate::shell::lua::{
        BUNDLED_KEYMAP, COMMANDS, bootstrap, new_state, test_script as script,
    };
    use mlua::{Function, Table};

    #[test]
    fn string_settings_cross_the_lua_boundary() {
        let lua = new_state().unwrap();
        let (ed, result) = script(
            &lua,
            Editor::new(Config::default()),
            r#"assert(get("default-eol") == "lf"); set("default-eol", "crlf"); assert(get("default-eol") == "crlf")"#,
        );
        result.unwrap();
        assert_eq!(ed.config().default_eol, Eol::Crlf);
        let (ed, result) = script(&lua, ed, r#"set("default-eol", "cr")"#);
        assert!(result.is_err());
        assert_eq!(ed.config().default_eol, Eol::Crlf);
        let (_, result) = script(&lua, ed, r#"set("default-eol", 1)"#);
        assert!(result.is_err());
    }

    #[test]
    fn api_get_and_set_validate_input() {
        let lua = new_state().unwrap();
        let (ed, result) = script(
            &lua,
            Editor::new(Config::default()),
            r#"assert(get("fill-column") == 70)"#,
        );
        result.unwrap();
        let (ed, result) = script(&lua, ed, r#"set("case-fold-search", false)"#);
        result.unwrap();
        assert!(!ed.config().toggles.contains(Toggles::CASE_FOLD_SEARCH));
        let (ed, result) = script(&lua, ed, r#"set("tab-width", 0)"#);
        assert!(result.is_err());
        let (ed, result) = script(&lua, ed, r#"set("nope", 1)"#);
        assert!(result.is_err());
        let (_, result) = script(&lua, ed, r#"set("fill-column", "x")"#);
        assert!(result.is_err());
    }

    #[test]
    fn undo_limit_applies_to_existing_buffers() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let (ed, result) = script(&lua, ed, r#"set("undo-limit", 0)"#);
        result.unwrap();

        let (ed, _) = ed.apply(Edit::insert(0, b"a".as_slice()));
        let ed = ed.end_command();
        let (ed, _) = ed.apply(Edit::insert(1, b"b".as_slice()));
        let ed = ed.end_command();

        let (ed, _) = ed.begin_command().undo(Vec::new());
        let (ed, _) = ed.begin_command().undo(Vec::new());
        assert_eq!(ed.message(), "No further undo information");
    }

    #[test]
    fn api_bind_and_bad_keys() {
        let lua = new_state().unwrap();
        let (ed, result) = script(
            &lua,
            Editor::new(Config::default()),
            r#"bind("C-s", "isearch-forward-regexp")"#,
        );
        result.unwrap();
        assert_eq!(
            ed.keymap().lookup(&parse_keys("C-s").unwrap()),
            Some("isearch-forward-regexp")
        );
        let (_, result) = script(&lua, ed, r#"bind("C-", "undo")"#);
        assert!(result.is_err());
    }

    #[test]
    fn api_unbind_removes_binding_and_rejects_invalid_keys() {
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default());
        let (ed, result) = script(&lua, ed, r#"bind("C-x 2", "split-window-below")"#);
        result.unwrap();
        let key = crate::core::parse_key_seq("C-x 2").unwrap();
        assert_eq!(ed.keymap().lookup(&key), Some("split-window-below"));

        let (ed, result) = script(&lua, ed, r#"unbind("C-x 2")"#);
        result.unwrap();
        assert_eq!(ed.keymap().lookup(&key), None);

        let (_, result) = script(&lua, ed, r#"unbind("C-left")"#);
        assert!(result.is_err());
    }

    #[test]
    fn bundled_keymap_loaded_by_bootstrap() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let keymap = ed.keymap();

        for (seq, name) in [
            ("C-k", "kill-line"),
            ("C-x 2", "split-window-below"),
            ("C-x o", "other-window"),
            ("C-x b", "switch-to-buffer"),
            ("C-x k", "kill-buffer"),
            ("M-q", "fill-paragraph"),
            ("M-g", "goto-line"),
        ] {
            let k = crate::core::parse_key_seq(seq).unwrap();
            assert_eq!(keymap.lookup(&k), Some(name));
        }
    }

    #[test]
    fn bundled_keymap_commands_all_exist() {
        let lua = new_state().unwrap();
        let _ed = bootstrap(Editor::new(Config::default()), &lua);
        let commands_table: Table = lua.globals().get(COMMANDS).unwrap();

        for line in BUNDLED_KEYMAP.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("bind(") {
                let parts: Vec<&str> = trimmed.split('"').collect();
                if parts.len() >= 4 {
                    let cmd_name = parts[3];
                    let in_builtins = is_builtin(cmd_name);
                    let in_lua = commands_table
                        .get::<Option<Function>>(cmd_name)
                        .unwrap()
                        .is_some();
                    assert!(
                        in_builtins || in_lua,
                        "bundled keymap.lua references non-existent command: '{cmd_name}'"
                    );
                }
            }
        }
    }

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
