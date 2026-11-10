//! Lua `syntax.register`: declares syntax definitions from Lua.

use super::mutate_editor;
use crate::core::editor::Editor;
use crate::core::syntax::SyntaxDef;
use mlua::{Lua, Scope, Table};
use std::cell::RefCell;

pub(super) fn install_syntax_api<'lua, 'scope>(
    lua: &'lua Lua,
    scope: &'lua Scope<'lua, 'scope>,
    g: &Table,
    cell: &'scope RefCell<Option<Editor>>,
) -> mlua::Result<()> {
    let syntax_table = lua.create_table()?;
    syntax_table.set(
        "register",
        scope.create_function(|_, (name, spec): (String, Table)| {
            let def = parse_syntax_def(name, &spec)?;
            mutate_editor(cell, |ed| ed.with_syntax_registry(|reg| reg.register(def)));
            Ok(())
        })?,
    )?;
    g.set("syntax", syntax_table)?;
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

fn parse_syntax_def(name: String, spec: &Table) -> mlua::Result<SyntaxDef> {
    if name.trim().is_empty() {
        return Err(mlua::Error::RuntimeError(
            "syntax name must not be empty".to_string(),
        ));
    }

    let extensions = parse_string_list(spec, "extensions")?;
    let filenames = parse_string_list(spec, "filenames")?;
    if extensions.is_empty() && filenames.is_empty() {
        return Err(mlua::Error::RuntimeError(
            "syntax spec must have at least one extension or filename".to_string(),
        ));
    }

    let line_comment: Option<String> = spec.get("line_comment").ok();
    let keywords = parse_string_list(spec, "keywords")?;
    let types = parse_string_list(spec, "types")?;
    let case_insensitive: bool = spec.get("case_insensitive").unwrap_or(false);
    let heading_prefix: Option<String> = spec
        .get("heading")
        .map_err(|_| mlua::Error::RuntimeError("heading must be a string".to_string()))?;
    if heading_prefix.as_deref() == Some("") {
        return Err(mlua::Error::RuntimeError(
            "heading must not be empty".to_string(),
        ));
    }

    Ok(SyntaxDef::new(
        name,
        extensions,
        filenames,
        line_comment,
        keywords,
        types,
        case_insensitive,
    )
    .with_heading_prefix(heading_prefix))
}
