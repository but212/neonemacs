# ADR 0007: Lua Extended Text Operations and Inspection (Historical)

- Status: superseded (superseded by ADR 0006 Pareto Spec Normalization)
- Date: 2026-10-25
- Spec: `docs/spec.md` §6, §12

> **Note**: This ADR consolidates former ADRs 0007 (Movement & Transposition), 0008 (Paragraph & Sentence Operations), 0010 (Buffer & Region Operations), and 0011 (Text Metrics & Inspection). Essential high-frequency operations were merged into ADR 0006, while low-frequency extensions were pruned.

## Context

Following the initial introduction of the Lua Buffer API in ADR 0006, NeonEmacs underwent successive feature additions to replicate GNU Emacs editing convenience in `src/shell/text.lua`:

1. Word and buffer navigation (`forward-word`, `backward-word`, `beginning-of-buffer`, `end-of-buffer`) and line/word transposition (`open-line`, `delete-indentation` / `join-line`, `transpose-chars`, `transpose-words`) (former ADR 0007).
2. Paragraph and sentence motion, marking, and killing (`forward-paragraph`, `backward-paragraph`, `mark-paragraph`, `forward-sentence`, `backward-sentence`, `kill-sentence`), alongside `buffer.mark` / `buffer.set_mark` FFI (former ADR 0008).
3. Whole buffer selection (`mark-whole-buffer`), blank line compression (`delete-blank-lines`), regional case transformations (`upcase-region`, `downcase-region`), and whole line killing (`kill-whole-line`) (former ADR 0010).
4. Buffer text statistics and cursor position inspection (`count-words-region`, `count-words`, `what-cursor-position`, `what-line`), supported by thin Rust SIMD FFI `buffer.count_lines` (former ADR 0011).

While these additions provided broad Emacs feature parity, they expanded `text.lua` to over 1,100 lines and added dozens of specialized keybindings and FFI surfaces.

## Historical Decisions

1. **Word & Buffer Movement and Transposition (Former ADR 0007)**:
   - Implemented `forward-word` (`M-f`), `backward-word` (`M-b`), `beginning-of-buffer` (`M-<`), `end-of-buffer` (`M->`).
   - Implemented line-bounded character transposition (`transpose-chars`, `C-t`), word swapping (`transpose-words`, `M-t`), `open-line` (`C-o`), and line joining (`delete-indentation` / `join-line`, `M-^`).
2. **Paragraph, Sentence, and Mark FFI (Former ADR 0008)**:
   - Added `buffer.mark()` and `buffer.set_mark(pos)` FFI to query and update the active buffer mark.
   - Implemented paragraph navigation (`M-}`, `M-{`) and marking (`M-h`).
   - Implemented sentence boundary parsing, navigation (`M-e`, `M-a`), and killing (`M-k`).
3. **Buffer & Region Operations (Former ADR 0010)**:
   - Implemented whole buffer selection (`mark-whole-buffer`, `C-x h`).
   - Implemented blank-line compression/deletion (`delete-blank-lines`, `C-x C-o`).
   - Implemented mark-guarded region case conversions (`upcase-region`, `C-x C-u`; `downcase-region`, `C-x C-l`).
   - Implemented `kill-whole-line` via `buffer.kill`.
4. **Text Metrics & Inspection FFI (Former ADR 0011)**:
   - Added thin allocation-free FFI `buffer.count_lines(start, end)` delegating to Rust SIMD `memchr`.
   - Implemented region/buffer word and line counting DWIM (`count-words-region`, `M-=`; `count-words`).
   - Implemented detailed point/character/column/byte inspection (`what-cursor-position`, `C-x =`).
   - Implemented 1-based line reporting (`what-line`).

## Supersession & Consolidation (ADR 0006)

In accordance with specification normalization and the Pareto (80/20) rule:

- **Retained & Consolidated into ADR 0006**:
  - `forward-word` (`M-f`), `backward-word` (`M-b`), `beginning-of-buffer` (`M-<`), `end-of-buffer` (`M->`).
  - `mark-whole-buffer` (`C-x h`).
  - `buffer.mark()` and `buffer.set_mark(pos)` FFI.
- **Pruned from Core & Shell**:
  - Transposition, sentence operations, paragraph motion, blank line deletion, regional case switching, word count metrics, and cursor inspection were removed from `src/shell/text.lua` and `src/shell/keymap.lua`.
  - `buffer.count_lines` FFI was removed from `src/shell/lua.rs`.
  - Former invariants I-28, I-29, and I-30 were decommissioned.
