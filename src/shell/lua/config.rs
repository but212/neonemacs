//! Lua `get` and `set`: reads and changes editor settings.

use super::{editor_ref, mutate_editor};
use crate::core::Editor;
use crate::core::{Setting, Value};
use mlua::{Lua, Scope, Table, Value as LuaValue};
use std::cell::RefCell;

pub(super) fn install_config_api<'lua, 'scope>(
    scope: &'lua Scope<'lua, 'scope>,
    g: &Table,
    cell: &'scope RefCell<Option<Editor>>,
) -> mlua::Result<()> {
    g.set(
        "get",
        scope.create_function(|lua, name: String| {
            let setting = Setting::parse(&name).map_err(mlua::Error::RuntimeError)?;
            to_lua(lua, editor_ref(cell).config().get(setting))
        })?,
    )?;
    g.set(
        "set",
        scope.create_function(|_, (name, value): (String, LuaValue)| {
            let setting = Setting::parse(&name).map_err(mlua::Error::RuntimeError)?;
            let value = from_lua(&value).map_err(mlua::Error::RuntimeError)?;
            let new_config = editor_ref(cell)
                .config()
                .set(setting, value)
                .map_err(mlua::Error::RuntimeError)?;
            mutate_editor(cell, |ed| ed.set_config(new_config));
            Ok(())
        })?,
    )?;
    Ok(())
}

fn to_lua(lua: &Lua, value: Value) -> mlua::Result<LuaValue> {
    Ok(match value {
        Value::Int(n) => LuaValue::Integer(n),
        Value::Bool(b) => LuaValue::Boolean(b),
        Value::Str(s) => LuaValue::String(lua.create_string(&s)?),
    })
}

fn from_lua(value: &LuaValue) -> Result<Value, String> {
    match value {
        LuaValue::Integer(n) => Ok(Value::Int(*n)),
        LuaValue::Boolean(b) => Ok(Value::Bool(*b)),
        LuaValue::String(s) => s
            .to_str()
            .map(|s| Value::Str(s.to_string()))
            .map_err(|_| "setting string must be valid UTF-8".to_owned()),
        other => Err(format!("unsupported setting value: {}", other.type_name())),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{bootstrap, new_state, test_script as script};
    use crate::core::Config;
    use crate::core::Editor;
    use crate::core::Edit;
    use crate::core::Eol;
    use crate::core::Toggles;

    #[test]
    fn string_settings_cross_the_lua_boundary() {
        let lua = new_state().unwrap();
        let (ed, result) = script(
            &lua,
            Editor::new(Config::default()),
            r#"assert(get("default-eol") == "lf"); set("default-eol", "crlf"); assert(get("default-eol") == "crlf")"#,
        );
        result.unwrap();
        assert_eq!(ed.config().default_eol, Eol::Crlf);
        let (ed, result) = script(&lua, ed, r#"set("default-eol", "cr")"#);
        assert!(result.is_err());
        assert_eq!(ed.config().default_eol, Eol::Crlf);
        let (_, result) = script(&lua, ed, r#"set("default-eol", 1)"#);
        assert!(result.is_err());
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
        assert!(!ed.config().toggles.contains(Toggles::CASE_FOLD_SEARCH));
        let (ed, result) = script(&lua, ed, r#"set("tab-width", 0)"#);
        assert!(result.is_err());
        let (ed, result) = script(&lua, ed, r#"set("nope", 1)"#);
        assert!(result.is_err());
        let (_, result) = script(&lua, ed, r#"set("fill-column", "x")"#);
        assert!(result.is_err());
    }

    #[test]
    fn undo_limit_applies_to_existing_buffers() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let (ed, result) = script(&lua, ed, r#"set("undo-limit", 0)"#);
        result.unwrap();

        // Two edits in two undo groups. With limit 0 the first group is dropped at once (I-35).
        let (ed, _) = ed.apply(&Edit::insert(0, b"a".as_slice()));
        let ed = ed.end_command();
        let (ed, _) = ed.apply(&Edit::insert(1, b"b".as_slice()));
        let ed = ed.end_command();

        // Each undo is its own command, as in the event loop.
        let (ed, _) = ed.begin_command().undo(Vec::new());
        let (ed, _) = ed.begin_command().undo(Vec::new());
        assert_eq!(ed.message(), "No further undo information");
    }
}
