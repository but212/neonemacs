//! Lua `buffer` table: reads and edits the selected buffer from Lua.

use super::{editor_ref, mutate_editor};
use crate::core::Editor;
use mlua::{Lua, Scope, Table};
use std::cell::RefCell;

#[allow(clippy::too_many_lines)]
pub(super) fn install_buffer_api<'lua, 'scope>(
    lua: &'lua Lua,
    scope: &'lua Scope<'lua, 'scope>,
    g: &Table,
    cell: &'scope RefCell<Option<Editor>>,
) -> mlua::Result<()> {
    let buffer_table = lua.create_table()?;
    buffer_table.set(
        "point",
        scope.create_function(|_, ()| Ok(editor_ref(cell).window().point))?,
    )?;
    buffer_table.set(
        "set_point",
        scope.create_function(|_, pos: usize| {
            mutate_editor(cell, |ed| ed.move_point(pos));
            Ok(())
        })?,
    )?;
    buffer_table.set(
        "len",
        scope.create_function(|_, ()| Ok(editor_ref(cell).buffer().text().len()))?,
    )?;
    buffer_table.set(
        "get_text",
        scope.create_function(|lua, (start, end): (Option<usize>, Option<usize>)| {
            let ed = editor_ref(cell);
            let text = ed.buffer().text();
            let len = text.len();
            let s = start.unwrap_or(0).min(len);
            let e = end.unwrap_or(len).min(len);
            if s >= e {
                return lua.create_string("");
            }
            let bytes = text.bytes(s..e);
            lua.create_string(&bytes)
        })?,
    )?;
    buffer_table.set(
        "line_start",
        scope.create_function(|_, pos: usize| {
            let ed = editor_ref(cell);
            let pos = pos.min(ed.buffer().text().len());
            Ok(ed.buffer().text().line_start(pos))
        })?,
    )?;
    buffer_table.set(
        "line_end",
        scope.create_function(|_, pos: usize| {
            let ed = editor_ref(cell);
            let pos = pos.min(ed.buffer().text().len());
            Ok(ed.buffer().text().line_end(pos))
        })?,
    )?;
    buffer_table.set(
        "width",
        scope.create_function(|_, s: mlua::LuaString| {
            let ed = editor_ref(cell);
            let m = ed.config().metrics();
            let bytes = s.as_bytes();
            let lossy = String::from_utf8_lossy(&bytes);
            Ok(crate::core::str_width(&lossy, m))
        })?,
    )?;
    buffer_table.set(
        "insert",
        scope.create_function(|_, (pos, text): (usize, mlua::LuaString)| {
            let bytes = text.as_bytes();
            mutate_editor(cell, |ed| ed.insert_at(pos, &bytes));
            Ok(())
        })?,
    )?;
    buffer_table.set(
        "delete",
        scope.create_function(|_, (start, end): (usize, usize)| {
            mutate_editor(cell, |ed| {
                let (ed, _) = ed.delete_range(start, end);
                ed
            });
            Ok(())
        })?,
    )?;
    buffer_table.set(
        "kill",
        scope.create_function(|_, (start, end): (usize, usize)| {
            mutate_editor(cell, |ed| ed.kill_range(start, end));
            Ok(())
        })?,
    )?;
    buffer_table.set(
        "replace",
        scope.create_function(|_, (start, end, text): (usize, usize, mlua::LuaString)| {
            let bytes = text.as_bytes();
            mutate_editor(cell, |ed| {
                let (ed, _) = ed.delete_range(start, end);
                ed.insert_at(start, &bytes)
            });
            Ok(())
        })?,
    )?;
    buffer_table.set(
        "mark",
        scope.create_function(|_, ()| Ok(editor_ref(cell).buffer().mark()))?,
    )?;
    buffer_table.set(
        "set_mark",
        scope.create_function(|_, pos: Option<usize>| {
            mutate_editor(cell, |ed| ed.with_current_buffer(|b| b.set_mark(pos)));
            Ok(())
        })?,
    )?;
    buffer_table.set(
        "comment_prefix",
        scope.create_function(|_, ()| {
            let ed = editor_ref(cell);
            let prefix = ed
                .syntax_registry()
                .line_comment_for_filename(ed.buffer().name())
                .map(str::to_owned);
            Ok(prefix)
        })?,
    )?;
    g.set("buffer", buffer_table)?;
    Ok(())
}
