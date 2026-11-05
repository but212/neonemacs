//! Lua `answer_alias` and `answer_unalias`: which characters answer y/n/q/! (I-46, I-48).

use super::mutate_editor;
use crate::core::editor::Editor;
use crate::core::keymap::AnswerKey;
use mlua::{Scope, Table};
use std::cell::RefCell;

/// The one character in `s`.
fn single_char(s: &str) -> mlua::Result<char> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(mlua::Error::RuntimeError(format!(
            "answer alias must be one character: '{s}'"
        ))),
    }
}

pub(super) fn install_answer_api<'lua, 'scope>(
    scope: &'lua Scope<'lua, 'scope>,
    g: &Table,
    cell: &'scope RefCell<Option<Editor>>,
) -> mlua::Result<()> {
    g.set(
        "answer_alias",
        scope.create_function(|_, (from, to): (String, String)| {
            let from = single_char(&from)?;
            let to = AnswerKey::parse(&to).map_err(mlua::Error::RuntimeError)?;
            mutate_editor(cell, |ed| ed.with_answer_aliases(|a| a.alias(from, to)));
            Ok(())
        })?,
    )?;
    g.set(
        "answer_unalias",
        scope.create_function(|_, from: String| {
            let from = single_char(&from)?;
            mutate_editor(cell, |ed| ed.with_answer_aliases(|a| a.unalias(from)));
            Ok(())
        })?,
    )?;
    Ok(())
}
