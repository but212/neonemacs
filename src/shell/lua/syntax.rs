//! Lua `syntax.register`: declares syntax definitions from Lua.

use super::mutate_editor;
use crate::core::Editor;
use crate::core::SyntaxDef;
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

#[cfg(test)]
mod tests {
    use super::super::{bootstrap, new_state, test_script as script};
    use crate::core::Config;
    use crate::core::Editor;
    use crate::core::SyntaxRegistry;

    fn lang_of<'a>(reg: &'a SyntaxRegistry, file: &str) -> Option<&'a str> {
        reg.find_by_filename(file).map(|d| d.name.as_str())
    }

    fn keyword_in(reg: &SyntaxRegistry, file: &str, word: &str) -> bool {
        reg.find_by_filename(file)
            .is_some_and(|d| d.is_keyword(word))
    }

    fn type_in(reg: &SyntaxRegistry, file: &str, word: &str) -> bool {
        reg.find_by_filename(file).is_some_and(|d| d.is_type(word))
    }

    fn comment_in<'a>(reg: &'a SyntaxRegistry, file: &str) -> Option<&'a str> {
        reg.line_comment_for_filename(file)
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
        assert_eq!(lang_of(ed.syntax_registry(), "file.xyz"), Some("mylang"));
        assert!(keyword_in(ed.syntax_registry(), "file.xyz", "fn"));
        assert_eq!(comment_in(ed.syntax_registry(), "file.xyz"), Some("#"));

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
            r#"syntax.register("sql", {
                extensions = { "sql" },
                filenames = { "query.sql.txt" },
                case_insensitive = true,
                keywords = { "select" },
                types = { "varchar" }
            })"#,
        );
        result.unwrap();
        assert_eq!(lang_of(ed.syntax_registry(), "query.sql.txt"), Some("sql"));
        assert!(keyword_in(ed.syntax_registry(), "query.sql.txt", "SELECT"));
        assert!(type_in(ed.syntax_registry(), "query.sql.txt", "VARCHAR"));
    }

    #[test]
    fn heading_is_a_syntax_field_and_bundled_markdown_uses_it() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let md = ed.syntax_registry().find_by_filename("README.md").unwrap();
        assert_eq!(md.heading_prefix.as_deref(), Some("#"));
        let (ed, result) = script(
            &lua,
            ed,
            r#"syntax.register("markdown", { extensions = { "md" } })"#,
        );
        result.unwrap();
        let md = ed.syntax_registry().find_by_filename("README.md").unwrap();
        assert_eq!(md.heading_prefix, None);
        let (_, result) = script(
            &lua,
            ed,
            r#"syntax.register("x", { extensions = { "x" }, heading = "" })"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn bundled_syntax_loaded_by_default() {
        let lua = new_state().unwrap();
        let ed = bootstrap(Editor::new(Config::default()), &lua);
        let reg = ed.syntax_registry();
        assert_eq!(lang_of(reg, "main.rs"), Some("rust"));
        assert_eq!(lang_of(reg, "script.py"), Some("python"));
        assert_eq!(lang_of(reg, "index.js"), Some("javascript"));
        assert_eq!(lang_of(reg, "config.lua"), Some("lua"));
        assert_eq!(lang_of(reg, "app.go"), Some("go"));
        assert_eq!(lang_of(reg, "build.sh"), Some("shell"));
        assert_eq!(lang_of(reg, "README.md"), Some("markdown"));
        assert_eq!(lang_of(reg, "main.c"), Some("c"));
        assert_eq!(lang_of(reg, "Makefile"), Some("makefile"));
        assert_eq!(lang_of(reg, "Cargo.toml"), Some("toml"));
        assert_eq!(lang_of(reg, "query.sql"), Some("sql"));
        assert!(type_in(reg, "main.rs", "i32"));
        assert!(keyword_in(reg, "query.sql", "SELECT"));
        assert!(type_in(reg, "query.sql", "VARCHAR"));
        assert!(keyword_in(reg, "build.sh", "if"));
    }
}
