# ADR 0006: Lua Buffer API and Bundled Text Editing Commands

- Status: accepted
- Date: 2026-10-25
- Spec: `docs/spec.md` §6, §12

## Context

Prior to this ADR, NeonEmacs lacked standard Emacs word transformation commands (`upcase-word`, `downcase-word`, `capitalize-word`), whitespace manipulation commands (`just-one-space`, `delete-horizontal-space`), and word deletion commands (`kill-word`, `backward-kill-word`). Furthermore, `fill-paragraph` (`M-q`) was hardcoded directly in Rust core (`src/core/commands/edit.rs`), along with single-purpose paragraph boundary and formatting helpers in `src/core/commands/util.rs`.

Lua had no buffer inspection or mutation primitives, only allowing key binding, configuration get/set, and syntax registration.

## Decision

1. **Expose `buffer` module in Lua (§12):**
   - Inspection: `buffer.point()`, `buffer.len()`, `buffer.get_text(start, end)`, `buffer.line_start(pos)`, `buffer.line_end(pos)`, `buffer.width(text)`.
   - Mutation: `buffer.set_point(pos)`, `buffer.insert(pos, text)`, `buffer.delete(start, end)`, `buffer.kill(start, end)`, `buffer.replace(start, end, text)`.
   - All positions use 0-based UTF-8 byte offsets matching Rust `Text`. Any cursor repositioning automatically snaps to valid unit boundaries (I-01).
   - `buffer.kill(start, end)` deletes text and pushes it to the kill ring, merging consecutive kills into a single kill ring entry (I-09).
2. **Bundle `src/shell/text.lua` (§6, §12):**
   - Implement 8 standard text transformation commands in Lua:
     - `fill-paragraph` (`M-q`): formats paragraph respecting `fill-column`, display width, tab width, and first-line indentation.
     - `upcase-word` (`M-u`): converts from point to end of word to uppercase.
     - `downcase-word` (`M-l`): converts from point to end of word to lowercase.
     - `capitalize-word` (`M-c`): capitalizes current word.
     - `kill-word` (`M-d`): kills to end of word.
     - `backward-kill-word` (`M-DEL`): kills backward to start of word.
     - `just-one-space` (`M-SPC`): collapses surrounding horizontal whitespace into a single space.
     - `delete-horizontal-space` (`M-\`): deletes all surrounding horizontal whitespace.
   - Bundle `text.lua` via `include_str!` into `BUNDLED_SCRIPTS` evaluated during bootstrap before `keymap.lua`.
3. **Core cleanup:**
   - Remove `fill_paragraph` from `src/core/commands/edit.rs` and `commands::BUILTINS`.
   - Remove single-use paragraph and space measurement helpers from `src/core/commands/util.rs`.
   - Add pure `Editor::kill_range`, `Editor::delete_range`, and `Editor::insert_at` methods returning `Self` (I-18).
4. **Keymap bindings:**
   - Add bindings for `M-u`, `M-l`, `M-c`, `M-d`, `M-DEL`, `M-SPC`, and `M-\` to `src/shell/keymap.lua`.
   - Update keymap integrity tests to validate commands against either Rust `BUILTINS` or registered Lua commands.

## Consequences

- Rust core remains pure, minimal, and completely free of text formatting heuristics.
- Users can customize, override, or extend word navigation and paragraph formatting logic directly in `init.lua`.
- Single-binary packaging (< 5MB) and zero-dependency runtime remain intact.
