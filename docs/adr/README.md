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
|  - Encapsulated KillRing (I-26), SyntaxRegistry (I-27), UndoLog (I-10)   |
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

- Lua is embedded exclusively in the shell (`src/shell/lua.rs`).
- The public API is deliberately small and compatibility-stable (ADR 0001, ADR 0004, ADR 0006).
- Re-entrancy protection: Lua commands execute synchronously with a maximum nesting limit of 50.
- Bundled prelude scripts (`syntax.lua`, `keymap.lua`, `text.lua`) boot ahead of user `init.lua`, enabling standard editor functionality without core code bloat.

### Pillar IV: Standard Editing & High-Level Text Primitives

- High-level text transformations, word/sentence/paragraph navigation, structural transpositions, region operations, and inspection are implemented in declarative Lua.
- Core commands table merges built-in Rust commands with Lua-registered commands for unified `M-x` TAB completion and keymap introspection (ADR 0008).

---

## 3. System Invariant Matrix (`I-01` ~ `I-30`)

| ID | Invariant Summary | Enforcement Layer | Governed By |
| :--- | :--- | :---: | :--- |
| **I-01** | Cursor and mark are always snapped to valid UTF-8 boundaries in `0..=len` | Runtime (`snap_anchors`) | ADR 0002, ADR 0006 |
| **I-02** | Gap buffer byte boundaries are strictly local and continuous | Data structure | `src/core/text.rs` |
| **I-03** | Buffer clean state tracks disk synchronization accuracy | Type / Runtime | `src/core/buffer.rs` |
| **I-04** | Save sequence strictly checks truncation first, then external disk change | Runtime | ADR 0002 |
| **I-05** | Successful disk write records fresh metadata (`DiskState`) | Type (`Event::Written`) | ADR 0002 |
| **I-06** | Modified flag accurately reflects buffer changes vs saved state | Runtime | `src/core/buffer.rs` |
| **I-07** | Read-only buffers reject mutations and EOL toggling | Runtime (`view_mode`) | Spec §10 |
| **I-08** | At least one buffer exists at all times; closing retargets windows safely | Runtime (`BufferPool`) | ADR 0002, Spec §5 |
| **I-09** | Consecutive kills append to the same kill-ring entry | Runtime (`append_last`) | ADR 0002, ADR 0006 |
| **I-10** | Persistent undo is verified against disk state before application | Runtime (`UndoLog::decode`) | ADR 0002, Spec §7 |
| **I-11** | Configuration settings validate ranges and types at ingress boundary | Runtime boundary | ADR 0001 |
| **I-12** | Key binding syntax accepts only valid single keys or `C-x` 2-key sequences | Parser boundary | ADR 0001, ADR 0005 |
| **I-13** | `WriteFile` effect is emitted only after confirmation or clean check | Runtime | ADR 0002 |
| **I-14** | `src/core` never imports I/O crates (`fs`, `process`, `mlua`, `crossterm`) | CI check / Test scan | ADR 0002 |
| **I-15** | Every `Effect` is answered by exactly one `Event` (1:1 correspondence) | Type (`shell::perform`) | ADR 0002 |
| **I-16** | `WriteUndo` bytes are encoded directly from `DiskState` in `Written` | Boundary | ADR 0002 |
| **I-17** | No `RefCell` borrow is held across a Lua call boundary | Runtime | ADR 0002 |
| **I-18** | Zero `&mut` in any `src/core` signature (parameters, returns, fields) | Pure scan (`tests/core_purity.rs`) | ADR 0002 |
| **I-19** | Selected window index is strictly bounded: `selected < layout.leaf_count()` | Runtime (`WindowTree`) | ADR 0002 |
| **I-20** | Any buffer id passed to `with_buffer` exists in the buffer pool | Runtime | ADR 0002 |
| **I-21** | Shell's `RefCell<Option<Editor>>` is `None` only during `step` evaluation | Runtime (`take`/`put`) | ADR 0002 |
| **I-22** | ANSI color escapes never distort text display width, coordinates, or diff | Type / Render queue | ADR 0003 |
| **I-23** | WAL persistence is an asynchronous shell concern; core transitions remain pure | Runtime (`wal.rs`) | ADR 0003 |
| **I-24** | Background shell execution is non-blocking with active spinner feedback | Runtime / Thread | ADR 0003 |
| **I-25** | Regular save, revert, or close immediately deletes sibling WAL file | Runtime (`wal.rs`) | ADR 0003 |
| **I-26** | Kill ring history is encapsulated in `KillRing` with ring rotation invariants | Data structure | ADR 0006, Spec §6 |
| **I-27** | Syntax definitions are purely declarative in `SyntaxRegistry` | Data structure | ADR 0004 |
| **I-28** | [Pruned] Line counting semantics (superseded by ADR 0006 Pareto Spec Normalization) | Pruned | ADR 0006, ADR 0007 |
| **I-29** | [Pruned] UTF-8 inspection slicing (superseded by ADR 0006 Pareto Spec Normalization) | Pruned | ADR 0006, ADR 0007 |
| **I-30** | [Pruned] Cursor inspection coordinates (superseded by ADR 0006 Pareto Spec Normalization) | Pruned | ADR 0006, ADR 0007 |

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
