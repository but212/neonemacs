# ADR 0007: Lua Text Movement and Advanced Editing Commands

- Status: accepted
- Date: 2026-10-25
- Spec: `docs/spec.md` §6, §12

## Context

Following ADR 0006, NeonEmacs possessed a stable `buffer` Lua API and bundled `text.lua` for text transformation and word killing. However, standard Emacs word and buffer navigation commands (`forward-word`, `backward-word`, `beginning-of-buffer`, `end-of-buffer`) and common editing commands (`open-line`, `delete-indentation` / `join-line`, `transpose-chars`, `transpose-words`) were not yet implemented.

## Decision

1. **Implement movement and advanced editing utilities in `src/shell/text.lua`:**
   - `forward-word` (`M-f`): moves point forward to the end of the next word.
   - `backward-word` (`M-b`): moves point backward to the start of the previous word.
   - `beginning-of-buffer` (`M-<`): moves point to byte offset 0.
   - `end-of-buffer` (`M->`): moves point to the end of the buffer (`buffer.len()`).
   - `open-line` (`C-o`): inserts `\n` at the current point without moving the point.
   - `delete-indentation` / `join-line` (`M-^`): joins the current line to the previous line, removing whitespace around the join point and replacing it with a single space. Registered under both `delete-indentation` and `join-line`.
   - `transpose-chars` (`C-t`): swaps adjacent characters around point with UTF-8 codepoint boundary safety. At EOL/EOB, swaps the two characters preceding the point.
   - `transpose-words` (`M-t`): swaps adjacent words around point, maintaining whitespace separator and landing point at the end of the second word.
2. **UTF-8 line-bounded character manipulation:**
   - Character transposition operations operate safely within line boundaries to avoid partial multibyte slicing and preserve invariant I-01 / I-14.
3. **Key bindings in `src/shell/keymap.lua`:**
   - Bind `M-f`, `M-b`, `M-<`, `M->`, `C-o`, `M-^`, `C-t`, and `M-t`.
4. **Zero changes to Rust core signatures:**
   - All logic utilizes existing `buffer` methods, maintaining pure Functional Core / Imperative Shell architecture (I-18).

## Consequences

- Full coverage of standard Emacs word/buffer movement and structural transposition without increasing core binary complexity.
- All composite mutations execute inside editor undo groupings, providing single-step undo atomicity.
