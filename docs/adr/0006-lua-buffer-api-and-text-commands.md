# ADR 0006: Lua Buffer API and Pareto Core Text Editing Commands

- Status: accepted
- Date: 2026-10-25
- Spec: `docs/spec.md` §6, §12

## Context

Prior to this ADR, text formatting and advanced editing commands were either hardcoded into the Rust core (`src/core/commands/edit.rs`) or fragmented across experimental extensions. In accordance with the Pareto principle (80/20 rule) and specification normalization, NeonEmacs consolidates user-facing text editing into a minimal Lua Buffer API and a bundled Pareto-core script (`src/shell/text.lua`).

## Decision

1. **Expose minimal `buffer` module in Lua (§12):**
   - Inspection: `buffer.point()`, `buffer.len()`, `buffer.get_text(start, end)`, `buffer.line_start(pos)`, `buffer.line_end(pos)`, `buffer.width(text)`, `buffer.mark()`, `buffer.comment_prefix()`.
   - Mutation: `buffer.set_point(pos)`, `buffer.set_mark(pos)`, `buffer.insert(pos, text)`, `buffer.delete(start, end)`, `buffer.kill(start, end)`, `buffer.replace(start, end, text)`.
   - All positions use 0-based UTF-8 byte offsets matching Rust `Text`. Any cursor repositioning automatically snaps to valid unit boundaries (I-01).
   - `buffer.kill(start, end)` deletes text and pushes it to the kill ring, merging consecutive kills into a single kill ring entry (I-09).
2. **Bundle Pareto-core `src/shell/text.lua` (§6, §12):**
   - Implement only the highest-frequency, essential editing commands in Lua:
     - Word navigation & deletion: `forward-word` (`M-f`), `backward-word` (`M-b`), `kill-word` (`M-d`), `backward-kill-word` (`M-DEL`).
     - Buffer navigation: `beginning-of-buffer` (`M-<`), `end-of-buffer` (`M->`).
     - Paragraph formatting: `fill-paragraph` (`M-q`) respecting `fill-column`, display width, tab width, and indentation.
     - Commenting: `comment-dwim` (`M-;`) with auto-detection of buffer syntax comment prefixes.
     - Selection: `mark-whole-buffer` (`C-x h`).
   - Bundle `text.lua` via `include_str!` into `BUNDLED_SCRIPTS` evaluated during bootstrap before `keymap.lua`.
3. **Core purity and minimal FFI surface:**
   - Low-frequency commands (sentence navigation, transpositions, line join/open, horizontal space cleanup, blank line compression, region case switching, cursor inspection) and special FFI (`buffer.count_lines`) are pruned to keep the maintenance surface minimal.
   - Core Rust signatures retain zero `&mut` parameters (I-18).

## Consequences

- Essential editing muscle memory is fully served with approximately 250 lines of clean Lua rather than thousands of lines of heuristic code.
- Core binary size remains strictly under 5MB.
- Users can override or extend commands directly in `~/.config/neonemacs/init.lua`.
