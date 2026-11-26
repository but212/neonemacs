//! Lua `buffer` table: reads and edits the selected buffer from Lua.

use super::{editor_ref, mutate_editor};
use crate::core::Editor;
use mlua::{Lua, Scope, Table};
use neonemacs_core::{line_end, line_start};
use std::cell::RefCell;

fn push_chars(out: &mut Vec<u8>, chars: impl Iterator<Item = char>) {
    let mut buf = [0u8; 4];
    for ch in chars {
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    }
}

/// Converts valid UTF-8 chunks according to `mode` (`"up"`, `"down"`, `"cap"`), preserving raw non-UTF-8 bytes.
fn convert_utf8_case(mode: &str, bytes: &[u8]) -> mlua::Result<Vec<u8>> {
    if !matches!(mode, "up" | "down" | "cap") {
        return Err(mlua::Error::RuntimeError(format!(
            "invalid case mode: {mode}"
        )));
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut first_char = true;
    for chunk in bytes.utf8_chunks() {
        let valid = chunk.valid();
        match mode {
            "up" => out.extend_from_slice(valid.to_uppercase().as_bytes()),
            "down" => out.extend_from_slice(valid.to_lowercase().as_bytes()),
            "cap" => {
                for ch in valid.chars() {
                    if first_char {
                        first_char = false;
                        push_chars(&mut out, ch.to_uppercase());
                    } else {
                        push_chars(&mut out, ch.to_lowercase());
                    }
                }
            }
            _ => unreachable!(),
        }
        out.extend_from_slice(chunk.invalid());
    }
    Ok(out)
}

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
        scope.create_function(|_, ()| Ok(editor_ref(cell).buffer().text().total_bytes()))?,
    )?;
    buffer_table.set(
        "get_text",
        scope.create_function(|lua, (start, end): (Option<usize>, Option<usize>)| {
            let ed = editor_ref(cell);
            let bytes = neonemacs_core::doc_range_bytes(
                ed.buffer().text(),
                start.unwrap_or(0)..end.unwrap_or(usize::MAX),
            );
            lua.create_string(&bytes)
        })?,
    )?;
    buffer_table.set(
        "line_start",
        scope.create_function(|_, pos: usize| {
            Ok(line_start(editor_ref(cell).buffer().text(), pos))
        })?,
    )?;
    buffer_table.set(
        "line_end",
        scope
            .create_function(|_, pos: usize| Ok(line_end(editor_ref(cell).buffer().text(), pos)))?,
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
            let applied =
                crate::shell::with_editor_cell(cell, |ed| ed.replace_range(start, end, &bytes));
            Ok(applied)
        })?,
    )?;
    buffer_table.set(
        "case_convert",
        scope.create_function(|lua, (mode, text): (String, mlua::LuaString)| {
            let bytes = text.as_bytes();
            let out = convert_utf8_case(&mode, &bytes)?;
            lua.create_string(&out)
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
                .find_by_filename(ed.buffer().name())
                .and_then(|d| d.line_comment.as_deref())
                .map(str::to_owned);
            Ok(prefix)
        })?,
    )?;
    g.set("buffer", buffer_table)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::core::Config;
    use crate::core::Editor;
    use crate::shell::lua::{bootstrap, new_state, test_script as script};

    #[test]
    fn set_mark_and_query_from_lua_manage_active_region() {
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(crate::core::Doc::from_bytes(b"hello world")));
        assert_eq!(ed.buffer().mark(), None);

        let (ed, res) = script(&lua, ed, "buffer.set_mark(5)");
        res.unwrap();
        assert!(ed.buffer().mark_active());
        assert_eq!(ed.buffer().mark(), Some(5));

        let (ed, res) = script(
            &lua,
            ed,
            r#"
            assert(buffer.mark() == 5, "mark should be 5")
            buffer.set_mark(nil)
        "#,
        );
        res.unwrap();
        assert!(!ed.buffer().mark_active());
        assert_eq!(ed.buffer().mark(), None);
    }

    #[test]
    fn set_mark_snaps_to_unit_boundary() {
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(crate::core::Doc::from_bytes("가".as_bytes())));
        let (ed, res) = script(&lua, ed, "buffer.set_mark(1)");
        res.unwrap();
        assert_eq!(ed.buffer().mark(), Some(0));

        let (ed, res) = script(&lua, ed, "buffer.set_mark(999)");
        res.unwrap();
        assert_eq!(ed.buffer().mark(), Some(3));
    }

    #[test]
    fn get_text_preserves_byte_ranges_inside_utf8_units() {
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(crate::core::Doc::from_bytes("é가".as_bytes())));
        let (_, result) = script(
            &lua,
            ed,
            r#"
            assert(buffer.get_text(1, 1) == "")
            assert(buffer.get_text(1, 0) == "")
            assert(buffer.get_text(1, 2) == string.char(0xA9))
            assert(buffer.get_text(1, 3) == string.char(0xA9, 0xEA))
            assert(buffer.get_text(4, 99) == string.char(0x80))
            "#,
        );
        result.unwrap();
    }

    #[test]
    fn api_buffer_inspection_and_mutation() {
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default());
        let (ed, id) = ed.add_buffer(
            "test",
            crate::core::Doc::from_bytes(b"hello world\nline two"),
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
        assert_eq!(
            ed.kill_ring().entries(),
            &[neonemacs::core::Doc::from_bytes(b"hi ")]
        );
    }

    #[test]
    fn comment_prefix_message_and_command_sync() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let (ed, id) = ed.add_buffer(
            "main.rs",
            crate::core::Doc::default(),
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

    #[test]
    fn replace_is_single_undo_step_and_word_case_commands_work() {
        use crate::core::Doc;
        use crate::core::effect::Event;
        use crate::shell::handle;

        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let ed = ed
            .with_current_buffer(|b| b.with_text(Doc::from_bytes("  café BAR baz".as_bytes())));
        let ed = ed.with_window(|w| crate::core::Window { point: 0, ..w });

        // M-u: skips leading spaces, upcases "café" -> "CAFÉ", moves point to end of word
        let ed = handle(ed, Some(&lua), Event::Run("upcase-word".to_owned()));
        assert_eq!(*ed.buffer().text(), Doc::from_bytes("  CAFÉ BAR baz".as_bytes()));
        assert_eq!(ed.window().point, 7);

        // Single undo restores the entire word in one step
        let (ed, _) = ed.undo(Vec::new());
        assert_eq!(*ed.buffer().text(), Doc::from_bytes("  café BAR baz".as_bytes()));

        // M-l on "BAR"
        let ed = ed.with_window(|w| crate::core::Window { point: 8, ..w });
        let ed = handle(ed, Some(&lua), Event::Run("downcase-word".to_owned()));
        assert_eq!(*ed.buffer().text(), Doc::from_bytes("  café bar baz".as_bytes()));
        assert_eq!(ed.window().point, 11);

        // M-c from middle of "baz" (at 'a')
        let ed = ed.with_window(|w| crate::core::Window { point: 13, ..w });
        let ed = handle(ed, Some(&lua), Event::Run("capitalize-word".to_owned()));
        assert_eq!(*ed.buffer().text(), Doc::from_bytes("  café bar bAz".as_bytes()));
        assert_eq!(ed.window().point, 15);
    }

    #[test]
    fn word_case_commands_respect_read_only_and_buffer_end() {
        use crate::core::Doc;
        use crate::core::effect::Event;
        use crate::shell::handle;

        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let ed = ed.with_current_buffer(|b| {
            b.with_text(Doc::from_bytes(b"hello   "))
                .set_view_only(true)
        });
        let ed = ed.with_window(|w| crate::core::Window { point: 0, ..w });

        let ed = handle(ed, Some(&lua), Event::Run("upcase-word".to_owned()));
        assert_eq!(*ed.buffer().text(), Doc::from_bytes(b"hello   "));
        assert_eq!(ed.window().point, 0);
        assert_eq!(ed.message(), "Buffer is read-only");

        // Writable buffer at trailing whitespace -> no-op
        let ed = ed.with_current_buffer(|b| b.set_view_only(false));
        let ed = ed.with_window(|w| crate::core::Window { point: 5, ..w });
        let ed = handle(ed, Some(&lua), Event::Run("upcase-word".to_owned()));
        assert_eq!(*ed.buffer().text(), Doc::from_bytes(b"hello   "));
        assert_eq!(ed.window().point, 5);
    }
}
