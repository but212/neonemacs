//! I-18: no `&mut` in a `src/core` signature (parameters, return types, `&mut self`, type aliases,
//! fields). `let mut` and `&mut` arguments inside bodies are allowed. Rust has no type-level ban on
//! `&mut` in a signature, so this text scan is the enforcement (ADR 0002).

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).expect("src/core exists") {
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
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/core");
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
