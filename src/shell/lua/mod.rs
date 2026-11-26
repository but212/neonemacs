//! Lua extension API (§12, R4). Five functions: `bind`, `command`, `run`, `get`, `set`.
//!
//! They are installed only while Lua code runs, so each one
//! reaches the editor through a `RefCell` that lasts for that call.
//!
//! Each submodule installs one group of functions into the Lua globals.

mod buffer;
mod command;
mod settings;
mod syntax;
#[cfg(test)]
mod tests;

use crate::core::Editor;
use buffer::install_buffer_api;
use command::install_command_api;
use mlua::{Lua, LuaOptions, StdLib};
use settings::install_settings_api;
use std::cell::{Cell, Ref, RefCell};
use syntax::install_syntax_api;

pub(crate) use command::call_lua;

/// Global table holding the commands defined with `command`.
const COMMANDS: &str = "neonemacs_commands";

/// The default extended keymap bundled into the binary (§3, §12).
pub const BUNDLED_KEYMAP: &str = include_str!("../keymap.lua");

/// All bundled scripts loaded during bootstrap, in evaluation order.
pub const BUNDLED_SCRIPTS: &[(&str, &str)] = &[
    ("syntax.lua", include_str!("../syntax.lua")),
    ("text.lua", include_str!("../text.lua")),
    ("keymap.lua", BUNDLED_KEYMAP),
];

/// Bootstraps built-in Lua scripts (such as syntax definitions and default keymaps) into `ed`.
///
/// # Panics
/// Panics if any bundled script fails to load.
#[must_use]
pub fn bootstrap(mut ed: Editor, lua: &Lua) -> Editor {
    for (name, source) in BUNDLED_SCRIPTS {
        let (next_ed, result) = crate::shell::run_source(ed, lua, &format!("={name}"), source);
        if let Err(e) = result {
            panic!("fatal: bundled {name} failed to load: {e}");
        }
        ed = next_ed;
    }
    ed
}

/// A Lua state with the command table in place.
///
/// Only `string`, `table`, `math`, and `utf8` are loaded next to the base functions, and the base
/// functions that read files are removed, so Lua code reaches the file system and processes only
/// through the editor API (I-57).
///
/// # Errors
/// Returns `mlua::Error` if creating the state or its global tables fails.
pub fn new_state() -> mlua::Result<Lua> {
    let libs = StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::UTF8;
    let lua = Lua::new_with(libs, LuaOptions::default())?;
    let g = lua.globals();
    for name in ["dofile", "loadfile"] {
        g.set(name, mlua::Value::Nil)?;
    }
    g.set(COMMANDS, lua.create_table()?)?;
    Ok(lua)
}

/// The editor in `cell`. The cell is full whenever Lua runs (I-21).
fn editor_ref(cell: &RefCell<crate::shell::EditorSlot>) -> Ref<'_, Editor> {
    Ref::map(cell.borrow(), crate::shell::EditorSlot::as_ref)
}

/// Applies an infallible transition `f` to the editor inside `cell`.
fn mutate_editor(cell: &RefCell<crate::shell::EditorSlot>, f: impl FnOnce(Editor) -> Editor) {
    crate::shell::with_editor_cell(cell, |ed| (f(ed), ()));
}

/// Runs `body` with the API bound to the editor in `cell`.
///
/// # Errors
/// Returns `mlua::Error` if binding any Lua API or executing `body` fails.
pub fn with_api<R>(
    lua: &Lua,
    cell: &RefCell<crate::shell::EditorSlot>,
    body: impl FnOnce(&Lua) -> mlua::Result<R>,
) -> mlua::Result<R> {
    let depth = Cell::new(0);
    lua.scope(|scope| {
        let g = lua.globals();
        install_settings_api(scope, &g, cell)?;
        install_command_api(scope, &g, cell, &depth)?;
        install_syntax_api(lua, scope, &g, cell)?;
        install_buffer_api(lua, scope, &g, cell)?;
        body(lua)
    })
}

#[cfg(test)]
pub(crate) fn test_script(lua: &Lua, ed: Editor, src: &str) -> (Editor, mlua::Result<()>) {
    crate::shell::run_source(ed, lua, "=test", src)
}
