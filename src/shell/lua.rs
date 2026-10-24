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
use mlua::{Function, Lua, Table, Value as LuaValue};
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Global table holding the commands defined with `command`.
const COMMANDS: &str = "neonemacs_commands";

/// The default syntax definitions bundled into the binary (§5, §12).
pub const BUNDLED_SYNTAX: &str = include_str!("syntax.lua");

/// Bootstraps built-in Lua scripts (such as syntax definitions) into `ed`.
pub fn bootstrap(ed: Editor, lua: &Lua) -> Editor {
    let (ed, result) = crate::shell::run_source(ed, lua, BUNDLED_SYNTAX);
    if let Err(e) = result {
        panic!("fatal: bundled syntax.lua failed to load: {e}");
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

/// Runs `body` with the API bound to the editor in `cell`.
pub fn with_api<R>(
    lua: &Lua,
    cell: &RefCell<Option<Editor>>,
    body: impl FnOnce(&Lua) -> mlua::Result<R>,
) -> mlua::Result<R> {
    let depth = Cell::new(0);
    lua.scope(|scope| {
        let g = lua.globals();
        g.set(
            "bind",
            scope.create_function(|_, (keys, name): (String, String)| {
                let mut ed = editor_mut(cell);
                let new_keymap = ed
                    .keymap()
                    .clone()
                    .bind_str(&keys, &name)
                    .map_err(|KeyError(msg)| mlua::Error::RuntimeError(msg))?;
                *ed = ed.clone().set_keymap(new_keymap);
                Ok(())
            })?,
        )?;
        g.set(
            "command",
            scope.create_function(|lua, (name, f): (String, Function)| {
                if commands::find(&name).is_some() {
                    return Err(mlua::Error::RuntimeError(format!(
                        "{name} is a built-in command"
                    )));
                }
                lua.globals().get::<Table>(COMMANDS)?.set(name, f)
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
                let result = if commands::find(&name).is_some() {
                    // The editor is put back before the effects run, so Lua they call can use get and set.
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
                let mut ed = editor_mut(cell);
                let new_config = ed
                    .config()
                    .clone()
                    .set(&name, value)
                    .map_err(mlua::Error::RuntimeError)?;
                *ed = ed.clone().set_config(new_config);
                Ok(())
            })?,
        )?;
        let syntax_table = lua.create_table()?;
        syntax_table.set(
            "register",
            scope.create_function(|_, (name, spec): (String, Table)| {
                let def = parse_syntax_def(name, spec)?;
                let mut ed = editor_mut(cell);
                let new_registry = ed.syntax_registry().clone().register(def);
                *ed = ed.clone().with_syntax_registry(new_registry);
                Ok(())
            })?,
        )?;
        g.set("syntax", syntax_table)?;
        body(lua)
    })
}

fn parse_syntax_def(name: String, spec: Table) -> mlua::Result<SyntaxDef> {
    if name.trim().is_empty() {
        return Err(mlua::Error::RuntimeError(
            "syntax name must not be empty".to_string(),
        ));
    }
    let ext_table: Table = spec.get("extensions").map_err(|_| {
        mlua::Error::RuntimeError("syntax spec must have 'extensions' table".to_string())
    })?;
    let mut extensions = Vec::new();
    for ext in ext_table.sequence_values::<String>() {
        let ext = ext.map_err(|_| {
            mlua::Error::RuntimeError("extensions must be an array of strings".to_string())
        })?;
        extensions.push(ext);
    }
    if extensions.is_empty() {
        return Err(mlua::Error::RuntimeError(
            "extensions array must not be empty".to_string(),
        ));
    }

    let line_comment: Option<String> = spec.get("line_comment").ok();

    let mut keywords = Vec::new();
    if let Ok(kw_table) = spec.get::<Table>("keywords") {
        for kw in kw_table.sequence_values::<String>() {
            let kw = kw.map_err(|_| {
                mlua::Error::RuntimeError("keywords must be an array of strings".to_string())
            })?;
            keywords.push(kw);
        }
    }

    Ok(SyntaxDef::new(name, extensions, line_comment, keywords))
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
        let (_, result) = script(&lua, ed.clone(), r#"syntax.register("bad", { extensions = 123 })"#);
        assert!(result.is_err());
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
