//! I-18: no `&mut` in a `neonemacs-core` signature (parameters, return types, `&mut self`, type
//! aliases, fields). `let mut` and `&mut` arguments inside bodies are allowed. Rust has no
//! type-level ban on `&mut` in a signature, so this text scan is the enforcement
//! (docs/context.md §4.1).
//!
//! I-14: no I/O crate in `neonemacs-core` (`std::fs`, `std::process`, `mlua`, `crossterm`). The
//! scan reads source text, so it does not see a path reached through a macro or a re-export.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).expect("crates/neonemacs-core/src exists") {
        let path = entry.expect("readable directory entry").path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files
}

/// True when the line puts `&mut` in a signature position.
fn has_signature_mut(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("let ") {
        return false;
    }
    let Some(at) = line.find("&mut") else {
        return false;
    };
    let head = &line[..at];
    head.contains(": ")
        || head.trim_end().ends_with(':')
        || head.contains("->")
        || head.contains("fn ")
        || head.contains("fn(")
        || line.contains("&mut self")
}

#[test]
fn core_has_no_mut_signatures() {
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for file in rust_files(&core) {
        let source = fs::read_to_string(&file).expect("readable source file");
        for (n, line) in source.lines().enumerate() {
            if has_signature_mut(line) {
                hits.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "I-18: &mut in a core signature:\n{}",
        hits.join("\n")
    );
}

#[test]
fn scanner_tells_signatures_from_bodies() {
    assert!(has_signature_mut("pub fn step(ed: &mut Editor) {"));
    assert!(has_signature_mut("    out: &mut Vec<Rect>,"));
    assert!(has_signature_mut("pub fn scroll(&mut self) {"));
    assert!(has_signature_mut("pub type Builtin = fn(&mut Editor);"));
    assert!(has_signature_mut("fn f() -> &mut Text {"));
    assert!(!has_signature_mut("    let r = &mut buf;"));
    assert!(!has_signature_mut("    let x = std::mem::take(&mut buf);"));
    assert!(!has_signature_mut("    let mut t = Text::default();"));
}

/// True when the line names an I/O crate or module that `crates/neonemacs-core/src` must not use.
fn has_io_import(line: &str) -> bool {
    let code = line.split("//").next().unwrap_or("");
    if ["std::fs", "std::process", "mlua", "crossterm"]
        .iter()
        .any(|path| has_word(code, path))
    {
        return true;
    }
    // `use std::{fs, process};` names the module without the `std::fs` path.
    code.split_once("std::{")
        .is_some_and(|(_, group)| has_word(group, "fs") || has_word(group, "process"))
}

/// True when `word` occurs in `text` with no identifier character on either side.
fn has_word(text: &str, word: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

#[test]
fn core_imports_no_io_crate() {
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for file in rust_files(&core) {
        let source = fs::read_to_string(&file).expect("readable source file");
        for (n, line) in source.lines().enumerate() {
            if has_io_import(line) {
                hits.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "I-14: I/O import in core:\n{}",
        hits.join("\n")
    );
}

#[test]
fn io_scanner_tells_imports_from_comments() {
    assert!(has_io_import("use std::fs;"));
    assert!(has_io_import("use std::fs::File;"));
    assert!(has_io_import("use std::{fs, path::Path};"));
    assert!(has_io_import("use std::{io, process};"));
    assert!(has_io_import(
        "let out = std::process::Command::new(\"ls\");"
    ));
    assert!(has_io_import("use mlua::Lua;"));
    assert!(has_io_import("use crossterm::event::KeyCode;"));
    assert!(!has_io_import("// use std::fs;"));
    assert!(!has_io_import("/// Reads through `std::fs` in the shell."));
    assert!(!has_io_import("use std::collections::BTreeMap;"));
    assert!(!has_io_import("use std::path::{Path, PathBuf};"));
    assert!(!has_io_import("let filesystem = 1;"));
}

/// I-59: the `crate::<module>` references between top-level core modules. Test code (everything from
/// the first `#[cfg(test)]` on) and `lib.rs`, the root that declares the modules, are left out.
fn module_edges(core: &Path) -> std::collections::BTreeSet<(String, String)> {
    let mut edges = std::collections::BTreeSet::new();
    for file in rust_files(core) {
        let rel = file.strip_prefix(core).expect("file is under core/src");
        let top = rel
            .components()
            .next()
            .expect("non-empty path")
            .as_os_str()
            .to_string_lossy()
            .trim_end_matches(".rs")
            .to_owned();
        if matches!(top.as_str(), "lib" | "test_support" | "scenarios") {
            continue;
        }
        let source = fs::read_to_string(&file).expect("readable source file");
        let production = source.split("#[cfg(test)]").next().unwrap_or("");
        for line in production.lines() {
            let code = line.split("//").next().unwrap_or("");
            for (at, _) in code.match_indices("crate::") {
                let name: String = code[at + "crate::".len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || *c == '_')
                    .collect();
                if !name.is_empty() && name != top {
                    edges.insert((top.clone(), name));
                }
            }
        }
    }
    edges
}

/// The pairs of modules that reach each other.
fn mutual_pairs(
    edges: &std::collections::BTreeSet<(String, String)>,
) -> std::collections::BTreeSet<(String, String)> {
    let mut reach = edges.clone();
    loop {
        let more: Vec<_> = reach
            .iter()
            .flat_map(|(a, b)| {
                reach
                    .iter()
                    .filter(move |(c, _)| c == b)
                    .map(move |(_, d)| (a.clone(), d.clone()))
            })
            .filter(|pair| !reach.contains(pair))
            .collect();
        if more.is_empty() {
            break;
        }
        reach.extend(more);
    }
    reach
        .iter()
        .filter(|(a, b)| a < b && reach.contains(&(b.clone(), a.clone())))
        .cloned()
        .collect()
}

#[test]
fn core_modules_form_no_cycle_but_commands_and_dispatch() {
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let cycles = mutual_pairs(&module_edges(&core));
    let accepted =
        std::collections::BTreeSet::from([("commands".to_owned(), "dispatch".to_owned())]);
    assert_eq!(cycles, accepted, "I-59: module cycles in core");
}

#[test]
fn cycle_finder_sees_indirect_cycles() {
    let edge = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    let edges = std::collections::BTreeSet::from([edge("a", "b"), edge("b", "c"), edge("c", "a")]);
    let pairs = mutual_pairs(&edges);
    assert_eq!(pairs.len(), 3);
    let line = std::collections::BTreeSet::from([edge("a", "b"), edge("b", "c")]);
    assert!(mutual_pairs(&line).is_empty());
}
