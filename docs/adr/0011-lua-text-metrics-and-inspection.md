# ADR 0011: Lua Text Metrics and Cursor Inspection

- Status: accepted
- Date: 2026-10-25
- Spec: `docs/spec.md` §6, §12

## Context
NeonEmacs delegates user-facing editing operations and commands to Lua (`src/shell/text.lua`). Inspection and text statistics commands were missing:
1. Counting lines, words, and characters in a region or buffer (`count-words-region`, `M-=`, and `count-words`, `M-x`).
2. Detailed cursor inspection at point (`what-cursor-position`, `C-x =`).
3. Reporting current line number (`what-line`, `M-x what-line`).

While line and column calculation could theoretically fetch text chunks, doing line counting across huge buffers in pure Lua would allocate large strings. Conversely, Rust `Text` already implements a SIMD-accelerated `count_lines(start, end)` using `memchr`.

## Decision
1. **Thin Rust FFI Surface (`src/shell/lua.rs`)**:
   - Exposed `buffer.count_lines(start, end)` wrapping `ed.buffer().text.count_lines(s, e)` with zero allocations.
2. **Commands in Section 9 of `src/shell/text.lua`**:
   - `count-words-region` / `count-words`: DWIM behavior. If mark is active, calculates metrics for the active region (`Region has ...`); otherwise for the entire buffer (`Buffer has ...`). Pluralization matches Emacs standards (`1 line` vs `N lines`, `1 word` vs `N words`, `1 character` vs `N characters`). Lines are counted with Emacs semantics (empty = 0 lines, otherwise newline count + 1 for unterminated trailing line).
   - `what-cursor-position`: Modern format reporting `Char: c (DEC, 0xHEX) point=P of TOTAL (PCT%) col=C` using 0-based index. At EOF (`pt >= total`), cleanly formats as `point=TOTAL of TOTAL (100%) col=C (EOF)`. UTF-8 characters and control chars (`\n`, `\t`) are decoded cleanly without partial-byte errors.
   - `what-line`: Reports `Line L` (1-based), calculated as `buffer.count_lines(0, pt) + 1`.
3. **Keybindings in `src/shell/keymap.lua`**:
   - `M-=` -> `"count-words-region"`
   - `C-x =` -> `"what-cursor-position"`
   - `count-words` and `what-line` exposed to `M-x`.

## Consequences
- Preserves the pure functional core (zero `&mut` in core signatures, I-18 preserved).
- High performance line counting via Rust SIMD memchr while keeping business logic and formatting declarative in Lua.
- All new commands are registered and discoverable via `M-x` TAB completion.
