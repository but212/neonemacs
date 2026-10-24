# ADR 0008: Lua Paragraph and Sentence Motion, Marking, and Killing

- Status: accepted
- Date: 2026-10-25
- Spec: `docs/spec.md` §6, §12

## Context

Following ADR 0007, NeonEmacs supported word and buffer navigation along with line-level whitespace manipulation in `src/shell/text.lua`. However, higher-level text navigation and destruction primitives standard in GNU Emacs—specifically paragraph motion (`M-}`, `M-{`), paragraph marking (`M-h`), sentence motion (`M-e`, `M-a`), and sentence killing (`M-k`)—were not yet available. Furthermore, setting buffer marks directly from Lua extensions was not supported by the FFI bridge.

## Decision

1. **Expose minimal buffer mark FFI in `src/shell/lua.rs`:**
   - `buffer.mark()`: Returns `Option<usize>` representing the current buffer mark or `nil`.
   - `buffer.set_mark(pos)`: Accepts `Option<usize>` to update or clear the active buffer mark.
2. **Implement paragraph and sentence operations in `src/shell/text.lua`:**
   - `forward-paragraph` (`M-}`): Advances point to the start of the next blank line or buffer end.
   - `backward-paragraph` (`M-{`): Moves point back to the start of the current or preceding paragraph.
   - `mark-paragraph` (`M-h`): Positions point at paragraph start and mark at paragraph end (including newline).
   - `forward-sentence` (`M-e`): Advances point to the end of the next sentence (following punctuation and closing delimiters).
   - `backward-sentence` (`M-a`): Moves point back to the start of the current or preceding sentence.
   - `kill-sentence` (`M-k`): Kills text from point to end of sentence and trailing spaces on the same line, preserving newlines.
   - Reused existing `kill-word` (`M-d`) and `backward-kill-word` (`M-DEL`).
3. **Sentence boundary definition:**
   - Punctuation (`.`, `?`, `!`) followed by optional closing delimiters (`'`, `"`, `)`, `]`, `}`) and at least one whitespace character, newline, or EOF.
4. **Key bindings in `src/shell/keymap.lua`:**
   - Bind `M-}`, `M-{`, `M-h`, `M-e`, `M-a`, and `M-k`.
5. **Architectural purity & invariants:**
   - Upholds core purity (I-18); zero mutation in Rust core.
   - Preserves continuous kill-ring appending (I-20) across consecutive kills.

## Consequences

- Complete coverage of Emacs structural navigation and killing above word level.
- Clean Lua-accessible mark manipulation enabling further region/selection utilities.
- Single-step undo atomicity preserved across paragraph and sentence operations.
