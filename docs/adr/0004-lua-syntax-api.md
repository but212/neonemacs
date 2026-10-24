# ADR 0004: Lua syntax registration API

- Status: accepted
- Date: 2026-10-25
- Spec: `docs/spec.md` §5, §12

## Context

ADR 0001 kept the public Lua API to five functions (`bind`, `command`, `run`, `get`, `set`). Syntax highlighting keywords, extensions, and comment styles were hardcoded as static constants inside `src/core/syntax.rs`. Adding or customizing language highlighting required recompiling the Rust binary, and bloated `src/core/` with language tables.

Directly executing Lua code per line during redisplay would violate core purity (ADR 0002, I-14) and harm rendering performance.

## Decision

- Provide a namespaced Lua module `syntax` with function `syntax.register(name: string, spec: table)`.
- The `spec` table schema:
  - `extensions`: non-empty array of file extension strings (e.g. `{"rs"}`).
  - `line_comment`: optional comment prefix string (e.g. `"//"`).
  - `keywords`: optional array of keyword strings (e.g. `{"fn", "let"}`).
- The registration is purely declarative: Lua registers definitions into the editor's pure `SyntaxRegistry` (`Editor::with_syntax_registry`).
- Screen rendering continues to execute pure Rust 1-pass zero-allocation tokenization against the registry (I-22).
- Default language definitions (Rust, C, Python, JavaScript, Lua, Go, Shell, Markdown) are stored in `src/shell/syntax.lua` and embedded into the binary via `include_str!`.
- Embedded syntax definitions are evaluated at startup before user `init.lua`, allowing user scripts to overwrite or append language rules.

## Error contract

- `syntax.register` raises a Lua error if `name` is empty, `extensions` is missing or not a string array, or `keywords` contains non-string elements.
- A failed registration changes nothing in the registry.

## Scope

- Not public: AST-level parsing, multi-line block comments, regex engines. The lightweight 1-pass tokenizer contract (I-22) is preserved.

## Consequences

- All static language arrays in `src/core/syntax.rs` are eliminated.
- Users can define custom language syntax highlighting directly in `init.lua`.
- Functional core purity (I-14, I-18) and redisplay speed are preserved.
