//! I-18: no `&mut` in a `neonemacs-core` signature (parameters, return types, `&mut self`, type
//! aliases, fields). `let mut` and `&mut` arguments inside bodies are allowed. Rust has no
//! type-level ban on `&mut` in a signature, so this `syn` AST scan is the enforcement
//! (docs/context.md §4.1).
//!
//! I-14: no I/O crate in `neonemacs-core` (`std::fs`, `std::process`, `mlua`, `crossterm`). The
//! scan parses source AST, so it catches multi-line signatures and grouped `use` trees, though
//! it still does not expand macros.

use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::{self, Visit};

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

struct MutSigVisitor {
    hits: Vec<String>,
}

impl MutSigVisitor {
    fn check_source(source: &str) -> Vec<String> {
        let syntax = syn::parse_file(source).expect("valid Rust source file");
        let mut visitor = Self { hits: Vec::new() };
        visitor.visit_file(&syntax);
        visitor.hits
    }
}

impl<'ast> Visit<'ast> for MutSigVisitor {
    fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
        self.visit_signature(&i.sig);
    }

    fn visit_impl_item_fn(&mut self, i: &'ast syn::ImplItemFn) {
        self.visit_signature(&i.sig);
    }

    fn visit_trait_item_fn(&mut self, i: &'ast syn::TraitItemFn) {
        self.visit_signature(&i.sig);
    }

    fn visit_item_type(&mut self, i: &'ast syn::ItemType) {
        self.visit_type(&i.ty);
    }

    fn visit_field(&mut self, i: &'ast syn::Field) {
        self.visit_type(&i.ty);
    }

    fn visit_receiver(&mut self, i: &'ast syn::Receiver) {
        if i.reference.is_some() && i.mutability.is_some() {
            self.hits.push("&mut self".to_owned());
        }
    }

    fn visit_type_reference(&mut self, i: &'ast syn::TypeReference) {
        if i.mutability.is_some() {
            self.hits.push("&mut".to_owned());
        }
        visit::visit_type_reference(self, i);
    }
}

#[test]
fn core_has_no_mut_signatures() {
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for file in rust_files(&core) {
        let source = fs::read_to_string(&file).expect("readable source file");
        for hit in MutSigVisitor::check_source(&source) {
            hits.push(format!("{}: {hit}", file.display()));
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
    let has_mut = |src: &str| !MutSigVisitor::check_source(src).is_empty();
    assert!(has_mut("pub fn step(ed: &mut Editor) {}"));
    assert!(has_mut(
        "pub fn multi_line(\n    a: usize,\n    out: &mut Vec<Rect>,\n) {}"
    ));
    assert!(has_mut("impl Foo { pub fn scroll(&mut self) {} }"));
    assert!(has_mut("pub type Builtin = fn(&mut Editor);"));
    assert!(has_mut("fn f() -> &mut Text { loop {} }"));
    assert!(has_mut("struct S { buf: &'static mut [u8] }"));
    assert!(!has_mut("fn f(mut ed: Editor) { let r = &mut ed; }"));
    assert!(!has_mut(
        "fn f() { let x = std::mem::take(&mut buf); let mut t = Text::default(); }"
    ));
}

struct IoImportVisitor {
    hits: Vec<String>,
}

impl IoImportVisitor {
    fn check_source(source: &str) -> Vec<String> {
        let syntax = syn::parse_file(source).expect("valid Rust source file");
        let mut visitor = Self { hits: Vec::new() };
        visitor.visit_file(&syntax);
        visitor.hits
    }

    fn is_forbidden(segments: &[String]) -> bool {
        matches!(
            segments.first().map(String::as_str),
            Some("mlua" | "crossterm")
        ) || (segments.first().map(String::as_str) == Some("std")
            && matches!(segments.get(1).map(String::as_str), Some("fs" | "process")))
    }

    fn walk_use_tree(&mut self, prefix: &mut Vec<String>, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                if Self::is_forbidden(prefix) {
                    self.hits.push(prefix.join("::"));
                } else {
                    self.walk_use_tree(prefix, &p.tree);
                }
                prefix.pop();
            }
            syn::UseTree::Name(n) => {
                prefix.push(n.ident.to_string());
                if Self::is_forbidden(prefix) {
                    self.hits.push(prefix.join("::"));
                }
                prefix.pop();
            }
            syn::UseTree::Rename(r) => {
                prefix.push(r.ident.to_string());
                if Self::is_forbidden(prefix) {
                    self.hits.push(prefix.join("::"));
                }
                prefix.pop();
            }
            syn::UseTree::Group(g) => {
                for item in &g.items {
                    self.walk_use_tree(prefix, item);
                }
            }
            syn::UseTree::Glob(_) => {}
        }
    }
}

impl<'ast> Visit<'ast> for IoImportVisitor {
    fn visit_item_use(&mut self, i: &'ast syn::ItemUse) {
        let mut prefix = Vec::new();
        self.walk_use_tree(&mut prefix, &i.tree);
    }

    fn visit_path(&mut self, i: &'ast syn::Path) {
        let segs: Vec<String> = i.segments.iter().map(|s| s.ident.to_string()).collect();
        if Self::is_forbidden(&segs) {
            self.hits.push(segs.join("::"));
        }
        visit::visit_path(self, i);
    }
}

#[test]
fn core_imports_no_io_crate() {
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for file in rust_files(&core) {
        let source = fs::read_to_string(&file).expect("readable source file");
        for hit in IoImportVisitor::check_source(&source) {
            hits.push(format!("{}: {hit}", file.display()));
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
    let has_io = |src: &str| !IoImportVisitor::check_source(src).is_empty();
    assert!(has_io("use std::fs;"));
    assert!(has_io("use std::fs::File;"));
    assert!(has_io("use std::{\n    io,\n    fs,\n    path::Path,\n};"));
    assert!(has_io("use std::{io, process};"));
    assert!(has_io(
        "fn f() { let _ = std::process::Command::new(\"ls\"); }"
    ));
    assert!(has_io("use mlua::Lua;"));
    assert!(has_io("use crossterm::event::KeyCode;"));
    assert!(!has_io("// use std::fs;\nfn f() {}"));
    assert!(!has_io(
        "/// Reads through `std::fs` in the shell.\nfn f() {}"
    ));
    assert!(!has_io("use std::collections::BTreeMap;"));
    assert!(!has_io("use std::path::{Path, PathBuf};"));
    assert!(!has_io("fn f() { let filesystem = 1; }"));
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
