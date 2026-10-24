# ADR 0010: Lua Buffer and Region Operations

- Status: accepted
- Date: 2026-10-25
- Spec: `docs/spec.md` §6, §12

## Context
NeonEmacs delegates high-level text transformation and editing commands to Lua (`src/shell/text.lua`) on top of a pure functional Rust core. While word, sentence, paragraph, transposition, and commenting operations were implemented in Lua, several foundational GNU Emacs buffer and region operations remained absent:
1. Whole buffer marking (`mark-whole-buffer`, `C-x h`).
2. Blank line cleanup and compression (`delete-blank-lines`, `C-x C-o`).
3. Active region case conversions (`upcase-region`, `C-x C-u` and `downcase-region`, `C-x C-l`).
4. Whole line killing (`kill-whole-line`, `M-x kill-whole-line`).

## Decision
1. **Implement in Section 8 of `src/shell/text.lua`**:
   - `mark-whole-buffer`: Sets point to 0, mark to `buffer.len()`, and messages `"Mark set"`. Bound to `C-x h`.
   - `delete-blank-lines`: Implements GNU Emacs dual semantics:
     - On a non-blank line: deletes all contiguous following blank lines.
     - On a blank line in a blank cluster: compresses the cluster down to exactly one blank line.
     - On an isolated single blank line: deletes the blank line completely (including its newline). Bound to `C-x C-o`.
   - `upcase-region` / `downcase-region`: Requires an active mark (echoes `"The mark is not set now"` if mark is unset); converts the region in-place and preserves original `point` and `mark` offsets without collapsing cursor positions. Bound to `C-x C-u` and `C-x C-l`.
   - `kill-whole-line`: Kills from line start through trailing newline into the kill-ring via `buffer.kill`, positioning the cursor at line start. Registered as an `M-x` command.
2. **Keybindings in `src/shell/keymap.lua`**:
   - `C-x h` -> `"mark-whole-buffer"`
   - `C-x C-o` -> `"delete-blank-lines"`
   - `C-x C-u` -> `"upcase-region"`
   - `C-x C-l` -> `"downcase-region"`

## Consequences
- No changes required in the Rust Core: existing FFI primitives (`point`, `set_point`, `mark`, `set_mark`, `len`, `delete`, `kill`, `replace`, `message`) fully suffice.
- All new commands are automatically synchronized with `ed.commands()` and exposed to `M-x` TAB completion.
- Consistent adherence to GNU Emacs editing muscle memory and behavior invariants.
