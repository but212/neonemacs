//! Lua extension API (§12, R4). Five functions: `bind`, `command`, `run`, `get`, `set`.
//!
//! They are installed only while Lua code runs, so each one
//! reaches the editor through a `RefCell` that lasts for that call.
//!
//! Each submodule installs one group of functions into the Lua globals.

mod answer;
mod buffer;
mod command;
mod config;
mod keymap;
mod syntax;
#[cfg(test)]
mod tests;

use crate::core::editor::Editor;
use answer::install_answer_api;
use buffer::install_buffer_api;
use command::install_command_api;
use config::install_config_api;
use keymap::install_keymap_api;
use mlua::Lua;
use std::cell::{Cell, Ref, RefCell};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use syntax::install_syntax_api;

pub(crate) use command::call_lua;

/// Global table holding the commands defined with `command`.
const COMMANDS: &str = "neonemacs_commands";

/// The default syntax definitions bundled into the binary (§5, §12).
pub const BUNDLED_SYNTAX: &str = include_str!("../syntax.lua");

/// Standard text transformation commands bundled into the binary (§6, §12).
pub const BUNDLED_TEXT: &str = include_str!("../text.lua");

/// The default extended keymap bundled into the binary (§3, §12).
pub const BUNDLED_KEYMAP: &str = include_str!("../keymap.lua");

/// All bundled scripts loaded during bootstrap, in evaluation order.
pub const BUNDLED_SCRIPTS: &[(&str, &str)] = &[
    ("syntax.lua", BUNDLED_SYNTAX),
    ("text.lua", BUNDLED_TEXT),
    ("keymap.lua", BUNDLED_KEYMAP),
];

/// Bootstraps built-in Lua scripts (such as syntax definitions and default keymaps) into `ed`.
///
/// # Panics
/// Panics if any bundled script fails to load.
#[must_use]
pub fn bootstrap(mut ed: Editor, lua: &Lua) -> Editor {
    for (name, source) in BUNDLED_SCRIPTS {
        let (next_ed, result) = crate::shell::run_source(ed, lua, source);
        if let Err(e) = result {
            panic!("fatal: bundled {name} failed to load: {e}");
        }
        ed = next_ed;
    }
    ed
}

/// A Lua state with the command table in place.
///
/// # Errors
/// Returns `mlua::Error` if initializing global tables fails.
pub fn new_state() -> mlua::Result<Lua> {
    let lua = Lua::new();
    lua.globals().set(COMMANDS, lua.create_table()?)?;
    Ok(lua)
}

/// `$XDG_CONFIG_HOME/neonemacs/init.lua`, else `<home>/.config/neonemacs/init.lua` (U2).
#[must_use]
pub fn config_path(xdg: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    xdg.filter(|s| !s.is_empty()).map_or_else(
        || home.map(|h| h.join(".config").join("neonemacs").join("init.lua")),
        |xdg| Some(PathBuf::from(xdg).join("neonemacs").join("init.lua")),
    )
}

/// The editor in `cell`. The cell is full whenever Lua runs (I-21).
fn editor_ref(cell: &RefCell<Option<Editor>>) -> Ref<'_, Editor> {
    Ref::map(cell.borrow(), |slot| {
        slot.as_ref().expect("editor is in the cell (I-21)")
    })
}

/// Applies an infallible transition `f` to the editor inside `cell`.
fn mutate_editor(cell: &RefCell<Option<Editor>>, f: impl FnOnce(Editor) -> Editor) {
    let current = cell
        .borrow_mut()
        .take()
        .expect("editor is in the cell (I-21)");
    *cell.borrow_mut() = Some(f(current));
}

/// Runs `body` with the API bound to the editor in `cell`.
///
/// # Errors
/// Returns `mlua::Error` if binding any Lua API or executing `body` fails.
pub fn with_api<R>(
    lua: &Lua,
    cell: &RefCell<Option<Editor>>,
    body: impl FnOnce(&Lua) -> mlua::Result<R>,
) -> mlua::Result<R> {
    let depth = Cell::new(0);
    lua.scope(|scope| {
        let g = lua.globals();
        install_keymap_api(scope, &g, cell)?;
        install_answer_api(scope, &g, cell)?;
        install_command_api(scope, &g, cell, &depth)?;
        install_config_api(scope, &g, cell)?;
        install_syntax_api(lua, scope, &g, cell)?;
        install_buffer_api(lua, scope, &g, cell)?;
        body(lua)
    })
}
