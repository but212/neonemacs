# Domain Context

> Domain terms, architectural boundaries, design rationale, operational lifecycles, and unified system invariants.

This document establishes the conceptual architecture, domain models, boundary contracts, and design rationale governing `neonemacs`. In case of conflict between prose and invariants, §5 governs.

---

## 1. Architecture Boundaries & Core Identity

`neonemacs` is a lightweight terminal text editor with Emacs key feel, functional purity, zero input latency, exact Unicode/CJK width handling, and single-pass atomic file persistence. Emacs conventions are adopted strictly for key bindings and command names without emulating Elisp, macros, or internal buffer architectures.

### 1.1 Functional Core / Imperative Shell

The system partitions strictly into a pure functional core and an imperative shell communicating via an `Effect`/`Event` contract (I-15).

- **Core (`crates/neonemacs-core`, exposed as `neonemacs::core`)**:
  - `#![no_std]` + `alloc` pure state transition machine: `step(Editor, Event) -> (Editor, Effects)`.
  - Zero `std` / I/O dependencies (I-14): never imports `std`, `mlua`, `crossterm`, `std::fs`, or `std::process`.
  - Zero `&mut` references in public and internal signatures (I-18); updates proceed via value ownership transfer (`Editor -> Editor`, `Buffer -> Buffer`, `WindowManager -> WindowManager`).
  - Built-in command dispatch: `BUILTINS` function-pointer table `fn(Editor, Effects) -> (Editor, Effects)`.
  - Module privacy: `lib.rs` curates the re-export surface; internal modules (`editor/`, `commands/`, `dispatch.rs`, `width.rs`) remain private.
- **Shell (`src/shell`)**:
  - Owns `Editor` within `RefCell<Option<Editor>>` and extracts it during `step` (I-21).
  - Terminal I/O: raw mode, alternate screen buffer, differential ANSI rendering (`session/screen.rs`).
  - Storage I/O: single-pass atomic temp-file saves (`registore.rs`), lexical path resolution, external modification detection (`file.rs`).
  - Process execution: background worker threads for non-blocking shell commands (`process.rs`).
  - Scripting: embedded Lua 5.4 runtime and bundled script evaluation (`lua/`).
  - Startup orchestration: command-line flag parsing and initial editor construction (`startup/`).
- **Effect & Event contracts**:
  - `Effect`: `StatDisk`, `WriteFile`, `ReadFile`, `RunShell`, `RunLua`, `ReloadInit`, `Suspend`.
  - `Event`: `Key`, `Run`, `Disk`, `Written`, `FileRead`, `ShellDone`, `LuaDone`, `InitLoaded`.
- **`Editor` internal state** (`crates/neonemacs-core/src/editor/`, private to `editor`, I-32):
  - Document & interaction fields directly on `Editor`: `BufferPool`, `WindowManager`, `KillRing`, `Mode`, `last`/`prev` `LastCommand`, echo-area message buffer, `quit` flag.
  - `Settings`: `Config`, `Keymap`, `KeyAliases`, `SyntaxRegistry`, `commands` (`BTreeSet<String>`), `cwd`.

### 1.2 Platform & Runtime Environment

- **Cross-Platform Host Boundary**: Functional core maintains zero platform-specific dependencies; host-specific system calls (raw terminal control, atomic file renaming, and process spawning across Linux, macOS, and Windows) are strictly encapsulated within the imperative shell.
- **Terminal Protection Domain (`TerminalGuard`)**: An RAII resource guard guarantees terminal sanity. It hooks directly into the host process lifecycle and pre-panic hooks to ensure the terminal restores raw mode and the primary screen buffer across normal exits, early error terminations, and unhandled panics (I-53).
- **Execution Budget & Simplicity**: Designed for instant startup and negligible memory footprint, targeting strip release binaries under 5MB by eliminating monolithic async runtimes and complex garbage collection engines outside the sandboxed Lua runtime.

---

## 2. Glossary & Domain Models

### 2.1 Buffers, Windows, and Files

- **`Buffer`**: Core text storage and metadata entity:
  - `Doc` (`Weave Tree`): Summarized immutable CoW slice B-Tree (`MAX_FANOUT = 32`, `MIN_FANOUT = 16`, `CHUNK_CAP = 4096`, `MAX_SLICE_BYTES = 16384`) over shared byte slices interpreted as strict RFC 3629 `Unit`s. For $P$ slices, line/offset queries take $O(\log P)$, while edits using one-pass tree splits take $O(\log P)$ worst case (`D-01`..`D-05`).
  - `saved_point`: Per-buffer cursor byte offset.
  - `Mark`: Optional anchor byte offset. Active mark forms the active region; inactive mark is preserved for position swapping (I-47).
  - `Eol`: Buffer line termination convention (`Lf` or `Crlf`).
  - `UndoLog`: Transactional edit history stack bounded by `Config.undo_limit`.
  - `Backing`: Storage binding: `None` (ephemeral), `Scratch` (`*scratch*`, I-42), or `File { path, disk }` (bound to absolute canonical path).
  - `modified`: Unpersisted modifications against disk baseline (I-03).
  - `view_only`: Read-only flag rejecting edits, `toggle-eol`, and `query-replace` (I-06).
  - `epoch`: Generation counter incremented on baseline changes (`record_saved`, `replace_from_disk`, `mark_clean`, `with_text`).
- **Buffer & Window invariants**:
  - `BufferPool` contains >= 1 buffer at all times (I-08).
  - Closing the last buffer spawns an empty `*scratch*`.
  - Closing any visited buffer retargets viewing windows to a surviving buffer (I-08).
  - Multiple windows can observe the same buffer with independent view cursors (`point`) and scroll tops (`top`).
- **`Unit::Raw`**: Non-UTF-8 (overlong, surrogate, or incomplete) byte preserved byte-exact across open, edit, and save cycles (`D-01`).
- **`DiskState`**: File metadata checkpoint: `Known { mtime: Duration, size: u64 }` (Unix epoch elapsed duration recorded on read/write) or `Unknown` (unopened file, `*scratch*`).
- **`FilePath`**: UTF-8 absolute, lexically folded path constructed exclusively through `FilePath::resolve(cwd, input)`. Normalizes `/` and `\` separators while preserving drive/UNC prefixes, drops `.`, folds `..`, rejects symlink traversal. Non-UTF-8 paths are rejected at the shell boundary. Strictly one buffer per path (I-37).
- **`StatTarget`**: Purpose of disk inspection: `Save(id)` (conflict check before write) or `SaveAs(id)` (overwrite check on existing target).
- **`ReadTarget`**: Purpose of file load: `Open { line, show }` (new buffer creation) or `Revert(id)` (reloading existing buffer).
- **Oversized file rejection**: Boundary error when file size exceeds `max-file-size`; `loaded_from` returns `Err(String)` and aborts buffer creation without partial reads (I-04).
- **`Confirm`**: Modal yes/no confirmation prompt: `SaveExternal`, `OverwriteFile`, `QuitUnsaved`, `KillModified`, `RevertModified`.
- **`Eol`**: Line termination convention (`Lf` `\n` or `Crlf` `\r\n`), determined by majority vote upon reading. Normalized in memory to `\n` (isolated `\r` preserved); CRLF restored upon writing. `toggle-eol` records undoable `Edit::SetEol` and marks the buffer modified.

### 2.2 Edit History and Anchors

- **`Edit`**: Primitive buffer mutations: `Replace(Replace { offset, delete_len, insert: Doc })`, `InsertInline { offset, data: InlineBytes }` (stack-allocated `<= 16` B fast path), and `SetEol { from, to }`. Sole mutation path through `Editor::apply(Edit)` (I-31).
- **Anchor snapping**: All cursor movements, mark placements, and edits clamp and snap `point` and `mark` within `0..=total_bytes()` to valid UTF-8 boundaries (`Doc::floor_boundary`, `ceil_boundary`, I-01).
- **`UndoLog` & `Group`**:
  - In-memory Emacs-style undo (`C-/`, `C-_`) recording inverse edit transactions (`Doc::apply` returns `(next_doc, inverse_replace)` directly with CoW structural sharing); no redo tree.
  - Consecutive `self-insert` insertions amalgamate into a single group up to `amalgamating-undo-limit` (default 20 characters). Non-insert commands immediately seal the group.
  - Clean state restoration (I-41): undo groups opened while clean record save epoch. Undoing the full group restores clean status (`modified = false`) iff epoch matches.
  - Whole-buffer replacements (`Buffer::with_text`: revert, shell output) wipe undo history to prevent stale offset replay (I-34).
  - `Config.undo_limit` governs the undo byte budget (I-35). Undo compacts inverse inserts of at most `CHUNK_CAP` (4 KiB) when they would retain more than `CHUNK_CAP` backing bytes. Each text edit then charges the greater of its logical edit size and the full backing byte lengths retained by its inverse slices, conservatively counting shared allocations more than once. Closed groups exceeding a positive limit are evicted immediately on boundary.
- **`KillRing`**: Ring buffer of CoW `Doc` slices (`Vec<Doc>`) capped at `kill-ring-max` entries (default 120) per `push` (I-26). `kill_range` and `kill_ring_save` extract slices in $O(\log^2 P)$ worst-case time via `Doc::slice`, consecutive kills (`kill-line`, `kill-word`, `C-w`) merge via `Doc::apply` (I-09), and `yank` / `yank-pop` insert `Doc` directly without UTF-8 re-parsing. `yank-pop` cycles entries strictly immediately after `LastCommand::Yank` (I-38). If the yank joined neighboring bytes into a UTF-8 unit, `yank-pop` leaves the text and ring unchanged.

### 2.3 Zero-Overhead Atomic Persistence (Registore)

- **Single-Pass Atomic Rename Swap (`src/shell/registore.rs`)**: Replaces WAL and recovery replay with `registore::persist_with`. Writes stream `Doc::try_for_each_slice` directly to a PID- and sequence-isolated sibling `.filename.<pid>_<seq>.tmp` file (`OpenOptions::create_new(true)`) without intermediate heap copies or double-writes (`WAF = 1.0`), call `file.sync_data()` (`fdatasync`), atomically replace the target via `fs::rename`, and best-effort `sync_all()` the parent directory.
- **Explicit Save Only**: Persistence occurs strictly on explicit `Effect::WriteFile` (`C-x C-s`, `write-file`), delegated from `file::save_doc` (which preserves symlink canonicalization, permissions, and `Eol::Crlf` encoding). `*scratch*` is ephemeral in memory.
- **Zero-Code Recovery**: Reading loads the canonical target file directly; failed writes clean up their isolated `.tmp` file while preserving the original target.

### 2.4 Keys, Input, and Modal Interaction

- **Two-Tier Input Architecture**: The keyboard dispatch partitions into an immutable *Survival Set* compiled directly into core dispatch (`Keymap::survival()`) to guarantee fundamental buffer operations, navigation, and file persistence in headless or unconfigured states, and an *Extended Dispatch Layer* loaded via bundled and user-configured Lua keymaps.
- **Unified Command Model**: Built-in commands (`fn(Editor, Effects) -> (Editor, Effects)`) operate strictly through pure functional state transitions. Every command—whether managing window partitions (`split-window-below`, `split-window-right`, `delete-other-windows`, `other-window`), buffer lifecycles (`switch-to-buffer`, `kill-buffer`), cursor travel, region killing, or persistence—shares this uniform contract without hidden side-effects.
- **Modal State Transitions & Invariant Totality**: Interactive workflows requiring user confirmation or arguments (disk conflict resolution, file overwrite checks, buffer termination, unsaved exit, and query-replace) construct explicit modal objects on `Editor::mode`. All modal prompts enforce total input resolution (I-46): every key event either deterministically mutates state or explains the rejection in the echo area, eliminating silent input drops or ambiguous prompt locks.
- **Internationalized Input Translation (`KeyAliases`, I-48)**: Character-to-ASCII translation mapping designed for non-Latin input layouts (e.g. Korean 2-set). Aliases dynamically resolve non-ASCII graphemes into ASCII commands exclusively for modal prompt answers and prefix/modifier lookups when exact matching fails, while strictly bypassing self-insert, prompt input, and search queries to preserve text entry fidelity.
- **Prefix Sequence State Machine**: Multi-key sequences (`C-x`, `Esc`) transition interaction state into pending prefix modes, reflecting the pending sequence dynamically in the echo area (I-47). The panic room contract intercepts cancellation keys (`C-g`, `Esc`) prior to keymap resolution (I-53), guaranteeing unconditional sequence abortion.

### 2.5 Typography, Display, and Terminal

- **Display Metrics & Width Model**: Visual column calculations decouple display metrics (`Metrics`) from raw byte offsets and character counts. Distinguishes single-column ASCII, double-column East Asian Wide/Fullwidth (Hangul, CJK ideographs), zero-width combining characters, control code representations, and configurable East Asian Ambiguous widths, maintaining alignment across terminal layouts.
- **Decoupled Visual Projection (`Frame`)**: Pure rendering engine produces a character grid tagged with abstract semantic style tokens (`TokenKind`). Escape code generation and terminal ANSI sequences are quarantined strictly within the shell's differential update pipeline (`session/screen.rs`, I-22), ensuring rendering diffs, column calculations, and cursor coordinates remain completely escape-free.
- **Modeline (Status Horizon)**: The status line acts as an informational demarcation between the document viewport and the interaction/echo area. Projects domain invariants: buffer title, disk modification state (`--` clean, `**` modified, `%%` read-only clean, `%*` read-only modified), line-ending convention (`Lf` vs `Crlf`), cursor coordinates, and document scroll percentage.
- **Viewport Clamping**: Viewport re-centering and scrolling calculate vertical bounds relative to window height, clamping scroll tops strictly to valid line starts and buffer boundaries.

### 2.6 Configuration, Lua Runtime, and CLI

- **Closed Configuration Model (`Setting`, I-52)**: Settings operate on a closed domain enum rather than arbitrary key-value tables. `Config::get` is total over all variants, and `Config::set` parses incoming values into strongly-typed domain primitives (`Eol`, `Metrics`, bounded numeric ranges) at ingress (I-11), making invalid editor configurations unrepresentable.
- **Panic Room Safeguards (I-53)**: Safety invariants that remain permanently active and cannot be muted, disabled, or unmapped by configuration settings, Lua extensions, or key rebinding (terminal raw mode restoration, external modification validation, modal confirmations, and `C-g`/`Esc` cancellation).
- **Lua Extensibility & Sandbox Boundary (I-57)**: Lua 5.4 provides a lightweight, embedded scripting environment operating under strict least-privilege sandboxing. Standard libraries providing filesystem or process access (`io`, `os`, `package`, dynamic loading) are eliminated. Extensions interact with the editor solely via safe exposed APIs: pure buffer inspection and character-snapped mutation, declarative syntax registration, key binding manipulation, and command execution.
- **Transactional Configuration Lifecycle (I-54, I-58)**: Configuration evaluation follows a double-buffered candidate evaluation pattern. The script runs against an isolated candidate editor; successful execution atomically swaps settings and the Lua state while preserving active buffers, windows, and edit histories, whereas errors discard the candidate state and log full diagnostics to `*init-log*`.
- **Deterministic CLI Invocation Model (I-39)**: Command-line arguments follow a single-pass classification separating immediate headless queries (version and usage help) from interactive editor bootstrapping and target file parsing.

---

## 3. Operational Lifecycles

### 3.1 Save & Atomic File Persistence

- **Optimistic Concurrency & Validation**: The persistence lifecycle implements an optimistic concurrency protocol. Core emits disk inspection effects (`StatDisk`) to verify baseline filesystem timestamps and sizes against `DiskState` recorded at read/write time.
- **Conflict Resolution**: External modification detection routes to modal confirmation (`Confirm::SaveExternal`, I-04); destination conflicts in target-switching writes enforce uniqueness (I-37) or prompt overwrite confirmation (`Confirm::OverwriteFile`).
- **Atomic Staging & Commit**: Approved writes execute via atomic filesystem semantics (PID- and sequence-isolated temporary file creation, `sync_data`, and atomic rename).
- **Metadata Reconciliation**: Successful disk writes update recorded metadata (`DiskState::Known`, I-05) and advance save epoch (I-36). Write failures preserve dirty buffer state.

### 3.2 Ingress and Buffer Creation

- **Lexical Ingress**: Path resolution normalizes input lexically (`FilePath::resolve`, I-37), focusing existing buffers if the canonical path is already open.
- **Boundary Validation**: File size boundaries enforce upfront rejection (I-04) when exceeding configured thresholds, preventing partial allocations or truncated writebacks.
- **Buffer Reversion**: Reverting a buffer atomically discards unpersisted modifications and synchronizes state with disk baseline, resetting undo history.

### 3.3 Interactive Search & Transactional Replacement

- **Incremental Match Projection**: Incremental search dynamically maintains match coordinates across the text model, projecting highlighted matches on the selected window (I-47).
- **Transactional Replacement**: Query replacement operates as an atomic multi-step interactive transaction. Modality enforces total key resolution (I-46) across confirmation, rejection, batch substitution, and cancellation. The entire session compiles into a single undo group, enabling single-stroke reversibility. Read-only buffers strictly reject interactive mutation (I-06).

### 3.4 Asynchronous Background Execution

- **Worker Thread Isolation**: Background shell commands execute on isolated OS worker threads, eliminating UI latency or terminal freeze (I-24).
- **Non-Blocking Feedback**: The interactive event loop monitors background completion via non-blocking channel polling (50ms interval) with animated visual feedback in the echo area, automatically suppressing animation updates during modal prompts to prevent visual conflict.
- **Resource Caps**: Concurrent background executions are refused to preserve single-session determinism. Stream outputs are strictly bounded (4 MiB per stream cap), directing captured stdout/stderr into dedicated unbacked output buffers.

### 3.5 Configuration Lifecycle & Atomic Hot-Reload

- **Bundled Bootstrap**: The initialization pipeline executes bundled declarative syntax and keymap definitions in fixed sequence prior to user configuration evaluation.
- **Transactional Sandboxing (I-54)**: User configuration (`init.lua`) executes within an isolated candidate sandbox. Entire script success commits configuration; syntax or runtime failures discard candidate changes, log full diagnostics to `*init-log*`, and surface error summaries without partial state corruption (I-55).
- **Runtime Hot-Reload (I-58)**: Hot-reloading re-evaluates configuration against a candidate state, atomically swapping settings and the Lua environment upon success while maintaining unbroken continuity for active buffers, windows, kill ring, and undo history. Failures preserve existing runtime settings intact.

---

## 4. Design Rationale

### 4.1 Core, Shell, and Output

- **Pure state machine with value semantics**: Core state has zero `&mut` signatures (I-18); transitions move ownership via `step(Editor, Event) -> (Editor, Effects)`. The shell owns `Editor` in `RefCell<Option<Editor>>` and extracts it during steps (I-21). Eliminates mutable borrow conflicts across Lua re-entrancy and workers; enables 100% in-memory deterministic testing of file saves and conflicts. *Rejected*: In-place interior mutability; persistent undo files (`.<name>.undo`).
- **Crate isolation**: Core is isolated in `crates/neonemacs-core`. Purity test (`tests/purity.rs`) forbids dependencies on I/O crates (`crossterm`, `mlua`) and std I/O (`std::fs`, `std::process`) (I-14). Prevents side-effect leakage into domain logic at compile time.
- **Terminal escape isolation**: `Frame` cells store pure characters and stylistic token tags. ANSI escape generation is deferred entirely to terminal rendering (`session/screen.rs`) (I-22), preventing escapes from corrupting width calculations, cursor placement, and differential diffs.
- **Synchronous background threads over async runtimes**: Background tasks use dedicated OS threads with standard channels (`mpsc`) polled at 50ms (I-24). Avoids async runtime bloat (Tokio/async-std) to meet the <= 5MB binary budget while keeping zero-latency UI.

### 4.2 Files and Persistence

- **Oversized file rejection upfront**: Files exceeding `max-file-size` (default 512 MiB) are rejected during read targets without loading partial bytes (I-04). Avoids opening truncated prefixes that risk destroying unread data on save. For accepted files, streaming document allocation failures return an I/O error without publishing a partial buffer.
- **Lexical path canonicalization**: Paths normalize strictly via lexical folding (`.` dropped, `..` folded) against working directory without resolving symlinks (I-37), ensuring deterministic buffer mapping without symlink loops or blocking.

### 4.3 Editing and Text Model

- **Immutable CoW slice B-Tree (`Weave Tree`)**: `Doc` (`weave_tree::WeaveTree<LineBreaks>` in `crates/weave-tree/`, with line/offset and compaction helpers in `crates/neonemacs-core/src/lib.rs`) replaces the gap buffer (`text.rs`). Keeps strict RFC 3629 `Unit` boundaries (`D-01`), one-pass O(log P) split-based edits and O(log P) line-index queries (`D-02`, `D-03`), CoW sharing for `UndoLog` inverse `Replace` patches (`D-04`), and zero `&mut` signatures (`D-05`, I-18); `H1` (`Arc<Vec<u8>>` + `Arc::get_mut` in-place append) and `H2` (`criterion` sweeps in `crates/weave-tree/benches/weave_tree_bench.rs`) are active and benchmarked.
- **Safe edit pipeline**: All text modifications and EOL conversions pass through `Editor::apply(Edit)` (I-31). Centralizes undo recording, modified flags, and view-only checks.
- **Save epoch clean tracking**: Undo groups record save epoch if opened while clean. Undoing restores clean status iff current epoch matches (I-03, I-41), accurately clearing dirty flags without diffing.
- **Undo log wiping on external replacement**: Wholesale buffer text replacements (`Buffer::with_text`) invalidate and wipe the undo log (I-34). Prevents replaying historical relative offsets against replaced content.
- **Pareto editing commands via Lua**: Frequent editing operations (`kill-line`, `forward-word`, `fill-paragraph`, etc.) reside in bundled `text.lua` (~270 lines) over character-snapped buffer APIs. Keeps core Rust minimal while enabling user extensibility.

### 4.4 Lua Runtime and Configuration

- **Restricted sandbox**: Sandbox provides base utilities, `string`, `table`, `math`, `utf8`, exposing only safe editor APIs (`buffer.*`, `syntax.*`, `bind`, `command`, `set`) (I-57). Restricts scripts to editor configuration without arbitrary OS or network access.
- **Closed configuration schema**: Configuration levers are strictly defined by closed `Setting` enum (I-52). `Config::get` is total; `Config::set` parses strings into strongly-typed domain primitives (`Eol`, `Metrics`, `usize`), eliminating invalid runtime states.
- **Atomic configuration reload**: `reload-init` evaluates scripts in an isolated candidate runtime; state commits only upon 100% successful execution (I-54, I-58), preventing partial or broken configuration states.

### 4.5 Keys and Input Mapping

- **Survival keys + M-x design**: Default bindings provide only essential survival keys; all other operations use `M-x` or Lua bindings. Minimizes cognitive load, eliminates terminal keycode collisions, and guarantees cross-platform consistency.
- **Modal and prefix alias fallback**: Key aliases translate single non-ASCII characters to ASCII graphics characters exclusively for modal confirmations (`Confirm`, `query-replace`) and fallback prefix lookups (I-48). Enables non-Latin keyboard layouts (e.g. Korean 2-set) to execute commands and confirm prompts without intercepting document text.

---

## 5. Unified System Invariants

This table consolidates all operational and structural invariants governing `neonemacs`. Invariant IDs are permanent and never recycled.

| ID | Invariant Definition | Enforcement Mechanism |
| :--- | :--- | :--- |
| **I-01** | Cursor (`point`), saved cursor (`saved_point`), and `mark` are strictly clamped within `0..=len` and snapped to valid UTF-8 character boundaries. | Runtime (`Buffer` encapsulation, `Editor::apply`) |
| **I-02** | Document byte and `Unit` boundaries are strictly local, continuous, and never split across `ByteSlice` leaves (`D-01`). | Data structure (`crates/weave-tree/`) |
| **I-03** | A buffer is clean (`modified = false`) iff its text matches the last read/saved disk baseline. Edits mark it modified; clean saves, reverts, `mark_clean`, and epoch-matched undos (I-41) clear it. | Runtime (`Buffer::apply_edit`, `record_saved`, `replace_from_disk`, `mark_clean`) |
| **I-04** | Saving checks for external disk modification (including files created externally after open). `write-file` confirms existing target overwrite. Files exceeding `max-file-size` are rejected upfront without partial buffer creation. `WriteFile` is emitted only after validation/confirmation. | Type (`loaded_from` -> `Result`) + Runtime (`save_buffer`, `Confirm`) |
| **I-05** | A successful disk write records fresh filesystem metadata (`DiskState::Known { mtime, size }`). | Type (`Event::Written`) |
| **I-06** | A view-only buffer (`view_only = true`) strictly rejects text modifications, `toggle-eol`, and query-replace operations. | Runtime (`Editor::apply`, `start_query_replace`) |
| **I-07** | *Retired*: Merged into I-04 and I-06. | - |
| **I-08** | At least one buffer exists at all times. Closing the final buffer spawns an empty `*scratch*`. Closing a visited buffer safely retargets viewing windows to a surviving buffer. | Runtime (`BufferPool`) |
| **I-09** | Consecutive kill commands append removed text segments to the same kill ring entry. | Runtime (`KillRing::append_last`) |
| **I-10** | *Retired*: Removed along with persistent undo (`.<name>.undo`). | - |
| **I-11** | Configuration settings validate value ranges and types at ingress. String values parse into domain types (`Eol`, `Metrics`). Invalid `set` calls make no state modifications. | Runtime boundary (`Config::set`) |
| **I-12** | Key binding syntax strictly accepts single keys or `C-x` two-key sequences. | Parser boundary (`parse_key_sequence`) |
| **I-13** | *Retired*: Merged into I-04 and I-06. | - |
| **I-14** | `neonemacs-core` never imports I/O crates (`crossterm`, `mlua`); usage of `std::fs` and `std::process` is strictly forbidden. | Crate graph (`Cargo.toml`) + text scan (`tests/purity.rs`) |
| **I-15** | Every `Effect` is answered by at most one `Event` (interactive `RunShell`, `ReloadInit`, and `Suspend` are handed to `ShellSession`; concurrent `RunShell` rejection yields no event). | Type (`shell::perform`) + Runtime (`handle_interactive`) |
| **I-16** | *Retired*: Removed along with persistent undo. | - |
| **I-17** | *Retired*: Renumbered as I-21. | - |
| **I-18** | Zero `&mut` references in any `neonemacs-core` function signatures, return types, or struct fields. | Purity scan (`crates/neonemacs-core/tests/purity.rs`) |
| **I-19** | Selected window index is strictly bounded: `selected < layout.leaf_count()`. | Runtime (`WindowTree`) |
| **I-20** | Any buffer ID passed to `with_buffer` exists within the buffer pool. | Runtime (`BufferPool::with_buffer`) |
| **I-21** | Effect execution only holds a short immutable borrow scoped strictly to slice reads during `WriteFile`, and no borrow is held across Lua invocations. | Runtime (`shell::perform`, Lua `run`) |
| **I-22** | Terminal ANSI escape sequences do not affect character display widths, coordinate mapping, or differential screen updates. | Type + Output render queue (`session/screen.rs`) |
| **I-23** | *Retired*: WAL persistence replaced by Registore single-pass atomic rename swap (`src/shell/registore.rs`). | - |
| **I-24** | Background shell execution is non-blocking with spinner animation; concurrent execution requests are rejected with busy notices. | Runtime + Worker thread channel (`mpsc`) |
| **I-25** | *Retired*: Replaced by Registore pre-commit isolation and atomic rename swap. | - |
| **I-26** | `KillRing` is encapsulated, maintaining at most `kill-ring-max` entries per `push`. `from_entries` is test-only. | Data structure (`KillRing`) |
| **I-27** | Syntax highlighting definitions are purely declarative in `SyntaxRegistry`. | Data structure (`SyntaxRegistry`) |
| **I-28..30** | *Retired*: Pruned with low-frequency text commands migrated to Lua. | - |
| **I-31** | Text and EOL mutate strictly through `Editor::apply(Edit)` (updating undo and `modified`) or whole-text replacements that reset undo (I-34). | Runtime (`Editor::apply`, `Buffer::with_text`) |
| **I-32** | `Editor` internal state (`Settings` and private `Editor` fields) is private to `editor`; cross-cutting transitions compose exclusively in `Editor` methods. | Module privacy (`crates/neonemacs-core/src/editor/`) |
| **I-33** | *Retired*: Renumbered as I-31. | - |
| **I-34** | Whole-buffer text replacements (`Buffer::with_text`) wipe undo history to prevent replaying stale offsets against replaced content. | Runtime (`Buffer::with_text`) |
| **I-35** | The undo byte budget, including inverse backing bytes, is governed strictly by `Config.undo_limit`; `UndoLog` maintains no internal limit state, and `close_with_limit` evicts oversized closed groups when `limit > 0`. | Type (`UndoLog::record(edit, limit)`, `UndoLog::close_with_limit(limit)`) |
| **I-36** | `write-file` updates buffer path and name strictly upon `Event::Written` success. | Type (`Event::Written { id, path, result }`) |
| **I-37** | `FilePath` is constructed exclusively through `FilePath::resolve` (lexical folding of `.` and `..`); strictly one buffer visits a given path. | Type (`FilePath`) + Runtime (`commands/file.rs`) |
| **I-38** | `LastCommand::Yank` remains valid strictly until the immediately following command or edit. | Runtime (`Editor::apply`) |
| **I-39** | Command-line parsing is deterministic via single-pass `parse_args`. `-v` and `-h` exit 0 without initializing subsystems. `--` treats remaining flags as files. Unknown flags exit 2. | Type (`Invocation`) + Boundary (`parse_args`) |
| **I-40** | *Retired*: Renumbered as I-39. | - |
| **I-41** | An undo group opened on clean text records the save epoch. Undoing the entire group clears `modified` iff the epoch matches. | Runtime (`Editor::undo`) |
| **I-42** | `Backing::Scratch` is instantiated strictly for the unique buffer named `*scratch*`. | Runtime (`BufferPool`, `switch-to-buffer`) |
| **I-43..45** | *Retired*: Scratch WAL slot, stale WAL quarantine, and WAL probe removed in favor of Registore. | - |
| **I-46** | Modal confirmations (`Confirm`, `query-replace`) handle ASCII case folding (`y`/`n`/`q`), Korean aliases (I-48), and `Esc`/`C-g` cancellations. Unrecognized keys reprompt without state mutation. | Runtime (`answer`, `query_key`) |
| **I-47** | Active mark forms the active region; selected window inverts active region and current search/replace match; pending prefix keys echo `C-x-` or `ESC-`. | Type (`Mark`) + Runtime (`render::frame`) |
| **I-48** | Key aliases map a single character to an ASCII graphic character. Applied strictly to modal confirmation prompts and fallback prefix/modifier lookups; never to regular text insertion, prompts, or search queries. | Type (`AliasTarget`) + Runtime (`answer_char`, `lookup_binding`) |
| **I-49** | `SyntaxDef` requires at least one extension or filename. Filename exact match takes precedence over extension. `heading` prefix colors entire line as keyword without further tokenization. | Runtime (`SyntaxDef::new`, `highlight_line`) |
| **I-50** | `SyntaxDef` maintains keyword and type token lists sorted and deduplicated for binary search lookup. | Runtime (`SyntaxDef::new`) |
| **I-51** | Line lexer is stateless and operates strictly on one line at a time. | Runtime (`LineLexer`) |
| **I-52** | Configuration settings are members of the closed `Setting` enum. `get` is total, `set` is exhaustive, and `set(s, get(s))` preserves state equality. | Type (`Setting`) + Runtime round-trip tests |
| **I-53** | Panic room invariants (terminal restoration, save conflict checks, confirmations, `C-g`/`Esc` cancel) cannot be disabled by configuration, Lua code, or key unbinding. | Type (`Setting` closed set) + Runtime (`C-g` priority) |
| **I-54** | Starting configuration and Lua state is either bundled scripts plus entire `init.lua`, or bundled scripts alone. `reload-init` results in either entire new state or existing state. Partial execution is never observed. | Runtime (`configure`, `ShellSession::reload_init`) |
| **I-55** | An unapplied `init.lua` is never silent: rejection logs full source to `*init-log*` and the first error line to the echo area. | Type (`InitSource`, `InitLoad`) + Boundary (`read_init`) |
| **I-56** | Startup notices never overwrite earlier notices: init error and file notices are concatenated with `; `. | Runtime (`Editor::append_message`) |
| **I-57** | Lua runtime accesses host filesystem and processes strictly via exposed editor APIs; sandbox restricts access to base, `string`, `table`, `math`, and `utf8` without `dofile` or `loadfile`. | Boundary (`lua::new_state`) |
| **I-58** | Successful `reload-init` swaps configuration and Lua runtime while preserving buffers, windows, and kill ring. Rejected or Lua-invoked reloads preserve existing state. | Type (`Editor::adopt_settings`) + Runtime (`reload_init`) |
| **I-59** | Core top-level crate module references are strictly acyclic, except the documented cycle between `commands` and `dispatch`. | Purity scan (`crates/neonemacs-core/tests/purity.rs`) |

### 5.1 Weave Tree Data Structure Invariants (`D-01`..`D-05`)

| ID | Invariant Definition | Enforcement Mechanism |
| :--- | :--- | :--- |
| **D-01** | **Unit Boundary & Strict UTF-8 Integrity**: Every `ByteSlice` endpoint, split point, edit endpoint, and boundary-checked query endpoint lies on a valid `Unit` boundary. Edits, slice streams, and `offset_to_line` return `DocError::InvalidBoundary` for offsets inside a unit; raw byte reads (`byte_at`) may read inside a unit. Only RFC 3629 UTF-8 sequences decode as `Unit::Char`; all other bytes remain `Unit::Raw(u8)`. Adjacent `Unit::Raw` bytes (up to 3B) across edit seams are re-merged into `Unit::Char` (`k_left`/`k_right` 3-way and 2-way seam merge) without splitting any `Unit` across slice boundaries. | Boundary validation + `compute_seam_merge` (`crates/weave-tree/src/utf8.rs`, `proptest`) |
| **D-02** | **Tree Balance, Fanout & Chunk Bounds**: All leaves share equal depth; non-root nodes hold `[MIN_FANOUT, MAX_FANOUT]` children (`16..=32`), root `Leaf` holds `1..=MAX_FANOUT`, root `Internal` holds `2..=MAX_FANOUT`, empty `Doc` has `root == None`. Every `ByteSlice` satisfies `1 <= range.len() <= MAX_SLICE_BYTES` (`16384`). | `from_slices`, `split_at`, `join` (`crates/weave-tree/src/tree.rs`, `assert_invariants`) |
| **D-03** | **Summary Metadata Consistency**: `ByteSlice.summary` equals `Summary::from_slice(&buf[range])`; `ChildEntry.(bytes, summary)` equals the sum of its subtree. `Doc::total_bytes()` and `total_lines(&doc)` equal `(bytes, 1 + total_metric())` of `root` (`(0, 1)` when empty). | `summarize_leaf`, `summarize_internal` (`crates/weave-tree/src/tree.rs`, `assert_invariants`) |
| **D-04** | **Operation Reversibility & Position Independence**: Every successful `apply`/`replace` returns an inverse `Replace` such that `next_doc.apply(inv)` succeeds and restores `doc' == doc`. `Replace` stores only logical offsets and an immutable `Doc`, eliminating dangling references across tree rebalancing. | `Doc::apply` + `weave_tree_proptest` |
| **D-05** | **Observational Purity & Core Isolation**: `Doc` forbids `std::io` and `&mut` in all signatures, return types, and persistent struct fields (`I-18`), exposing bidirectional zero-copy streaming via `ControlFlow<B>` closures (`try_for_each_slice`, `try_for_each_slice_rev`). | `tests/purity.rs` (`I-14`, `I-18`) |

---

## 6. Recording Design Choices & Evolving Context

- **Embedded Contextual Rationale**: Avoid formal, disconnected ADR artifacts. Design choices, constraints, and trade-offs are integrated directly into the relevant domain models (§2), lifecycles (§3), or rationale topics (§4) so reasoning resides alongside the concepts it governs.
- **Living Context & Living Updates**: When requirements, assumptions, or technical realities shift, rewrite the context and rationale directly in place to reflect the current baseline. Rationale must evolve naturally with the code rather than freezing historical states; historical deltas remain preserved in git.
- **Invariant Synchronization**: Any alteration to behavioral or structural contracts must immediately update §5 with verifiable enforcement mechanics and an immutable invariant ID.
