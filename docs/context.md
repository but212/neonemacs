# Domain Context

> Domain terms, architectural boundaries, and design rationale.

Precedence: `docs/spec.md` governs observable behavior and external invariants (§16). This file governs domain terms, boundaries, internal invariants (§5), and design rationale. In case of conflict, the specific invariant list governs.

## 1. Architecture Boundaries

Two layers: pure core emits effects down to the shell; shell executes effects and returns events up, strictly one `Event` per `Effect` (I-15).

- **Core** (`crates/neonemacs-core`, exposed as `neonemacs::core`): pure transitions `step(Editor, Event) -> (Editor, Effects)`. No I/O (I-14), zero `&mut` signatures (I-18). Root re-exports public types; internal modules remain private. Built-in command signature: `Builtin = fn(Editor, Effects) -> (Editor, Effects)`. `Editor` comprises `Documents`, `Interaction`, and `Settings` (I-32).
- **Shell** (`src/shell`): owns `Editor`, executes `Effect`s, returns `Event`s. Encapsulates all external interaction: terminal/ANSI rendering (`session/screen.rs`), atomic writes (`file.rs`), WAL persistence (`wal.rs`), background worker threads, Lua 5.4 runtime (`lua/`, bundled scripts), and startup orchestration.
- **`Effect`**: `StatDisk`, `WriteFile`, `ReadFile`, `RemoveWal`, `FlushWal`, `ProbeWal`, `RunShell`, `RunLua`, `ReloadInit`.
- **`Event`**: `Key`, `Run`, `Disk`, `Written`, `FileRead`, `WalRemoved`, `Idle`, `WalFlushed`, `WalProbed`, `ShellDone`, `LuaDone`, `InitLoaded`.
- **`Editor` groups** (`crates/neonemacs-core/src/editor/`, private to `editor`):
  - `Documents`: `BufferPool`, `WindowManager`, `KillRing`, `edit_seq`, scratch slot path.
  - `Interaction`: `Mode`, `last`/`prev` `LastCommand`, echo-area message, quit flag.
  - `Settings`: `Config`, `Keymap`, `KeyAliases`, `SyntaxRegistry`, `CommandRegistry`, `cwd`.

## 2. Glossary

### Buffers and files

- **`Buffer`**: text (`Text`, gap buffer of raw bytes read as `Unit`s), cursor (`saved_point`), `Mark`, `Eol`, `UndoLog`, `Backing`, `modified`, `view_only`, save epoch.
- **`Unit::Raw`**: non-UTF-8 byte preserved byte-exact across saves.
- **`Backing`**: backing storage: `None` (unbacked, e.g. `*Shell Command Output*`), `Scratch { wal_dirty }` (the single `*scratch*` buffer, I-42), `File { path, disk, wal_dirty }`.
- **`DiskState`**: `Known { mtime, size }` (last read/write) or `Unknown` (new file, scratch slot).
- **`FilePath`**: absolute, lexically folded path constructed solely via `FilePath::resolve(cwd, input)` (I-37).
- **`StatTarget`**: reason for disk inspection: `Save(id)` (disk conflict check), `SaveAs(id)` (overwrite confirmation).
- **`ReadTarget`**: reason for file read: `Open { line, show }` (new buffer), `Revert(id)` (reload existing).
- **`TooLarge`**: result when file length exceeds `max-file-size`; aborts buffer creation (I-04).
- **`Confirm`**: modal yes/no prompt: `SaveExternal`, `OverwriteFile`, `QuitUnsaved`, `KillModified`, `RevertModified`.
- **`view_only`**: buffer flag rejecting edits, `toggle-eol`, and query-replace (I-06).
- **`Eol`**: `Lf` or `Crlf`, determined by majority vote on read; normalized to `\n` in memory.

### Edit history

- **`Edit`**: `Replace { pos, removed, inserted }` or `SetEol { from, to }`. Sole mutation path via `Editor::apply` (I-31).
- **`UndoLog`**: undo `Group` stack bounded by `undo-limit`; log stores no limit internally (I-35).
- **`Group`**: single undo transaction. Sets `clean_epoch` if opened while clean (I-41).
- **Save epoch**: `Buffer::epoch`. Advances whenever baseline text changes (`record_saved`, `replace_from_disk`, `mark_clean`, `with_text`).
- **Clean / modified**: clean iff text matches disk baseline (I-03); status indicators `--` / `**`.
- **`LastCommand`**: kind of preceding command for chaining operations (`Insert`, `Undo`, `Kill`, `Vertical`, `Yank`).
- **`KillRing`**: kill history retaining at most `kill-ring-max` entries per `push` (I-26).

### Recovery

- **WAL**: `.<name>.wal` sibling file holding unsaved text written after 1s idle delay; starts with `NEONWAL2` header recording `DiskState` and `Eol`.
- **`wal_dirty`**: set on edit, cleared on flush. Exists only in `Backing::File` and `Backing::Scratch`.
- **Scratch slot**: `$XDG_STATE_HOME/neonemacs/scratch.wal` (fallback `~/.local/state/neonemacs/scratch.wal`) for unsaved scratch text (I-43).
- **Stale WAL**: WAL mismatching target file (modified, damaged, obsolete header). Renamed to `<wal>.stale`, never deleted (I-44).
- **`WalTarget`**: WAL location: `Sibling(path)` or `Slot(path)`.
- **`WalProbe`**: answer to `ProbeWal`: `Absent`, `Valid(Recovered)`, `Stale(Result<PathBuf, String>)`.

### Keys and syntax

- **`Keymap`**: key sequences to commands. `Keymap::survival()` provides standalone fallback; bundled `keymap.lua` completes bindings.
- **`KeyAliases` / `AliasTarget`**: character-to-ASCII map for modal answers and fallback lookup on prefixed/modified keys (I-48).
- **`SyntaxDef` / `SyntaxRegistry`**: declarative highlighting definitions registered from Lua (I-27, I-49, I-50).
- **Bundled scripts**: `syntax.lua`, `text.lua`, `keymap.lua` embedded via `include_str!` and executed in sequence prior to `init.lua`.

### Configuration and start-up

- **`Setting`**: closed enum of setting names parsed solely via `Setting::parse` (I-52).
- **`Value`**: `Int`, `Bool`, `Str`. Strings parse to domain types at `Config::set` (I-11).
- **`Config`**: setting values with booleans packed into `Toggles`.
- **`Metrics`**: tab stop width and ambiguous-width mode for display columns.
- **`ShellSpec`**: program and flag specification for `Effect::RunShell`.
- **Panic room**: non-disableable safety invariants (I-53, spec §12): terminal restore, WAL persistence/recovery, save conflict checks, confirmations, and `C-g`/`Esc` cancel.
- **`Invocation`**: `Run(Vec<FileArg>)`, `Version`, `Help` parsed from command line (I-39).
- **`InitSource`**: classified init state: `Absent`, `Unreadable { reason }`, `Loaded(text)`.
- **`InitLoad`**: result of applying `init.lua`: `Absent`, `Applied`, `Rejected(text)` (I-54).
- **`*init-log*`**: buffer containing entire source of rejected `init.lua` (I-55).
- **Echo area / notice**: single-line status bar; startup notices concatenated with `; ` via `Editor::append_message` (I-56).

## 3. Lifecycles

- **Save**: `save-buffer` emits `StatDisk { Save }`. If unchanged, emits `WriteFile`; if altered, asks `SaveExternal`. `Written` updates `DiskState` (I-05), increments save epoch, and emits `RemoveWal`. `write-file` issues `StatDisk { SaveAs }`, prompts `OverwriteFile` on collision, and retargets buffer path only on successful `Written` (I-36).
- **Open**: `find-file` and startup arguments emit `ReadFile { Open }`. `FileRead` reuses existing visited buffer, rejects with `TooLarge`, or creates buffer and emits `ProbeWal` (I-45). `WalProbed` applies valid WAL as modified text or reports stale file.
- **Idle flush**: 1s after edit, session emits `Idle` (`ShellSession::wal_deadline`). Core answers with `FlushWal` for all `wal_dirty` buffers; dirty scratch without slot clears immediately. `WalFlushed` clears dirty flag for written buffers. Failed writes retain dirty state and re-trigger on next idle; error echoed only in `Mode::Normal` to avoid masking prompts.
- **`init.lua`**: `read_init` -> `InitSource` -> `configure` builds candidate `Editor` and Lua state, committing only if entire script succeeds. `reload-init` re-executes `configure`; on `Applied` or `Absent`, session transfers new `Settings` (`Editor::adopt_settings`) and swaps Lua runtime (I-58).

## 4. Design Rationale

### 4.1 Core, shell, and output

- **Core purity**: Core is pure and takes zero `&mut` in signatures; transitions move state by value via `step`. Shell owns `Editor` in `RefCell<Option<Editor>>` and extracts it during `step`, preventing borrows across Lua invocations. Synchronous Lua `run` calls `step` directly; recursion caps at depth 50 (`MAX_RUN_DEPTH`) with Lua error. (I-01, I-04, I-05, I-08, I-14, I-15, I-18 to I-21, I-32)
  - *Why*: Decoupling I/O via effects makes save flows testable in-memory without files; exhaustive effect matching ensures compiler-verified handling.
  - *Rejected*: Persistent undo (`.<name>.undo`) dropped for in-memory undo.
- **Crate boundary**: Core is an isolated workspace crate (`crates/neonemacs-core`, re-exported as `neonemacs::core`). `lib.rs` is sole public surface; internal modules are private except `effect` and `render`. Non-public test needs stay in `scenarios/`. Unit tests use effect-panicking `test_support`; effect-verifying tests live in `tests/`. Compiler enforces crate boundaries; text scan (`crates/neonemacs-core/tests/purity.rs`) guards against `std::fs` and `std::process`. Accepted cycle: `commands` and `dispatch` mutually refer due to `M-x` and search termination (I-59).
- **Command files**: `commands/basic.rs` handles motion, edit, and buffer/window management; `commands/file.rs` governs save verification; `commands/load.rs` handles read/open/revert and WAL recovery; `commands/mod.rs` registers built-in table.
- **Purity scanner limit**: Purity scan inspects signature tokens, not macro expansions.
- **Terminal frame**: `Frame` stores glyphs and `TokenKind` per `Cell`. Terminal escape formatting is deferred to output queue (`src/shell/session/screen.rs`), ensuring column calculations, cursor positioning, and `render::diff` remain escape-free (I-22).
- **Shell commands**: Executed on background thread via `std::sync::mpsc`. Event loop polls every 50ms with spinner. Concurrent execution refused (I-24). Avoids async runtime overhead (spec §2).

### 4.2 Files and recovery

- **WAL persistence**: 1s WAL debounce prevents per-keystroke I/O latency (spec §14). Atomic writes use PID- and sequence-isolated temp files, `sync_all`, and atomic rename. `NEONWAL2` header records disk metadata and EOL byte (`0` LF, `1` CRLF). Legacy WAL versions treated as stale without migration. (I-23, I-25, spec §7)
- **Oversized files**: Files exceeding `max-file-size` return `TooLarge` without reading bytes, preventing partial overwrite truncations. Reverting a file that expanded past limit preserves existing buffer and WAL. (I-04, spec §8)
- **Path folding**: `write-file` moves path only upon `Event::Written` success, preserving WAL on failure. Lexical path folding against `Settings.cwd` ensures one-buffer-per-path invariant without following symlinks (I-25, I-36, I-37, spec §8).
- **Scratch durability**: `*scratch*` uses `Backing::Scratch` and dedicated slot path (`Documents.scratch_slot`). Core emits `RemoveWal(Slot)` on clean-undo, buffer kill, or confirmed quit. Corrupt WALs renamed to `.stale` and never deleted. (I-42 to I-45, spec §7)
- **Undo clean tracking**: Undo groups record save epoch if opened while clean. Undoing entire group restores clean state and removes WAL only if epoch matches (I-03, I-41, spec §7). Revisit: Content hashing at group boundaries if trimmed history false-modifications become problematic.

### 4.3 Editing

- **Safe edit pipeline**: `toggle-eol` (`Edit::SetEol`) passes through `Editor::apply`, logs undo, sets `modified`/`wal_dirty`, and is rejected on `view_only`. `start_query_replace` validates `view_only` upfront. Entire replace session forms one undo group. (I-06, I-31, spec §8 to §10)
- **Undo invalidation**: Whole-buffer replacement (`Buffer::with_text`) clears undo log to prevent stale offset replays. Applying edits clears `LastCommand::Yank` to prevent invalid `yank-pop` ranges. (I-26, I-34, I-35, I-38, spec §6)
- **Lua Pareto commands**: `text.lua` implements high-frequency Pareto editing commands (~270 lines) using byte-offset buffer API snapped to character boundaries. Infrequent operations left to user scripts. (I-01, I-09, spec §6)

### 4.4 Lua and configuration

- **Minimal API boundary**: Global functions, `syntax`, and `buffer` modules. Built-in names protected. Registered Lua commands appear in `M-x` completion. Sandbox restricts access to `string`, `table`, `math`, `utf8`, base functions; excludes `dofile`, `loadfile`, direct filesystem, and process APIs. (I-11, I-12, I-57, spec §4, §12)
- **Bundled initialization**: Declarative language rules and key mappings live in bundled scripts run in fixed sequence (`syntax.lua` -> `text.lua` -> `keymap.lua`). `Keymap::survival()` provides standalone fallback. Startup script errors panic immediately to prevent silent fallback. (I-27, I-49 to I-51, spec §3, §5, §12)
- **Closed settings model**: `Config::get` is total; `Config::set` matches exhaustively. Only user-facing operational levers are exposed; constants without configuration need are omitted. (I-11, I-26, I-52, I-53)
- **Atomic config reload**: `configure` evaluates on fresh state, abandoning candidate on error. Rejection logs full source to `*init-log*` with line location. (I-54 to I-58, spec §4, §12, §15)

### 4.5 Keys

- **Alias fallback**: `KeyAliases` handles modal confirmation mapping and fallback binding resolution for prefixed/modified keys on exact miss. Self-insert, prompt, and search bypass alias rewrites. Bundled Korean two-set layout supported. (I-46, I-48, spec §3, §8, §9, §15)

### 4.6 Command line

- **Deterministic flag parsing**: Single-pass left-to-right parsing via `parse_args`. `-v`/`--version` and `-h`/`--help` exit immediately before subsystem initialization. Unknown options exit with code 2. (I-39, spec §12)

## 5. Internal Invariants

Invariants about structure and types, which no user sees directly. The invariants that change what a user sees are in `docs/spec.md` §16. Both lists share one ID space.

- An ID is never reused. New IDs are appended after the highest one, whichever list they belong to.
- A retired invariant leaves its row. Pruned IDs: I-07, I-13 (merged into I-04 and I-06), I-10, I-16 (persistent undo removed), I-17 (now I-21), I-28 to I-30 (removed with the low-frequency text commands, §4.3), I-33 (now I-31), I-40 (now I-39).
- A change to an invariant updates its row in the list it belongs to.

| ID | Invariant | Enforcement |
| :--- | :--- | :--- |
| **I-02** | Gap buffer byte boundaries are strictly local and continuous | Data structure (`crates/neonemacs-core/src/text.rs`) |
| **I-05** | A successful disk write records fresh metadata (`DiskState`) | Type (`Event::Written`) |
| **I-14** | `neonemacs-core` never imports I/O crates: `mlua`, `crossterm` are not in its dependencies; `fs`, `process` are rejected by a text scan | Crate graph (`crates/neonemacs-core/Cargo.toml`); text scan for `std` (`crates/neonemacs-core/tests/purity.rs`) |
| **I-15** | Every `Effect` is answered by exactly one `Event`. Exception: an interactive session (`handle_interactive`) hands `RunShell` and `ReloadInit` to `ShellSession`; a `RunShell` refused because another one runs gets no `Event`, and one step never yields two of them | Type (`shell::perform`) + runtime (`handle_interactive`) |
| **I-18** | Zero `&mut` in any `neonemacs-core` signature (parameters, returns, fields) | Text scan (`crates/neonemacs-core/tests/purity.rs`) |
| **I-19** | The selected window index is strictly bounded: `selected < layout.leaf_count()` | Runtime (`WindowTree`) |
| **I-20** | Any buffer id passed to `with_buffer` exists in the buffer pool | Runtime |
| **I-21** | The shell's `RefCell<Option<Editor>>` is `None` only during `step`, and no borrow of it is held across a Lua call or while an effect runs | Runtime (`shell::step_in` take/put, `perform_in`, Lua `run`) |
| **I-23** | WAL persistence is an asynchronous shell concern; core transitions remain pure. The header records the EOL (`NEONWAL2`) | Runtime (`wal.rs`) |
| **I-27** | Syntax definitions are purely declarative in `SyntaxRegistry` | Data structure |
| **I-31** | Text and EOL change only through `Editor::apply(Edit)`, which updates undo, `modified`, and `wal_dirty`, or through a whole-text replacement that resets undo (I-34); `edit_seq` is owned by Documents and changed only there | Runtime (`Editor::apply`, `Buffer::with_text`, `Buffer::mark_clean`) |
| **I-32** | `Editor` groups (Documents, Interaction, Settings) are private to `editor`; a group never writes another group's fields, and cross-group operations are composed in `Editor` methods | Module privacy (`crates/neonemacs-core/src/editor/`) |
| **I-35** | The undo limit is `Config.undo_limit` only; `UndoLog` stores no limit | Type (`UndoLog::record(edit, limit)`) |
| **I-50** | A `SyntaxDef` keeps its keyword and type lists sorted and deduplicated (ASCII-lowercased when case-insensitive), so lookups use binary search | Runtime (`SyntaxDef::new`) |
| **I-51** | The line lexer is stateless and works on one line at a time | Runtime (`LineLexer`) |
| **I-52** | Every configuration setting is one `Setting` variant: `get` is total over `Setting`, `set` matches it exhaustively, and `set(s, get(s))` yields an equal `Config` | Type (`Setting`) + Runtime (`Setting::ALL` round-trip test) |
| **I-57** | Lua code reaches the file system and processes only through the API of spec §12: the state loads only `string`, `table`, `math`, and `utf8` next to the base functions, without `dofile` and `loadfile` | Boundary (`lua::new_state`) |
| **I-59** | The `crate::` references between top-level core modules form no cycle except `commands` and `dispatch` | Text scan (`crates/neonemacs-core/tests/purity.rs`) |

## 6. Recording a Design Choice

- A hard-to-reverse or high-trade-off choice is added to the matching §4 topic, with what was chosen, why, what was rejected, and, when known, a revisit trigger and accepted limits. A new area gets a new subsection appended at the end.
- A choice that no longer holds is rewritten to the current state; history stays in git.
- A change to an invariant updates its row in spec §16 or §5 above.
- Behavior that users see goes to `docs/spec.md` in the same change.
