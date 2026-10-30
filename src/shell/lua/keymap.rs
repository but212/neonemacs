//! Lua `bind` and `unbind`: keymap changes made from Lua.

use super::mutate_editor;
use crate::core::editor::Editor;
use crate::core::keymap::{KeyError, parse_key_seq};
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
