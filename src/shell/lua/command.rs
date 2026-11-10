//! Lua `command`, `message`, and `run`: registers Lua commands and runs built-in or Lua commands by name.

use super::{COMMANDS, mutate_editor};
use crate::core::Editor;
use crate::core::effect::Event;
use crate::core::is_builtin;
use crate::shell::{perform_in, step_in};
use mlua::{Function, Lua, Scope, Table};
use std::cell::{Cell, RefCell};

/// Most nested `run` calls allowed. A command that runs itself stops with an error here, before the native stack runs out.
const MAX_RUN_DEPTH: usize = 50;

pub(super) fn install_command_api<'lua, 'scope>(
    scope: &'lua Scope<'lua, 'scope>,
    g: &Table,
    cell: &'scope RefCell<Option<Editor>>,
    depth: &'scope Cell<usize>,
) -> mlua::Result<()> {
    g.set(
        "command",
        scope.create_function(|lua, (name, f): (String, Function)| {
            if is_builtin(&name) {
                return Err(mlua::Error::RuntimeError(format!(
                    "{name} is a built-in command"
                )));
            }
            lua.globals().get::<Table>(COMMANDS)?.set(name.clone(), f)?;
            mutate_editor(cell, |ed| ed.register_command(&name));
            Ok(())
        })?,
    )?;
    g.set(
        "message",
        scope.create_function(|_, msg: String| {
            mutate_editor(cell, |ed| ed.set_message(msg));
            Ok(())
        })?,
    )?;
    g.set(
        "run",
        scope.create_function(|lua, name: String| {
            if depth.get() >= MAX_RUN_DEPTH {
                return Err(mlua::Error::RuntimeError(format!(
                    "{name}: run nested too deeply"
                )));
            }
            depth.set(depth.get() + 1);
            let result = if is_builtin(&name) {
                let effects = step_in(cell, Event::Run(name));
                perform_in(cell, Some(lua), effects);
                Ok(())
            } else {
                call_lua(lua, &name)
            };
            depth.set(depth.get() - 1);
            result
        })?,
    )?;
    Ok(())
}

/// Calls the Lua command `name`, which must have been defined with `command`.
pub fn call_lua(lua: &Lua, name: &str) -> mlua::Result<()> {
    let table: Table = lua.globals().get(COMMANDS)?;
    table.get::<Option<Function>>(name)?.map_or_else(
        || {
            Err(mlua::Error::RuntimeError(format!(
                "No command named {name}"
            )))
        },
        |f| f.call(()),
    )
}

#[cfg(test)]
mod tests {
    use super::super::{bootstrap, new_state, test_script as script};
    use crate::core::Config;
    use crate::core::Editor;
    use crate::core::effect::Event;
    use crate::core::request_open;
    use crate::core::{Code, Key};
    use crate::shell::{handle, perform_all};
    use std::fs;

    #[test]
    fn api_command_and_run() {
        let lua = new_state().unwrap();
        let (ed, result) = script(
            &lua,
            Editor::new(Config::default()),
            r#"command("wide", function() set("fill-column", 33); run("undo") end)"#,
        );
        result.unwrap();
        let ed = handle(ed, Some(&lua), Event::Run("wide".to_owned()));
        assert_eq!(ed.config().fill_column, 33);
        let (ed, result) = script(&lua, ed, r#"command("undo", function() end)"#);
        assert!(result.is_err());
        let (_, result) = script(&lua, ed, r#"run("nope")"#);
        assert!(result.is_err());
    }

    #[test]
    fn run_from_lua_can_use_get_and_set() {
        let lua = new_state().unwrap();
        let (ed, result) = script(
            &lua,
            Editor::new(Config::default()),
            r#"command("inner", function() set("fill-column", 40) end)
               command("outer", function() run("inner"); assert(get("fill-column") == 40) end)"#,
        );
        result.unwrap();
        let ed = handle(ed, Some(&lua), Event::Run("outer".to_owned()));
        assert_eq!(ed.message(), "");
        assert_eq!(ed.config().fill_column, 40);
    }

    #[test]
    fn run_sequence_saves_before_quit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        fs::write(&path, b"abc").unwrap();
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default());
        let fx = request_open(&ed, &path, None, true, Vec::new());
        let ed = perform_all(ed, None, fx);
        let ed = handle(ed, None, Event::Key(Key::plain(Code::Char('x'))));

        let (ed, result) = script(
            &lua,
            ed,
            r#"command("finish", function() run("save-buffer"); run("save-buffers-kill-emacs") end)"#,
        );
        result.unwrap();
        let ed = handle(ed, Some(&lua), Event::Run("finish".to_owned()));
        assert!(ed.quit());
        assert_eq!(fs::read(&path).unwrap(), b"xabc");
    }

    #[test]
    fn reentrant_run_does_not_panic() {
        let lua = new_state().unwrap();
        let (ed, result) = script(
            &lua,
            Editor::new(Config::default()),
            r#"command("again", function() run("again") end)"#,
        );
        result.unwrap();
        let ed = handle(ed, Some(&lua), Event::Run("again".to_owned()));
        assert_ne!(ed.message(), "");
        assert!(!ed.quit());
    }

    #[test]
    fn test_pruned_commands_unregistered() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let pruned = [
            "what-cursor-position",
            "what-line",
            "count-words",
            "count-words-region",
            "transpose-chars",
            "transpose-words",
            "open-line",
            "join-line",
            "delete-indentation",
            "just-one-space",
            "delete-horizontal-space",
            "delete-blank-lines",
            "upcase-region",
            "downcase-region",
            "kill-whole-line",
            "upcase-word",
            "downcase-word",
            "capitalize-word",
            "forward-sentence",
            "backward-sentence",
            "kill-sentence",
            "forward-paragraph",
            "backward-paragraph",
            "mark-paragraph",
            "comment-line",
        ];
        for cmd in pruned {
            assert!(
                !ed.commands().contains(cmd),
                "Command '{cmd}' should be pruned from registered commands"
            );
        }
    }

    #[test]
    fn test_kill_line_and_exchange_point_and_mark_override_allowed() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);

        // Unlike BUILTINS which are protected from Lua redefinition,
        // bundled text.lua commands can be cleanly overridden in user init.lua
        let (ed, res) = script(
            &lua,
            ed,
            r#"
            command("kill-line", function()
                message("custom kill-line")
            end)
            command("exchange-point-and-mark", function()
                message("custom exchange")
            end)
            run("kill-line")
            "#,
        );
        res.unwrap();
        assert_eq!(ed.message(), "custom kill-line");

        let (ed, res) = script(&lua, ed, r#"run("exchange-point-and-mark")"#);
        res.unwrap();
        assert_eq!(ed.message(), "custom exchange");
    }
}
