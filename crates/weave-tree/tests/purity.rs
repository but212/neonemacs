//! I-18: no `&mut` in a `weave-tree` signature (parameters, return types, `&mut self`, type
//! aliases, fields). `let mut` and `&mut` arguments inside bodies are allowed.
//!
//! I-14: no I/O crate in `weave-tree` (`std::fs`, `std::process`, `mlua`, `crossterm`).
//!
//! I-59: top-level `weave-tree` module references are strictly acyclic.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).expect("crates/weave-tree/src exists") {
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
fn weave_tree_has_no_mut_signatures() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for file in rust_files(&src) {
        let source = fs::read_to_string(&file).expect("readable source file");
        for (n, line) in source.lines().enumerate() {
            if has_signature_mut(line) {
                hits.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "I-18: &mut in a weave-tree signature:\n{}",
        hits.join("\n")
    );
}

#[test]
fn scanner_tells_signatures_from_bodies() {
    assert!(has_signature_mut(
        "pub fn seek_byte(&mut self, off: usize) {"
    ));
    assert!(has_signature_mut("    stack: &mut Vec<CursorFrame>,"));
    assert!(has_signature_mut("fn f() -> &mut [u8] {"));
    assert!(!has_signature_mut(
        "    let Some(node) = Arc::get_mut(&mut entry.node) else {"
    ));
    assert!(!has_signature_mut("    let mut acc = 0usize;"));
}

/// True when the line names an I/O crate or module that `crates/weave-tree/src` must not use.
fn has_io_import(line: &str) -> bool {
    let code = line.split("//").next().unwrap_or("");
    if ["std::fs", "std::process", "mlua", "crossterm"]
        .iter()
        .any(|path| has_word(code, path))
    {
        return true;
    }
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
fn weave_tree_imports_no_io_crate() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for file in rust_files(&src) {
        let source = fs::read_to_string(&file).expect("readable source file");
        for (n, line) in source.lines().enumerate() {
            if has_io_import(line) {
                hits.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "I-14: I/O import in weave-tree:\n{}",
        hits.join("\n")
    );
}

#[test]
fn io_scanner_tells_imports_from_comments() {
    assert!(has_io_import("use std::fs;"));
    assert!(has_io_import("use std::{fs, path::Path};"));
    assert!(has_io_import("use mlua::Lua;"));
    assert!(has_io_import("use crossterm::event::KeyCode;"));
    assert!(!has_io_import("// use std::fs;"));
    assert!(!has_io_import("use core::ops::Range;"));
}

/// I-59: the `crate::<module>` references between top-level `weave-tree` modules.
fn module_edges(src: &Path) -> BTreeSet<(String, String)> {
    let mut edges = BTreeSet::new();
    for file in rust_files(src) {
        let rel = file
            .strip_prefix(src)
            .expect("file is under weave-tree/src");
        let top = rel
            .components()
            .next()
            .expect("non-empty path")
            .as_os_str()
            .to_string_lossy()
            .trim_end_matches(".rs")
            .to_owned();
        if top == "lib" {
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
fn mutual_pairs(edges: &BTreeSet<(String, String)>) -> BTreeSet<(String, String)> {
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
fn weave_tree_modules_form_no_cycle() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let cycles = mutual_pairs(&module_edges(&src));
    assert!(
        cycles.is_empty(),
        "I-59: module cycles in weave-tree: {cycles:?}"
    );
}
