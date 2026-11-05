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
| [0011](0011-cli-flags.md) | CLI Flags, Version and Help, and End of Options | Accepted | 2026-10-27 | §12 | `Invocation` sum type (I-39), `--` end of options and order-dependent parsing (I-40), unknown `-` options exit 2 |
| [0012](0012-scratch-durability-and-wal-probe.md) | Durable Scratch and One WAL Probe | Accepted | 2026-11-05 | §7, §12 | `Backing::Scratch` and its WAL slot (I-42, I-43), unusable WALs moved aside (I-44), one `ProbeWal` on every open (I-45) |
| [0013](0013-save-epoch-modified-tracking.md) | Save Epoch and Clean-by-Undo | Accepted | 2026-11-05 | §7 | Undo back to the saved text restores clean and drops the WAL (I-41) |

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

## 3. System Invariant Matrix (`I-01` ~ `I-47`)

| ID | Invariant Summary | Enforcement Layer | Governed By |
| :--- | :--- | :---: | :--- |
| **I-01** | Cursor and mark are always snapped to valid UTF-8 boundaries in `0..=len` | Runtime (`snap_anchors`) | ADR 0002, ADR 0006 |
| **I-02** | Gap buffer byte boundaries are strictly local and continuous | Data structure | `src/core/text.rs` |
| **I-03** | Buffer clean state tracks disk synchronization accuracy | Type / Runtime | `src/core/buffer.rs` |
| **I-04** | Save sequence checks external disk change, including a file created after a missing path was opened; `write-file` over an existing file asks first; files over `max-file-size` are refused at open, so no partial buffer exists | Type (`loaded_from` → `Result`) / Runtime | ADR 0002, ADR 0009 |
| **I-05** | Successful disk write records fresh metadata (`DiskState`) | Type (`Event::Written`) | ADR 0002 |
| **I-06** | Modified flag accurately reflects buffer changes vs saved state | Runtime | `src/core/buffer.rs` |
| **I-07** | Read-only buffers reject mutations and EOL toggling | Runtime (`view_mode`) | Spec §10 |
| **I-08** | At least one buffer exists at all times; closing retargets windows safely | Runtime (`BufferPool`) | ADR 0002, Spec §5 |
| **I-09** | Consecutive kills append to the same kill-ring entry | Runtime (`append_last`) | ADR 0002, ADR 0006 |
| **I-10** | [Pruned] Persistent undo verified against disk state (persistent undo removed) | Pruned | ADR 0002, Spec §7 |
| **I-11** | Configuration settings validate ranges and types at ingress boundary | Runtime boundary | ADR 0001 |
| **I-12** | Key binding syntax accepts only valid single keys or `C-x` 2-key sequences | Parser boundary | ADR 0001, ADR 0005 |
| **I-13** | `WriteFile` effect is emitted only after confirmation or clean check | Runtime | ADR 0002 |
| **I-14** | `src/core` never imports I/O crates (`fs`, `process`, `mlua`, `crossterm`) | CI check / Test scan | ADR 0002 |
| **I-15** | Every `Effect` is answered by exactly one `Event` (1:1 correspondence) | Type (`shell::perform`) | ADR 0002 |
| **I-16** | [Pruned] `WriteUndo` bytes encoded from `DiskState` (persistent undo removed) | Pruned | ADR 0002 |
| **I-17** | No `RefCell` borrow is held across a Lua call boundary | Runtime | ADR 0002 |
| **I-18** | Zero `&mut` in any `src/core` signature (parameters, returns, fields) | Pure scan (`tests/core/purity.rs`) | ADR 0002 |
| **I-19** | Selected window index is strictly bounded: `selected < layout.leaf_count()` | Runtime (`WindowTree`) | ADR 0002 |
| **I-20** | Any buffer id passed to `with_buffer` exists in the buffer pool | Runtime | ADR 0002 |
| **I-21** | Shell's `RefCell<Option<Editor>>` is `None` only during `step` evaluation | Runtime (`take`/`put`) | ADR 0002 |
| **I-22** | ANSI color escapes never distort text display width, coordinates, or diff | Type / Render queue | ADR 0003 |
| **I-23** | WAL persistence is an asynchronous shell concern; core transitions remain pure. The header records the EOL (`NEONWAL2`) | Runtime (`wal.rs`) | ADR 0003, ADR 0009 |
| **I-24** | Background shell execution is non-blocking with active spinner feedback | Runtime / Thread | ADR 0003 |
| **I-25** | Regular save, revert, or close immediately deletes sibling WAL file; `write-file` deletes the old path's WAL only after its write succeeds | Runtime (`wal.rs`) | ADR 0003, ADR 0010 |
| **I-26** | Kill ring history is encapsulated in `KillRing` with ring rotation invariants, and holds at most `KILL_RING_MAX` (120) entries; `from_entries` (no check) is test-only | Data structure | ADR 0006, ADR 0010, Spec §6 |
| **I-27** | Syntax definitions are purely declarative in `SyntaxRegistry` | Data structure | ADR 0004 |
| **I-28** | [Pruned] Line counting semantics (superseded by ADR 0006 Pareto Spec Normalization) | Pruned | ADR 0006, ADR 0007 |
| **I-29** | [Pruned] UTF-8 inspection slicing (superseded by ADR 0006 Pareto Spec Normalization) | Pruned | ADR 0006, ADR 0007 |
| **I-30** | [Pruned] Cursor inspection coordinates (superseded by ADR 0006 Pareto Spec Normalization) | Pruned | ADR 0006, ADR 0007 |
| **I-31** | Text and EOL change only through `Editor::apply(Edit)`, which updates undo, `modified`, and `wal_dirty`, or through a whole-text replacement that resets undo (I-34) | Runtime (`Editor::apply`, `Buffer::with_text`, `Buffer::mark_clean`) | ADR 0009, ADR 0010 |
| **I-32** | `Editor` groups (Documents, Interaction, Settings) are private to `editor`; a group never writes another group's fields, and cross-group operations are composed in `Editor` methods | Module privacy (`src/core/editor/`) | ADR 0002 |
| **I-33** | `edit_seq` is owned by Documents and changed only by `Editor::apply` | Runtime (`Editor::apply`) | ADR 0002 |
| **I-34** | Replacing a buffer's text outside `Editor::apply` resets its undo log, so undo describes the current text | Runtime (`Buffer::with_text`) | ADR 0010 |
| **I-35** | The undo limit is `Config.undo_limit` only; `UndoLog` stores no limit | Type (`UndoLog::record(edit, limit)`) | ADR 0010 |
| **I-36** | `write-file` changes the buffer's backing, name, and old WAL only after a successful write | Type (`Event::Written { id, path, result }`) | ADR 0010 |
| **I-37** | `FilePath` has one constructor, `resolve`, which folds `.` and `..` lexically; one buffer visits one path (checked in `file_read(Open)` and `write_file`) | Type (`FilePath`) + Runtime (`commands/file.rs`) | ADR 0010 |
| **I-38** | `LastCommand::Yank` is valid only until the next applied edit | Runtime (`Editor::apply`) | ADR 0010 |
| **I-39** | A version or help request carries no file list, and `parse_args` is the only place that classifies `-`-prefixed arguments | Type (`Invocation`) + Boundary (`parse_args`) | ADR 0011 |
| **I-40** | `--` makes later `-`-prefixed arguments files; the rule depends on argument order | Runtime (`parse_args` loop) | ADR 0011 |
| **I-41** | An undo group records the save epoch if it opened on clean text; undoing the whole group restores clean only at that epoch, and a clean buffer never holds modified text. Text equal to the saved text may stay modified after the log was trimmed or replaced | Runtime (`Editor::undo`) | ADR 0013 |
| **I-42** | `Backing::Scratch` is made only for the buffer named `*scratch*`, and at most one exists | Runtime (`BufferPool`, `switch-to-buffer`) | ADR 0012 |
| **I-43** | The scratch WAL slot exists only while a scratch buffer holds unsaved text (a crash may leave it) | Runtime (`ShellSession`) | ADR 0012 |
| **I-44** | An unusable WAL is never deleted or overwritten by inference; it is moved to `.stale` and reported before the next flush | Boundary (`wal::probe_file`) | ADR 0012 |
| **I-45** | A WAL is looked for in one place, when a file read creates a buffer, so every way to open a file recovers | Type (`Effect::ProbeWal` made only in `file_read(Open)`) | ADR 0012 |
| **I-46** | In a yes-or-no question and in query-replace every key either changes the state or says why it did not; `y`/`n`/`q` fold case and Korean jamo `ㅛ`/`ㅜ`/`ㅂ`; `Esc` cancels | Runtime (`answer`, `query_key`) | Spec §8, §9 |
| **I-47** | Only an active mark makes a region, and only the selected window draws the region and the current search match in reverse video | Type (`Mark`) + Runtime (`render::frame`) | Spec §5, §6 |

---

## 4. ADR Authoring Guide

When creating a new ADR (`0009-*.md`):

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
   - `## Invariants`: Invariants introduced or referenced (`I-XX`).
   - `## Consequences`: Benefits, trade-offs, and behavioral changes.
4. **Index Update**: Add the new ADR to `docs/adr/README.md` and synchronize with `docs/spec.md`.
