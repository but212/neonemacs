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
            mutate_editor(cell, |ed| ed.delete_only(start, end));
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
            mutate_editor(cell, |ed| ed.delete_only(start, end).insert_at(start, &bytes));
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

#[cfg(test)]
mod tests {
    use super::super::{bootstrap, new_state, test_script as script};
    use crate::core::Config;
    use crate::core::Editor;

    #[test]
    fn set_mark_from_lua_makes_an_active_region() {
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(crate::core::Text::from_bytes(b"abc")));
        let (ed, res) = script(&lua, ed, "buffer.set_mark(2)");
        res.unwrap();
        assert!(ed.buffer().mark_active());
        let (ed, res) = script(&lua, ed, "buffer.set_mark(nil)");
        res.unwrap();
        assert!(!ed.buffer().mark_active());
        assert_eq!(ed.buffer().mark(), None);
    }

    #[test]
    fn set_mark_snaps_to_unit_boundary() {
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(crate::core::Text::from_bytes("가".as_bytes())));
        let (ed, res) = script(&lua, ed, "buffer.set_mark(1)");
        res.unwrap();
        assert_eq!(ed.buffer().mark(), Some(0));

        let (ed, res) = script(&lua, ed, "buffer.set_mark(999)");
        res.unwrap();
        assert_eq!(ed.buffer().mark(), Some(3));
    }

    #[test]
    fn api_buffer_inspection_and_mutation() {
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default());
        let (ed, id) = ed.add_buffer(
            "test",
            crate::core::Text::from_bytes(b"hello world\nline two"),
            crate::core::Backing::None,
        );
        let ed = ed.with_window(|w| crate::core::Window {
            buffer: id,
            point: 0,
            ..w
        });

        let (ed, result) = script(
            &lua,
            ed,
            r#"
            assert(buffer.len() == 20)
            assert(buffer.point() == 0)
            assert(buffer.get_text() == "hello world\nline two")
            assert(buffer.get_text(0, 5) == "hello")
            assert(buffer.line_start(3) == 0)
            assert(buffer.line_end(3) == 11)
            assert(buffer.line_start(15) == 12)
            assert(buffer.width("가나") == 4)

            buffer.set_point(5)
            assert(buffer.point() == 5)

            buffer.insert(5, " beautiful")
            assert(buffer.get_text(0, 21) == "hello beautiful world")

            buffer.delete(5, 15)
            assert(buffer.get_text(0, 11) == "hello world")

            buffer.replace(0, 5, "hi")
            assert(buffer.get_text(0, 8) == "hi world")

            buffer.kill(0, 3)
            assert(buffer.get_text(0, 5) == "world")
        "#,
        );
        result.unwrap();
        assert_eq!(ed.kill_ring().entries(), &[b"hi ".to_vec()]);
    }

    #[test]
    fn buffer_mark_api() {
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(crate::core::Text::from_bytes(b"hello world")));
        assert_eq!(ed.buffer().mark(), None);

        // set mark to 5
        let (ed, res) = script(&lua, ed, r"buffer.set_mark(5)");
        res.unwrap();
        assert_eq!(ed.buffer().mark(), Some(5));

        // query mark from lua
        let (ed, res) = script(
            &lua,
            ed,
            r#"
            local m = buffer.mark()
            assert(m == 5, "mark should be 5")
            buffer.set_mark(nil)
        "#,
        );
        res.unwrap();
        assert_eq!(ed.buffer().mark(), None);
    }

    #[test]
    fn comment_prefix_message_and_command_sync() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let (ed, id) = ed.add_buffer(
            "main.rs",
            crate::core::Text::default(),
            crate::core::Backing::None,
        );
        let ed = ed.with_window(|w| crate::core::Window { buffer: id, ..w });

        // comment_prefix for rust buffer
        let (ed, res) = script(&lua, ed, r#"assert(buffer.comment_prefix() == "//")"#);
        res.unwrap();

        // message FFI
        let (ed, res) = script(&lua, ed, r#"message("hello from lua")"#);
        res.unwrap();
        assert_eq!(ed.message(), "hello from lua");

        // command sync into Editor::commands
        let (ed, res) = script(&lua, ed, r#"command("my-test-lua-cmd", function() end)"#);
        res.unwrap();
        assert!(ed.commands().contains("my-test-lua-cmd"));
    }

    #[test]
    fn test_count_lines_ffi_removed() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let (_ed, res) = script(
            &lua,
            ed,
            r#"
            assert(buffer.count_lines == nil, "buffer.count_lines FFI should not exist")
            "#,
        );
        res.unwrap();
    }
}
