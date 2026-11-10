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
