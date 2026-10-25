//! Lua `get` and `set`: reads and changes editor settings.

use super::{editor_ref, mutate_editor};
use crate::core::config::Value;
use crate::core::editor::Editor;
use mlua::{Scope, Table, Value as LuaValue};
use std::cell::RefCell;

pub(super) fn install_config_api<'lua, 'scope>(
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
            let new_config = editor_ref(cell)
                .config()
                .set(&name, value)
                .map_err(mlua::Error::RuntimeError)?;
            mutate_editor(cell, |ed| ed.set_config(new_config));
            Ok(())
        })?,
    )?;
    Ok(())
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
