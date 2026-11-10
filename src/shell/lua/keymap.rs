//! Lua `bind` and `unbind`: keymap changes made from Lua.

use super::mutate_editor;
use crate::core::Editor;
use crate::core::{KeyError, parse_key_seq};
use mlua::{Scope, Table};
use std::cell::RefCell;

pub(super) fn install_keymap_api<'lua, 'scope>(
    scope: &'lua Scope<'lua, 'scope>,
    g: &Table,
    cell: &'scope RefCell<Option<Editor>>,
) -> mlua::Result<()> {
    g.set(
        "bind",
        scope.create_function(|_, (keys, name): (String, String)| {
            let seq =
                parse_key_seq(&keys).map_err(|KeyError(msg)| mlua::Error::RuntimeError(msg))?;
            mutate_editor(cell, |ed| ed.with_keymap(|km| km.bind(seq, &name)));
            Ok(())
        })?,
    )?;
    g.set(
        "unbind",
        scope.create_function(|_, keys: String| {
            let seq =
                parse_key_seq(&keys).map_err(|KeyError(msg)| mlua::Error::RuntimeError(msg))?;
            mutate_editor(cell, |ed| ed.with_keymap(|km| km.unbind(&seq)));
            Ok(())
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{BUNDLED_KEYMAP, COMMANDS, bootstrap, new_state, test_script as script};
    use crate::core::Config;
    use crate::core::Editor;
    use crate::core::is_builtin;
    use crate::core::parse_keys;
    use mlua::{Function, Table};

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

        // Invalid key syntax produces error
        let (_, result) = script(&lua, ed, r#"unbind("C-left")"#);
        assert!(result.is_err());
    }

    #[test]
    fn bundled_keymap_loaded_by_bootstrap() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let keymap = ed.keymap();

        // Check extended bindings defined in keymap.lua
        let k_c_k = crate::core::parse_key_seq("C-k").unwrap();
        assert_eq!(keymap.lookup(&k_c_k), Some("kill-line"));

        let k_split = crate::core::parse_key_seq("C-x 2").unwrap();
        assert_eq!(keymap.lookup(&k_split), Some("split-window-below"));

        let k_other = crate::core::parse_key_seq("C-x o").unwrap();
        assert_eq!(keymap.lookup(&k_other), Some("other-window"));

        let k_switch = crate::core::parse_key_seq("C-x b").unwrap();
        assert_eq!(keymap.lookup(&k_switch), Some("switch-to-buffer"));

        let k_kill = crate::core::parse_key_seq("C-x k").unwrap();
        assert_eq!(keymap.lookup(&k_kill), Some("kill-buffer"));

        let k_fill_para = crate::core::parse_key_seq("M-q").unwrap();
        assert_eq!(keymap.lookup(&k_fill_para), Some("fill-paragraph"));

        let k_goto = crate::core::parse_key_seq("M-g").unwrap();
        assert_eq!(keymap.lookup(&k_goto), Some("goto-line"));
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
}
