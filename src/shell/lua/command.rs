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
