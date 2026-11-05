# Architecture Decision Records (ADRs)

> Living architectural contracts for NeonEmacs. Every irreversible decision, public API commitment, structural invariant, and boundary rule is recorded here.

---

## 1. Master ADR Catalog

| ADR | Title | Status | Date | Spec | Key Invariants / Scope |
| :--- | :--- | :---: | :---: | :---: | :--- |
| [0001](0001-lua-public-api.md) | Lua public API | Accepted | 2026-10-24 | §12 | Minimal 5-function public API (`bind`, `command`, `run`, `get`, `set`), setting validations |
| [0002](0002-functional-core-imperative-shell.md) | Functional core, imperative shell | Accepted | 2026-10-24 | §2, §7, §8 | Pure state transitions (`step`), single-effect/event mapping, zero `&mut` in core signatures (`I-18`) |
| [0003](0003-async-shell-wal-syntax.md) | Async Shell, Idle WAL Crash Recovery, and 1-Pass Syntax Highlighting | Accepted | 2026-10-24 | §4, §5, §7 | Non-blocking shell queue (`I-24`), 1s idle delta WAL (`I-23`, `I-25`), zero-alloc ANSI-free tokenizer (`I-22`) |
| [0004](0004-lua-syntax-api.md) | Lua syntax registration API | Accepted | 2026-10-25 | §5, §12 | Declarative `syntax.register` module, `SyntaxRegistry` (`I-27`), bundled `syntax.lua` |
| [0005](0005-lua-keymap-prelude.md) | Bundled Lua keymap prelude and unbind API | Accepted | 2026-10-25 | §3, §12 | `unbind` API, bundled `keymap.lua`, separation of 25-key survival set from extended keybindings |
| [0006](0006-lua-buffer-api-and-text-commands.md) | Lua Buffer API and Pareto Core Text Editing Commands | Accepted | 2026-10-25 | §6, §12 | Minimal `buffer` FFI, bundled `text.lua` Pareto core (`forward/backward-word`, `kill-word`, `beginning/end-of-buffer`, `fill-paragraph`, `comment-dwim`, `mark-whole-buffer`) |
| [0007](0007-lua-extended-text-operations-and-inspection.md) | Lua Extended Text Operations and Inspection (Historical) | Superseded | 2026-10-25 | §6, §12 | Superseded by ADR 0006 Pareto Spec Normalization (Consolidates former ADRs 0007, 0008, 0010, 0011) |
| [0008](0008-lua-comment-dwim-and-mx-completion.md) | Lua Comment DWIM and M-x Command Completion Integration | Accepted | 2026-10-25 | §4, §6, §12 | Unified `M-x` completion for Lua commands, `buffer.comment_prefix`, echo `message`, indent-aware `comment-dwim` (`M-;`) |
| [0009](0009-oversized-files-and-eol-edit.md) | Refuse Oversized Files, Undoable EOL Toggle, and Query-Replace Read-Only Check | Accepted | 2026-10-26 | §7, §8, §9, §10 | Oversized files refused at open (I-04), `toggle-eol` as `Edit::SetEol` (I-31), WAL v2 stores EOL (I-23) |
| [0010](0010-save-as-transitions-path-identity-bounded-history.md) | Save-As Transitions, Path Identity, and Bounded History | Accepted | 2026-10-26 | §6, §7, §8, §12 | Save-as changes state only after success (I-36), `FilePath` and one buffer per path (I-37), one undo limit (I-35), undo reset on text replacement (I-34), yank ends at next edit (I-38), kill ring cap (I-26) |
| [0011](0011-cli-flags.md) | CLI Flags, Version and Help, and End of Options | Accepted | 2026-10-27 | §12 | `Invocation` sum type (I-39), `--` end of options and order-dependent parsing (I-39), unknown `-` options exit 2 |
| [0012](0012-scratch-durability-and-wal-probe.md) | Durable Scratch and One WAL Probe | Accepted | 2026-11-05 | §7, §12 | `Backing::Scratch` and its WAL slot (I-42, I-43), unusable WALs moved aside (I-44), one `ProbeWal` on every open (I-45) |
| [0013](0013-save-epoch-modified-tracking.md) | Save Epoch and Clean-by-Undo | Accepted | 2026-11-05 | §7 | Undo back to the saved text restores clean and drops the WAL (I-41) |
| [0014](0014-lua-key-aliases.md) | Lua key aliases | Accepted | 2026-11-06 | §3, §8, §9, §12, §15 | `key_alias`/`key_unalias`, full two-set table in bundled `keymap.lua`, aliases apply to modal answers and to prefixed or modified binding lookups after an exact miss (I-46, I-48) |

---

## 2. Architecture Pillars

```
+--------------------------------------------------------------------------+
|                     Imperative Shell (src/shell/)                        |
|  - Terminal I/O & ANSI Rendering (crossterm)                             |
|  - Asynchronous Background Tasks (threads, channels, I-24)               |
|  - Atomic Disk I/O & WAL Crash Recovery (atomic.rs, wal.rs, I-23, I-25)  |
|  - Lua 5.4 Runtime & Bundled Scripts (lua.rs, text.lua, syntax.lua)     |
+------------------------------------+-------------------------------------+
                                     |
                         Events / Effects Boundary
                         (1:1 Mapping, ADR 0002, I-15)
                                     v
+--------------------------------------------------------------------------+
|                       Functional Core (src/core/)                        |
|  - Zero I/O Imports (I-14) & Zero `&mut` Signatures (I-18)               |
|  - Deterministic Transitions: step(Editor, Event) -> (Editor, Effects)   |
|  - UTF-8 GapBuffer, Multi-Window Layout (I-19), BufferPool (I-08)        |
|  - Encapsulated KillRing (I-26), SyntaxRegistry (I-27), UndoLog          |
+--------------------------------------------------------------------------+
```

### Pillar I: Pure Functional Core

- State is immutable value data transformed by pure step functions: `step(Editor, Event) -> (Editor, Effects)`.
- Core never imports I/O crates (`std::fs`, `std::process`, `mlua`, `crossterm`) (**I-14**).
- Zero `&mut` in any function or method signature in `src/core` (**I-18**).
- All anchors (cursor, mark) are strictly snapped to valid UTF-8 character boundaries in `0..=len` (**I-01**).

### Pillar II: Imperative Shell & Crash Resilience

- The shell handles all external interactions and stateful drivers.
- Non-blocking asynchronous task execution (`Effect::RunShell` -> `Event::ShellDone`) (**I-24**).
- Safe idle delta WAL (Write-Ahead Log) flushed after 1 second of typing inactivity (**I-23**) and purged upon clean save/revert (**I-25**).
- Atomic file writes using temporary isolation and `rename` to prevent corrupted 0-byte files upon sudden power loss.

### Pillar III: Minimal Public Lua FFI Bridge

- Lua is embedded exclusively in the shell (`src/shell/lua/`).
- The public API is deliberately small and compatibility-stable (ADR 0001, ADR 0004, ADR 0006).
- Re-entrancy protection: Lua commands execute synchronously with a maximum nesting limit of 50.
- Bundled prelude scripts (`syntax.lua`, `keymap.lua`, `text.lua`) boot ahead of user `init.lua`, enabling standard editor functionality without core code bloat.

### Pillar IV: Standard Editing & High-Level Text Primitives

- High-level text transformations, word/sentence/paragraph navigation, structural transpositions, region operations, and inspection are implemented in declarative Lua.
- Core commands table merges built-in Rust commands with Lua-registered commands for unified `M-x` TAB completion and keymap introspection (ADR 0008).

---

## 3. System Invariants

The current wording of every invariant (`I-01` ~ `I-51`) is in [`docs/invariants.md`](../invariants.md), which governs if an ADR's wording differs. ADRs record when and why an invariant changed.

---

## 4. ADR Authoring Guide

When creating a new ADR (`NNNN-*.md`):

1. **Filename**: `NNNN-kebab-case-title.md`
2. **Metadata Header**:

   ```markdown
   # ADR NNNN: Title

   - Status: proposed | accepted | superseded by ADR XXXX
   - Date: YYYY-MM-DD
   - Spec: `docs/spec.md` §X, §Y
   ```

3. **Required Sections**:
   - `## Context`: The problem, existing constraints, and architectural gap.
   - `## Decision`: Detailed technical choices, API schemas, and invariants.
   - `## Invariants`: One bullet per invariant the ADR changes: `- **I-XX** (new | revised | extended | pruned; enforcement): what changed`. The current wording goes in `docs/invariants.md`.
   - `## Consequences`: Benefits, trade-offs, and behavioral changes.
4. **Index Update**: Add the new ADR to `docs/adr/README.md`, update `docs/invariants.md`, and synchronize with `docs/spec.md`.
