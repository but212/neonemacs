# ADR 0009: Lua Comment DWIM and M-x Command Completion Integration

- Status: accepted
- Date: 2026-10-25
- Spec: `docs/spec.md` §4, §6, §12

## Context

Following the migration of text transformations, whitespace handling, and structural paragraph/sentence motions to Lua (`text.lua`), two key architectural and usability gaps remained:
1. `M-x` TAB completion in `src/core/commands/prompt.rs` only queried the static Rust `BUILTINS` table. Consequently, commands registered from Lua via `command(...)` were callable via keybindings or exact typed names, but were invisible in `M-x` candidate lists and TAB completion.
2. Code commenting (`comment-dwim` / `comment-line`, `M-;`), an essential editor editing capability, was absent. `syntax.lua` already registered `line_comment` syntax strings (`//`, `#`, `--`, etc.) across languages, but there was no FFI bridge to query the active buffer's comment syntax from Lua or display transient echo messages without raising runtime errors.

## Decision

1. **Expose Core Command Registry & Unify `M-x` Completion:**
   - Added `commands: BTreeSet<String>` to `Editor` in `src/core/editor.rs` with `register_command` and `registered_commands`.
   - Updated `src/shell/lua.rs` so `command(name, f)` registers the command into `Editor::register_command`.
   - Updated `prompt::completions` in `src/core/commands/prompt.rs` to merge `BUILTINS` and `ed.registered_commands()` into a single deduplicated, alphabetically sorted list with matching keybinding representations.
2. **Expose `buffer.comment_prefix` and `message` FFI:**
   - `buffer.comment_prefix()`: Detects language from buffer name via `SyntaxRegistry::detect_lang(&buffer.name)` and queries `line_comment`, returning `Option<String>`.
   - `message(msg)`: Displays transient echo messages in `Editor::message` without raising runtime errors.
3. **Implement Indent-Aware `comment-dwim` and `comment-line` in `src/shell/text.lua`:**
   - Modern line-toggle behavior: If no region is active, toggles commenting on the current line; if a region is active, toggles all lines spanning the region.
   - Indent-aware placement: Inserts prefix and space after line indentation (`    // code`). Removes prefix and single trailing space on uncomment.
   - Idempotency & reverse-order replacement: Replaces lines in bottom-to-top order to preserve buffer offsets of earlier lines.
   - Fallback protection: Displays `"No comment syntax defined for buffer"` when `comment_prefix()` is nil.
4. **Keybinding in `src/shell/keymap.lua`:**
   - Bound `M-;` to `"comment-dwim"`. Registered `"comment-line"` as an alias.
5. **Architectural Purity & Invariants:**
   - `src/core` remains 100% pure (I-18); no Lua or mlua references in core.
   - All string allocations in core use standard Rust collections (`BTreeSet<String>`).

## Consequences

- All Lua-registered commands now have first-class discoverability in `M-x` TAB completion.
- Seamless, language-aware comment toggling for all supported syntax definitions.
- Safe non-destructive behavior for non-commentable buffers (Markdown, plain text).
