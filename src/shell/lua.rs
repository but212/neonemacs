//! Lua extension API (§12, R4). Five functions: `bind`, `command`, `run`,
//! `get`, `set`. They are installed only while Lua code runs, so each one
//! reaches the editor through a `RefCell` that lasts for that call.

use crate::core::commands;
use crate::core::config::Value;
use crate::core::editor::Editor;
use crate::core::effect::Event;
use crate::core::keymap::KeyError;
use crate::core::syntax::SyntaxDef;
use crate::shell::{perform_in, step_in};
use mlua::{Function, Lua, Scope, Table, Value as LuaValue};
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Global table holding the commands defined with `command`.
const COMMANDS: &str = "neonemacs_commands";

/// The default syntax definitions bundled into the binary (§5, §12).
pub const BUNDLED_SYNTAX: &str = include_str!("syntax.lua");

/// Standard text transformation commands bundled into the binary (§6, §12).
pub const BUNDLED_TEXT: &str = include_str!("text.lua");

/// The default extended keymap bundled into the binary (§3, §12).
pub const BUNDLED_KEYMAP: &str = include_str!("keymap.lua");

/// All bundled scripts loaded during bootstrap, in evaluation order.
pub const BUNDLED_SCRIPTS: &[(&str, &str)] = &[
    ("syntax.lua", BUNDLED_SYNTAX),
    ("text.lua", BUNDLED_TEXT),
    ("keymap.lua", BUNDLED_KEYMAP),
];

/// Bootstraps built-in Lua scripts (such as syntax definitions and default keymaps) into `ed`.
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

/// Backwards-compatible alias for `bootstrap`.
pub fn load_bundled_syntax(ed: Editor, lua: &Lua) -> Editor {
    bootstrap(ed, lua)
}

/// Most nested `run` calls allowed. A command that runs itself stops with an error here, before the native stack runs out.
const MAX_RUN_DEPTH: usize = 50;

/// A Lua state with the command table in place.
pub fn new_state() -> mlua::Result<Lua> {
    let lua = Lua::new();
    lua.globals().set(COMMANDS, lua.create_table()?)?;
    Ok(lua)
}

/// `$XDG_CONFIG_HOME/neonemacs/init.lua`, else `<home>/.config/neonemacs/init.lua` (U2).
pub fn config_path(xdg: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    match xdg.filter(|s| !s.is_empty()) {
        Some(xdg) => Some(PathBuf::from(xdg).join("neonemacs").join("init.lua")),
        None => home.map(|h| h.join(".config").join("neonemacs").join("init.lua")),
    }
}

/// The editor in `cell`. The cell is full whenever Lua runs (I-21).
fn editor_ref(cell: &RefCell<Option<Editor>>) -> Ref<'_, Editor> {
    Ref::map(cell.borrow(), |slot| {
        slot.as_ref().expect("editor is in the cell (I-21)")
    })
}

/// The editor in `cell`, for writing. The cell is full whenever Lua runs (I-21).
fn editor_mut(cell: &RefCell<Option<Editor>>) -> RefMut<'_, Editor> {
    RefMut::map(cell.borrow_mut(), |slot| {
        slot.as_mut().expect("editor is in the cell (I-21)")
    })
}

/// Applies a fallible transition `f` to the editor inside `cell`.
fn mutate_editor(
    cell: &RefCell<Option<Editor>>,
    f: impl FnOnce(Editor) -> mlua::Result<Editor>,
) -> mlua::Result<()> {
    let mut ed = editor_mut(cell);
    let current = ed.clone();
    *ed = f(current)?;
    Ok(())
}

/// Runs `body` with the API bound to the editor in `cell`.
pub fn with_api<R>(
    lua: &Lua,
    cell: &RefCell<Option<Editor>>,
    body: impl FnOnce(&Lua) -> mlua::Result<R>,
) -> mlua::Result<R> {
    let depth = Cell::new(0);
    lua.scope(|scope| {
        let g = lua.globals();
        install_keymap_api(scope, &g, cell)?;
        install_command_api(lua, scope, &g, cell, &depth)?;
        install_config_api(scope, &g, cell)?;
        install_syntax_api(lua, scope, &g, cell)?;
        install_buffer_api(lua, scope, &g, cell)?;
        body(lua)
    })
}

fn install_keymap_api<'lua, 'scope>(
    scope: &'lua Scope<'lua, 'scope>,
    g: &Table,
    cell: &'scope RefCell<Option<Editor>>,
) -> mlua::Result<()> {
    g.set(
        "bind",
        scope.create_function(|_, (keys, name): (String, String)| {
            mutate_editor(cell, |ed| {
                let keymap = ed
                    .keymap()
                    .clone()
                    .bind_str(&keys, &name)
                    .map_err(|KeyError(msg)| mlua::Error::RuntimeError(msg))?;
                Ok(ed.with_keymap(|_| keymap))
            })
        })?,
    )?;
    g.set(
        "unbind",
        scope.create_function(|_, keys: String| {
            mutate_editor(cell, |ed| {
                let keymap = ed
                    .keymap()
                    .clone()
                    .unbind_str(&keys)
                    .map_err(|KeyError(msg)| mlua::Error::RuntimeError(msg))?;
                Ok(ed.with_keymap(|_| keymap))
            })
        })?,
    )?;
    Ok(())
}

fn install_command_api<'lua, 'scope>(
    _lua: &'lua Lua,
    scope: &'lua Scope<'lua, 'scope>,
    g: &Table,
    cell: &'scope RefCell<Option<Editor>>,
    depth: &'scope Cell<usize>,
) -> mlua::Result<()> {
    g.set(
        "command",
        scope.create_function(|lua, (name, f): (String, Function)| {
            if commands::find(&name).is_some() {
                return Err(mlua::Error::RuntimeError(format!(
                    "{name} is a built-in command"
                )));
            }
            lua.globals().get::<Table>(COMMANDS)?.set(name.clone(), f)?;
            mutate_editor(cell, |ed| Ok(ed.register_command(&name)))
        })?,
    )?;
    g.set(
        "message",
        scope
            .create_function(|_, msg: String| mutate_editor(cell, |ed| Ok(ed.set_message(msg))))?,
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
            let result = if commands::find(&name).is_some() {
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

fn install_config_api<'lua, 'scope>(
    scope: &'lua Scope<'lua, 'scope>,
    g: &Table,
    cell: &'scope RefCell<Option<Editor>>,
) -> mlua::Result<()> {
    g.set(
        "get",
        scope.create_function(|_, name: String| {
            let ed = editor_ref(cell);
            let value = ed.config().get(&name).map_err(mlua::Error::RuntimeError)?;
            Ok(to_lua(value))
        })?,
    )?;
    g.set(
        "set",
        scope.create_function(|_, (name, value): (String, LuaValue)| {
            let value = from_lua(&value).map_err(mlua::Error::RuntimeError)?;
            mutate_editor(cell, |ed| {
                let new_config = ed
                    .config()
                    .clone()
                    .set(&name, value)
                    .map_err(mlua::Error::RuntimeError)?;
                Ok(ed.set_config(new_config))
            })
        })?,
    )?;
    Ok(())
}

fn install_syntax_api<'lua, 'scope>(
    lua: &'lua Lua,
    scope: &'lua Scope<'lua, 'scope>,
    g: &Table,
    cell: &'scope RefCell<Option<Editor>>,
) -> mlua::Result<()> {
    let syntax_table = lua.create_table()?;
    syntax_table.set(
        "register",
        scope.create_function(|_, (name, spec): (String, Table)| {
            let def = parse_syntax_def(name, spec)?;
            mutate_editor(cell, |ed| {
                let new_registry = ed.syntax_registry().clone().register(def);
                Ok(ed.with_syntax_registry(new_registry))
            })
        })?,
    )?;
    g.set("syntax", syntax_table)?;
    Ok(())
}

fn install_buffer_api<'lua, 'scope>(
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
        scope.create_function(|_, pos: usize| mutate_editor(cell, |ed| Ok(ed.move_point(pos))))?,
    )?;
    buffer_table.set(
        "len",
        scope.create_function(|_, ()| Ok(editor_ref(cell).buffer().text.len()))?,
    )?;
    buffer_table.set(
        "get_text",
        scope.create_function(|_, (start, end): (Option<usize>, Option<usize>)| {
            let ed = editor_ref(cell);
            let text = &ed.buffer().text;
            let len = text.len();
            let s = start.unwrap_or(0).min(len);
            let e = end.unwrap_or(len).min(len);
            if s >= e {
                return Ok(String::new());
            }
            let bytes = text.bytes(s..e);
            String::from_utf8(bytes).map_err(|e| mlua::Error::RuntimeError(e.to_string()))
        })?,
    )?;
    buffer_table.set(
        "line_start",
        scope.create_function(|_, pos: usize| {
            let ed = editor_ref(cell);
            let pos = pos.min(ed.buffer().text.len());
            Ok(ed.buffer().text.line_start(pos))
        })?,
    )?;
    buffer_table.set(
        "line_end",
        scope.create_function(|_, pos: usize| {
            let ed = editor_ref(cell);
            let pos = pos.min(ed.buffer().text.len());
            Ok(ed.buffer().text.line_end(pos))
        })?,
    )?;
    buffer_table.set(
        "width",
        scope.create_function(|_, s: String| {
            let ed = editor_ref(cell);
            let tab = usize::from(ed.config().tab_width);
            Ok(crate::core::width::str_width(&s, tab))
        })?,
    )?;
    buffer_table.set(
        "insert",
        scope.create_function(|_, (pos, text): (usize, String)| {
            mutate_editor(cell, |ed| Ok(ed.insert_at(pos, text.as_bytes())))
        })?,
    )?;
    buffer_table.set(
        "delete",
        scope.create_function(|_, (start, end): (usize, usize)| {
            mutate_editor(cell, |ed| {
                let (ed, _) = ed.delete_range(start, end);
                Ok(ed)
            })
        })?,
    )?;
    buffer_table.set(
        "kill",
        scope.create_function(|_, (start, end): (usize, usize)| {
            mutate_editor(cell, |ed| Ok(ed.kill_range(start, end)))
        })?,
    )?;
    buffer_table.set(
        "replace",
        scope.create_function(|_, (start, end, text): (usize, usize, String)| {
            mutate_editor(cell, |ed| {
                let (ed, _) = ed.delete_range(start, end);
                Ok(ed.insert_at(start, text.as_bytes()))
            })
        })?,
    )?;
    buffer_table.set(
        "mark",
        scope.create_function(|_, ()| Ok(editor_ref(cell).buffer().mark))?,
    )?;
    buffer_table.set(
        "set_mark",
        scope.create_function(|_, pos: Option<usize>| {
            mutate_editor(cell, |ed| {
                Ok(ed.with_current_buffer(|b| crate::core::buffer::Buffer { mark: pos, ..b }))
            })
        })?,
    )?;
    buffer_table.set(
        "comment_prefix",
        scope.create_function(|_, ()| {
            let ed = editor_ref(cell);
            let prefix = ed
                .syntax_registry()
                .line_comment_for_filename(&ed.buffer().name)
                .map(str::to_owned);
            Ok(prefix)
        })?,
    )?;
    g.set("buffer", buffer_table)?;
    Ok(())
}

fn parse_string_list(spec: &Table, key: &str) -> mlua::Result<Vec<String>> {
    let mut list = Vec::new();
    if let Ok(table) = spec.get::<Table>(key) {
        for item in table.sequence_values::<String>() {
            let s = item.map_err(|_| {
                mlua::Error::RuntimeError(format!("{key} must be an array of strings"))
            })?;
            list.push(s);
        }
    }
    Ok(list)
}

fn parse_syntax_def(name: String, spec: Table) -> mlua::Result<SyntaxDef> {
    if name.trim().is_empty() {
        return Err(mlua::Error::RuntimeError(
            "syntax name must not be empty".to_string(),
        ));
    }

    let extensions = parse_string_list(&spec, "extensions")?;
    let filenames = parse_string_list(&spec, "filenames")?;
    if extensions.is_empty() && filenames.is_empty() {
        return Err(mlua::Error::RuntimeError(
            "syntax spec must have at least one extension or filename".to_string(),
        ));
    }

    let line_comment: Option<String> = spec.get("line_comment").ok();
    let keywords = parse_string_list(&spec, "keywords")?;
    let types = parse_string_list(&spec, "types")?;
    let case_insensitive: bool = spec.get("case_insensitive").unwrap_or(false);

    Ok(SyntaxDef::builder(name)
        .extensions(extensions)
        .filenames(filenames)
        .line_comment(line_comment)
        .keywords(keywords)
        .types(types)
        .case_insensitive(case_insensitive)
        .build())
}

/// Calls the Lua command `name`, which must have been defined with `command`.
pub(crate) fn call_lua(lua: &Lua, name: &str) -> mlua::Result<()> {
    let table: Table = lua.globals().get(COMMANDS)?;
    match table.get::<Option<Function>>(name)? {
        Some(f) => f.call(()),
        None => Err(mlua::Error::RuntimeError(format!(
            "No command named {name}"
        ))),
    }
}

const fn to_lua(value: Value) -> LuaValue {
    match value {
        Value::Int(n) => LuaValue::Integer(n),
        Value::Bool(b) => LuaValue::Boolean(b),
    }
}

fn from_lua(value: &LuaValue) -> Result<Value, String> {
    match value {
        LuaValue::Integer(n) => Ok(Value::Int(*n)),
        LuaValue::Boolean(b) => Ok(Value::Bool(*b)),
        other => Err(format!("unsupported setting value: {}", other.type_name())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::commands::request_open;
    use crate::core::config::Config;
    use crate::core::keymap::{Code, Key, parse_keys};
    use crate::shell::{handle, perform_all, run_source};
    use std::fs;

    /// Runs `src` against `ed`. Returns the editor back with the result.
    fn script(lua: &Lua, ed: Editor, src: &str) -> (Editor, mlua::Result<()>) {
        run_source(ed, lua, src)
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
        assert!(ed.message().is_empty());
        assert_eq!(ed.config().fill_column, 40);
    }

    #[test]
    fn run_sequence_saves_before_quit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        fs::write(&path, b"abc").unwrap();
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default());
        let fx = request_open(&ed, path.clone(), None, true, Vec::new());
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
        assert!(!ed.message().is_empty());
        assert!(!ed.quit());
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
        assert!(!ed.config().case_fold_search);
        let (ed, result) = script(&lua, ed, r#"set("tab-width", 0)"#);
        assert!(result.is_err());
        let (ed, result) = script(&lua, ed, r#"set("nope", 1)"#);
        assert!(result.is_err());
        let (_, result) = script(&lua, ed, r#"set("fill-column", "x")"#);
        assert!(result.is_err());
    }
    #[test]
    fn api_syntax_register_and_validation() {
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default());
        let (ed, result) = script(
            &lua,
            ed,
            r##"syntax.register("mylang", {
                extensions = { "xyz" },
                line_comment = "#",
                keywords = { "fn", "var" }
            })"##,
        );
        result.unwrap();
        assert_eq!(ed.syntax_registry().detect_lang("file.xyz"), Some("mylang"));
        assert!(ed.syntax_registry().is_keyword("mylang", "fn"));
        assert_eq!(ed.syntax_registry().line_comment("mylang"), Some("#"));

        // Validation failures
        let (_, result) = script(&lua, ed.clone(), r#"syntax.register("", {})"#);
        assert!(result.is_err());
        let (_, result) = script(
            &lua,
            ed.clone(),
            r#"syntax.register("bad", { extensions = 123 })"#,
        );
        assert!(result.is_err());
        // Missing both extensions and filenames
        let (_, result) = script(
            &lua,
            ed.clone(),
            r#"syntax.register("bad", { keywords = { "foo" } })"#,
        );
        assert!(result.is_err());

        // Extended fields: filenames, types, case_insensitive
        let (ed, result) = script(
            &lua,
            ed,
            r##"syntax.register("sql", {
                extensions = { "sql" },
                filenames = { "query.sql.txt" },
                case_insensitive = true,
                keywords = { "select" },
                types = { "varchar" }
            })"##,
        );
        result.unwrap();
        assert_eq!(ed.syntax_registry().detect_lang("query.sql.txt"), Some("sql"));
        assert!(ed.syntax_registry().is_keyword("sql", "SELECT"));
        assert!(ed.syntax_registry().is_type("sql", "VARCHAR"));
    }

    #[test]
    fn bundled_syntax_loaded_by_default() {
        let lua = new_state().unwrap();
        let ed = load_bundled_syntax(Editor::new(Config::default()), &lua);
        let reg = ed.syntax_registry();
        assert_eq!(reg.detect_lang("main.rs"), Some("rust"));
        assert_eq!(reg.detect_lang("script.py"), Some("python"));
        assert_eq!(reg.detect_lang("index.js"), Some("javascript"));
        assert_eq!(reg.detect_lang("config.lua"), Some("lua"));
        assert_eq!(reg.detect_lang("app.go"), Some("go"));
        assert_eq!(reg.detect_lang("build.sh"), Some("shell"));
        assert_eq!(reg.detect_lang("README.md"), Some("markdown"));
        assert_eq!(reg.detect_lang("main.c"), Some("c"));
        assert_eq!(reg.detect_lang("Makefile"), Some("makefile"));
        assert_eq!(reg.detect_lang("Cargo.toml"), Some("toml"));
        assert_eq!(reg.detect_lang("query.sql"), Some("sql"));
        assert!(reg.is_type("rust", "i32"));
        assert!(reg.is_keyword("sql", "SELECT"));
        assert!(reg.is_type("sql", "VARCHAR"));
        assert!(reg.is_keyword("shell", "if"));
    }

    #[test]
    fn api_unbind_removes_binding_and_rejects_invalid_keys() {
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default());
        let (ed, result) = script(&lua, ed, r#"bind("C-x 2", "split-window-below")"#);
        result.unwrap();
        let key = crate::core::keymap::parse_key_seq("C-x 2").unwrap();
        assert_eq!(ed.keymap().lookup(&key), Some("split-window-below"));

        let (ed, result) = script(&lua, ed, r#"unbind("C-x 2")"#);
        result.unwrap();
        assert_eq!(ed.keymap().lookup(&key), None);

        // Invalid key syntax produces error
        let (_, result) = script(&lua, ed, r#"unbind("C-left")"#);
        assert!(result.is_err());
    }

    #[test]
    fn api_buffer_inspection_and_mutation() {
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default());
        let (ed, id) = ed.add_buffer(
            "test",
            crate::core::text::Text::from_bytes(b"hello world\nline two"),
            crate::core::file::Backing::None,
        );
        let ed = ed.with_window(|w| crate::core::editor::Window {
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
    fn bundled_keymap_loaded_by_bootstrap() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let keymap = ed.keymap();

        // Check extended bindings defined in keymap.lua
        let k_c_k = crate::core::keymap::parse_key_seq("C-k").unwrap();
        assert_eq!(keymap.lookup(&k_c_k), Some("kill-line"));

        let k_split = crate::core::keymap::parse_key_seq("C-x 2").unwrap();
        assert_eq!(keymap.lookup(&k_split), Some("split-window-below"));

        let k_other = crate::core::keymap::parse_key_seq("C-x o").unwrap();
        assert_eq!(keymap.lookup(&k_other), Some("other-window"));

        let k_switch = crate::core::keymap::parse_key_seq("C-x b").unwrap();
        assert_eq!(keymap.lookup(&k_switch), Some("switch-to-buffer"));

        let k_kill = crate::core::keymap::parse_key_seq("C-x k").unwrap();
        assert_eq!(keymap.lookup(&k_kill), Some("kill-buffer"));

        let k_fill = crate::core::keymap::parse_key_seq("M-q").unwrap();
        assert_eq!(keymap.lookup(&k_fill), Some("fill-paragraph"));

        let k_goto = crate::core::keymap::parse_key_seq("M-g").unwrap();
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
                    let in_builtins = commands::find(cmd_name).is_some();
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
    fn text_fill_paragraph_in_lua() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let (ed, id) = ed.add_buffer(
            "test",
            crate::core::text::Text::from_bytes(b"  hello world foo bar"),
            crate::core::file::Backing::None,
        );
        let ed = ed.with_window(|w| crate::core::editor::Window {
            buffer: id,
            point: 0,
            ..w
        });
        let mut cfg = ed.config().clone();
        cfg.fill_column = 12;
        let ed = ed.set_config(cfg);

        let (ed, res) = script(&lua, ed, r#"run("fill-paragraph")"#);
        res.unwrap();
        assert_eq!(ed.buffer().text.to_bytes(), b"  hello\n  world foo\n  bar");
    }

    #[test]
    fn text_word_and_buffer_motion() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let (ed, id) = ed.add_buffer(
            "test",
            crate::core::text::Text::from_bytes(b"hello world neon"),
            crate::core::file::Backing::None,
        );
        let ed = ed.with_window(|w| crate::core::editor::Window {
            buffer: id,
            point: 0,
            ..w
        });

        // forward-word: 0 -> 5 ("hello"), then 5 -> 11 ("world")
        let (ed, res) = script(&lua, ed, r#"run("forward-word")"#);
        res.unwrap();
        assert_eq!(ed.window().point, 5);

        let (ed, res) = script(&lua, ed, r#"run("forward-word")"#);
        res.unwrap();
        assert_eq!(ed.window().point, 11);

        // backward-word: 11 -> 6 (start of "world")
        let (ed, res) = script(&lua, ed, r#"run("backward-word")"#);
        res.unwrap();
        assert_eq!(ed.window().point, 6);

        // end-of-buffer & beginning-of-buffer
        let (ed, res) = script(&lua, ed, r#"run("end-of-buffer")"#);
        res.unwrap();
        assert_eq!(ed.window().point, 16);

        let (ed, res) = script(&lua, ed, r#"run("beginning-of-buffer")"#);
        res.unwrap();
        assert_eq!(ed.window().point, 0);
    }

    #[test]
    fn buffer_mark_api() {
        let lua = new_state().unwrap();
        let ed = Editor::new(Config::default());
        assert_eq!(ed.buffer().mark, None);

        // set mark to 5
        let (ed, res) = script(&lua, ed, r#"buffer.set_mark(5)"#);
        res.unwrap();
        assert_eq!(ed.buffer().mark, Some(5));

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
        assert_eq!(ed.buffer().mark, None);
    }

    #[test]
    fn text_kill_word_and_backward() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);

        let (ed, id) = ed.add_buffer(
            "test_kill",
            crate::core::text::Text::from_bytes(b"hello world and neon"),
            crate::core::file::Backing::None,
        );
        let ed = ed.with_window(|w| crate::core::editor::Window {
            buffer: id,
            point: 0,
            ..w
        });

        // kill-word from point 0
        let (ed, res) = script(&lua, ed, r#"run("kill-word")"#);
        res.unwrap();
        assert_eq!(ed.buffer().text.to_bytes(), b" world and neon");
        assert_eq!(ed.window().point, 0);

        // move to end and backward-kill-word
        let len = ed.buffer().text.len();
        let ed = ed.move_point(len);
        let (ed, res) = script(&lua, ed, r#"run("backward-kill-word")"#);
        res.unwrap();
        assert_eq!(ed.buffer().text.to_bytes(), b" world and ");
        assert_eq!(ed.window().point, ed.buffer().text.len());
    }

    #[test]
    fn comment_prefix_message_and_command_sync() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let (ed, id) = ed.add_buffer(
            "main.rs",
            crate::core::text::Text::new(),
            crate::core::file::Backing::None,
        );
        let ed = ed.with_window(|w| crate::core::editor::Window { buffer: id, ..w });

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
    fn test_comment_dwim_integration() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let (ed, id) = ed.add_buffer(
            "test.rs",
            crate::core::text::Text::from_bytes(b"    let x = 1;\n    let y = 2;"),
            crate::core::file::Backing::None,
        );
        let ed = ed.with_window(|w| crate::core::editor::Window {
            buffer: id,
            point: 4,
            ..w
        });

        // Single line comment-dwim on indented line
        let (ed, res) = script(&lua, ed, r#"run("comment-dwim")"#);
        res.unwrap();
        assert_eq!(
            ed.buffer().text.to_bytes(),
            b"    // let x = 1;\n    let y = 2;"
        );

        // Uncommenting single line with comment-dwim
        let (ed, res) = script(&lua, ed, r#"run("comment-dwim")"#);
        res.unwrap();
        assert_eq!(
            ed.buffer().text.to_bytes(),
            b"    let x = 1;\n    let y = 2;"
        );

        // Region multi-line comment
        let ed = ed.with_current_buffer(|b| crate::core::buffer::Buffer { mark: Some(0), ..b });
        let len = ed.buffer().text.len();
        let ed = ed.move_point(len);
        let (ed, res) = script(&lua, ed, r#"run("comment-dwim")"#);
        res.unwrap();
        assert_eq!(
            ed.buffer().text.to_bytes(),
            b"    // let x = 1;\n    // let y = 2;"
        );

        // Region multi-line uncomment
        let (ed, res) = script(&lua, ed, r#"run("comment-dwim")"#);
        res.unwrap();
        assert_eq!(
            ed.buffer().text.to_bytes(),
            b"    let x = 1;\n    let y = 2;"
        );

        // Missing syntax error message
        let (ed, id2) = ed.add_buffer(
            "notes.md",
            crate::core::text::Text::from_bytes(b"plain markdown text"),
            crate::core::file::Backing::None,
        );
        let ed = ed.with_window(|w| crate::core::editor::Window {
            buffer: id2,
            point: 0,
            ..w
        });
        let (ed, res) = script(&lua, ed, r#"run("comment-dwim")"#);
        res.unwrap();
        assert_eq!(ed.buffer().text.to_bytes(), b"plain markdown text");
        assert_eq!(ed.message(), "No comment syntax defined for buffer");
    }

    #[test]
    fn test_mark_whole_buffer() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);

        // Multi-line non-empty buffer
        let (ed, id) = ed.add_buffer(
            "test.txt",
            crate::core::text::Text::from_bytes(b"hello world\nline 2"),
            crate::core::file::Backing::None,
        );
        let ed = ed.with_window(|w| crate::core::editor::Window {
            buffer: id,
            point: 5,
            ..w
        });

        let (ed, res) = script(&lua, ed, r#"run("mark-whole-buffer")"#);
        res.unwrap();
        assert_eq!(ed.window().point, 0);
        assert_eq!(ed.buffer().mark, Some(18));
        assert_eq!(ed.message(), "Mark set");

        // Empty buffer
        let (ed, empty_id) = ed.add_buffer(
            "empty.txt",
            crate::core::text::Text::from_bytes(b""),
            crate::core::file::Backing::None,
        );
        let ed = ed.with_window(|w| crate::core::editor::Window {
            buffer: empty_id,
            point: 0,
            ..w
        });
        let (ed, res) = script(&lua, ed, r#"run("mark-whole-buffer")"#);
        res.unwrap();
        assert_eq!(ed.window().point, 0);
        assert_eq!(ed.buffer().mark, Some(0));
        assert_eq!(ed.message(), "Mark set");
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
    fn config_path_follows_xdg_then_home() {
        assert_eq!(
            config_path(Some(OsStr::new("/x")), Some(Path::new("/h"))),
            Some(PathBuf::from("/x/neonemacs/init.lua"))
        );
        assert_eq!(
            config_path(Some(OsStr::new("")), Some(Path::new("/h"))),
            Some(Path::new("/h/.config/neonemacs/init.lua").to_path_buf())
        );
        assert_eq!(config_path(None, None), None);
    }
}
