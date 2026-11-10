# Domain Context

> Domain terms, architectural boundaries, and the design rationale in force.

Which document governs what:

- `docs/spec.md`: observable behavior, commands, settings, and messages.
- `docs/spec.md` §16: the invariants that change what a user sees (`I-xx`). §5 below: the internal ones.
- This file: the vocabulary, the boundaries, and the design rationale behind both.

If this file and the spec disagree on behavior, the spec governs. If they disagree on an invariant, the list that holds it (spec §16 or §5 below) governs.

## 1. Architecture Boundaries

Two layers, with effects flowing from the core down to the shell and events flowing back up, one `Event` per `Effect` (I-15).

- **Core** (`crates/neonemacs-core`, the crate `neonemacs-core`, exposed as `neonemacs::core`, whose root re-exports the types the shell uses and keeps every other module private): editor state and its pure transitions, `step(Editor, Event) -> (Editor, Effects)`. It does no I/O (I-14) and has no `&mut` in any signature (I-18). A built-in command is `Builtin = fn(Editor, Effects) -> (Editor, Effects)`. `Editor` is `Documents`, `Interaction`, and `Settings` (I-32).
- **Shell** (`src/shell`): owns the `Editor`, performs each `Effect`, and answers it with one `Event`. It holds everything that touches the outside: terminal and ANSI output (`session/screen.rs`), atomic writes (`atomic.rs`), the WAL (`wal.rs`), the background shell thread, Lua 5.4 (`lua/` and the bundled `syntax.lua`, `text.lua`, `keymap.lua`), and start-up.
- **`Effect`**: `StatDisk`, `WriteFile`, `ReadFile`, `RemoveWal`, `FlushWal`, `ProbeWal`, `RunShell`, `RunLua`, `ReloadInit`.
- **`Event`**: `Key`, `Run`, `Disk`, `Written`, `FileRead`, `WalRemoved`, `Idle`, `WalFlushed`, `WalProbed`, `ShellDone`, `LuaDone`, `InitLoaded`.
- **`Editor` groups** (`crates/neonemacs-core/src/editor/`), each private to `editor`:
  - `Documents`: `BufferPool`, `WindowManager`, `KillRing`, `edit_seq`, and the scratch slot path.
  - `Interaction`: `Mode`, `last`/`prev` `LastCommand`, the echo-area message, and the quit flag.
  - `Settings`: `Config`, `Keymap`, `KeyAliases`, `SyntaxRegistry`, `CommandRegistry`, `cwd`.

## 2. Glossary

### Buffers and files

- **`Buffer`**: text (`Text`, a gap buffer over raw bytes, read as `Unit`s), cursor (`saved_point`), `Mark`, `Eol`, `UndoLog`, `Backing`, `modified`, `view_only`, and the save epoch.
- **`Unit::Raw`**: a byte that is not valid UTF-8, kept as itself so the file saves unchanged.
- **`Backing`**: what stands behind a buffer.
  - `None`: no file, for example `*Shell Command Output*`.
  - `Scratch { wal_dirty }`: the one `*scratch*` buffer (I-42).
  - `File { path, disk, wal_dirty }`.
- **`DiskState`**: `Known { mtime, size }` as last read or written, or `Unknown` (a new file or the scratch slot).
- **`FilePath`**: an absolute, lexically folded path, made only by `FilePath::resolve(cwd, input)` (I-37).
- **`StatTarget`**: why a disk state is read. `Save(id)` compares it with the recorded state. `SaveAs(id)` asks before overwriting an existing file.
- **`ReadTarget`**: why a file is read. `Open { line, show }` makes a new buffer. `Revert(id)` reloads one.
- **`TooLarge`**: the result of opening a file longer than `max-file-size`. No buffer is made (I-04).
- **`Confirm`**: a yes-or-no question kind: `SaveExternal`, `OverwriteFile`, `QuitUnsaved`, `KillModified`, `RevertModified`.
- **`view_only`**: a per-buffer flag that refuses edits, `toggle-eol`, and query-replace (I-06).
- **`Eol`**: `Lf` or `Crlf`, chosen at read time by majority. Memory holds `\n` only.

### Edit history

- **`Edit`**: `Replace { pos, removed, inserted }` or `SetEol { from, to }`. It is the only way text and EOL change through `Editor::apply` (I-31).
- **`UndoLog`**: `Group`s of edits, bounded by `undo-limit`, which the log does not store (I-35).
- **`Group`**: one undo unit. Its `clean_epoch` is the buffer's save epoch if the group opened on clean text (I-41).
- **Save epoch**: `Buffer::epoch`. It changes whenever the text that `modified` is measured against changes (`record_saved`, `replace_from_disk`, `mark_clean`, `with_text`).
- **Clean / modified**: clean means the text equals the text last read from or written to disk (I-03). Status line `--` / `**`.
- **`LastCommand`**: the kind of the last command, for commands that chain (`Insert`, `Undo`, `Kill`, `Vertical`, `Yank`).
- **`KillRing`**: kill history, at most `kill-ring-max` entries after each `push` (I-26).

### Recovery

- **WAL**: `.<name>.wal` next to a file. It holds unsaved text written after one idle second, with a `NEONWAL2` header that records the disk state and the EOL.
- **`wal_dirty`**: set by an edit, cleared by a successful flush. It exists only in `Backing::File` and `Backing::Scratch`.
- **Scratch slot**: `$XDG_STATE_HOME/neonemacs/scratch.wal`, else `~/.local/state/neonemacs/scratch.wal`. It holds the unsaved scratch text (I-43).
- **Stale WAL**: a WAL that does not match its file (changed file, damaged, old format). It is moved to `<wal>.stale` and never deleted (I-44).
- **`WalTarget`**: where a WAL lives: `Sibling(path)` is the `.<name>.wal` beside a file, `Slot(path)` is the scratch slot itself. `Editor::wal_target` gives it for a buffer, and none for a buffer that cannot have a WAL.
- **`WalProbe`**: the answer to `ProbeWal`: `Absent`, `Valid(Recovered)`, or `Stale(Result<PathBuf, String>)`.

### Keys and syntax

- **`Keymap`**: key sequences to command names. `Keymap::survival()` holds the core's fixed bindings, so the editor works without Lua. Bundled `keymap.lua` adds the rest.
- **`KeyAliases` / `AliasTarget`**: a map from one character to one ASCII graphic character, used for modal answers and for prefixed or modified binding lookups after an exact miss (I-48).
- **`SyntaxDef` / `SyntaxRegistry`**: declarative language rules (extensions, filenames, line comment, keywords, types, case folding, `heading_prefix`), registered from Lua (I-27, I-49, I-50).
- **Bundled scripts**: `syntax.lua`, `text.lua`, `keymap.lua`, embedded with `include_str!` and run in that order before `init.lua` (`BUNDLED_SCRIPTS`).

### Configuration and start-up

- **`Setting`**: the closed set of setting names. `Setting::parse` is the only way from a name (I-52).
- **`Value`**: `Int`, `Bool`, or `Str`. A string parses into its domain type at `Config::set` (I-11).
- **`Config`**: setting values, with booleans packed in `Toggles`.
- **`Metrics`**: tab width plus ambiguous-width mode, passed to width functions.
- **`ShellSpec`**: the program and switch carried by `Effect::RunShell`.
- **Panic room**: safety behavior that nothing can disable (I-53, spec §12): terminal restore, WAL write and recovery, the save-time disk check, the overwrite, quit, kill, and revert confirmations, and `C-g`/`Esc` cancel.
- **`Invocation`**: `Run(Vec<FileArg>)`, `Version`, or `Help`, from `parse_args` (I-39).
- **`InitSource`**: the classified `init.lua` read: `Absent`, `Unreadable { reason }`, or `Loaded(text)`.
- **`InitLoad`**: how `init.lua` was applied: `Absent`, `Applied`, or `Rejected(text)` (I-54).
- **`*init-log*`**: the buffer holding the whole text of the last rejected `init.lua` (I-55).
- **Echo area / notice**: the one-line message. Start-up notices are joined with `; ` by `Editor::append_message` (I-56).

## 3. Lifecycles

- **Save**: `save-buffer` emits `StatDisk { Save }`. On `Disk`, an unchanged state emits `WriteFile`; a changed one asks `SaveExternal`. `Written` records the new `DiskState` (I-05), bumps the epoch, and emits `RemoveWal`. `write-file` uses `StatDisk { SaveAs }`, asks `OverwriteFile` for an existing file, and moves the buffer to the new path only on `Written` success (I-36).
- **Open**: `find-file` and start-up files emit `ReadFile { Open }`. `FileRead` either reuses the buffer visiting the path, refuses with `TooLarge`, or makes a buffer and emits `ProbeWal` (I-45). `WalProbed` restores a valid WAL as modified text or reports a stale one.
- **Idle flush**: one second after the last edit, the session sends `Idle` (`ShellSession::wal_deadline` is the only clock). The core answers with one `FlushWal` that holds every `wal_dirty` buffer that has a WAL; a dirty scratch with no slot is cleared at once. `WalFlushed` clears the buffers that were written. A failed write keeps the buffer dirty, so the session sends `Idle` again a second later; the failure is echoed only in `Mode::Normal`, so a pending question stays visible.
- **`init.lua`**: `read_init` → `InitSource` → `configure` builds a candidate `Editor` and Lua state and keeps it only if the whole file ran. `reload-init` runs `configure` again; on `Applied` or `Absent` the session moves the new `Settings` in (`Editor::adopt_settings`) and replaces the Lua state (I-58).

## 4. Design Rationale

Each topic gives what is in force and why, then what was rejected or accepted as a limit. Cited `I-xx` are in spec §16 or §5 below.

### 4.1 Core, shell, and output

- The core is pure and takes no `&mut` in any signature. State moves by value through `step`. The shell owns the `Editor` in a `RefCell<Option<Editor>>`; `step` takes it out and puts it back, so no borrow lives across a Lua call. A Lua `run` of a built-in calls `step` synchronously and performs its effects in the same Lua call. Nested `run` stops at depth 50 (`MAX_RUN_DEPTH`) with a Lua error. (I-01, I-04, I-05, I-08, I-14, I-15, I-18 to I-21, I-32)
- Why: commands that did I/O directly needed real files in tests and spread the save checks across functions. Effects make every save step testable without files, and the exhaustive `match` in `perform` makes a new effect fail to compile until the shell handles it.
- Rejected: persistent undo (`.<name>.undo`), removed with its effects; undo is in memory only.
- Boundary: the core is its own crate, so `mlua`, `crossterm`, and the shell are unreachable from it at build time. `neonemacs` re-exports it as `core` (`pub use neonemacs_core as core`). The core's `lib.rs` is the only public surface: every module is private except `effect` and `render`, which are the protocol and the frame the shell paints, and `lib.rs` re-exports the types and functions the shell and `tests/` use, so a path is `neonemacs::core::Editor`, not `neonemacs::core::editor::Editor`. A test that needs a name outside that list moves into the core (`crates/neonemacs-core/src/scenarios/`) instead of widening it. The core's unit tests use `neonemacs-core`'s own `test_support`, which runs `step` and panics on any effect; a test that needs effects performed lives in `tests/` and uses the root `test_support`. Why: a module boundary inside one crate is enforced only by convention and a text scan, while a crate boundary and private modules are enforced by the compiler. Rejected: adding `Event` variants for `shell::startup` calls to `request_open`, `switch_to`, and `record_init_error` (they are pure calls whose effects the shell performs, so I-14 and I-15 already hold). Limit: `std::fs` and `std::process` cannot be cut by crate, so the text scan stays. Accepted cycle: `commands` and `dispatch` call each other, because `M-x` and the end of a search run a command again by name or key (I-59).
- Command files: `commands/file.rs` holds the save check flow and `commands/load.rs` holds reading, opening, reverting, and WAL recovery; both are re-exported from `commands`.
- Limit: Rust cannot ban `&mut` in a signature by type, so `crates/neonemacs-core/tests/purity.rs` scans the text. It does not see `&mut` inside a body or behind a macro.
- `Frame` holds glyph and `TokenKind` per `Cell`. Color escapes are produced only when the shell queues terminal output (`src/shell/session/screen.rs`), so widths, columns, cursor coordinates, and `render::diff` never see escape bytes and wide characters stay aligned. (I-22, spec §5)
- `shell-command` runs on a `std::thread` and reports through `std::sync::mpsc`. The event loop polls every 50 ms and turns a spinner. A second command while one runs is refused. Why: non-blocking without an async runtime, which would cost binary size (spec §2). Rejected: Tokio or another async runtime; a queue of commands, which could leak threads and lose results silently. (I-24, spec §4)

### 4.2 Files and recovery

- Unsaved text goes to the WAL after one idle second, not on every key, because a write per key breaks the no-input-delay rule (spec §14). Files and WALs are written to a temporary file, synced, and renamed, so power loss never leaves a zero-byte file. The header is `NEONWAL2`, the disk state, and one EOL byte (`0` LF, `1` CRLF, anything else invalid), so a crash after `toggle-eol` recovers the right line ending. A WAL from an older format is not migrated; it is handled as stale. (I-23, I-25, spec §7)
- A file longer than `max-file-size` gives `TooLarge` and no buffer; the shell does not read its bytes. A revert of a file that grew past the limit keeps the buffer and its WAL. A successful revert removes the WAL after the reload, never before. Why: a partial buffer could be saved over the whole file and drop its tail. Rejected: opening the head of the file with a confirmation before save, which the code cannot enforce. (I-04, spec §8)
- `write-file` only requests the write; `Event::Written { id, path, result }` moves the buffer on success and changes nothing on failure. `FilePath::resolve` folds paths lexically against `Settings.cwd`, which start-up (`configure`) sets to the process working directory (empty in tests, so relative paths keep their spelling). One buffer visits one path: `file_read(Open)` reuses it, and `write_file` refuses another buffer's path. Why: a failed save-as lost the WAL, and two buffers on one file overwrote each other's WAL. Limit: folding is lexical, so `a/../b` becomes `b` even when `a` is a symbolic link; links are not followed, which keeps the core free of I/O. (I-25, I-36, I-37, spec §8)
- `*scratch*` has `Backing::Scratch` and a WAL slot in the state directory. The slot is `Documents.scratch_slot`, set by `Editor::with_scratch_slot` at start-up, so tests never touch the real directory. The core emits `RemoveWal(Slot)` where it emits one for a file buffer: when undo makes the scratch clean, when the buffer is killed, and on a confirmed quit; quit after `y` is an explicit discard. A WAL is looked for only when `file_read(Open)` makes a buffer, so every way of opening recovers. An unusable WAL is renamed to `.stale` before any flush and reported. Why: scratch text was lost on crash, a file opened with `C-x C-f` never recovered, and a mismatched WAL was overwritten silently. Accepted risk: no locking, so two editors on the same file or state directory overwrite each other's WAL (spec §15); the editor never deletes `.stale` files. (I-42 to I-45, spec §7)
- Each undo `Group` records the save epoch if it opened on clean text. Undoing a whole group restores clean (and removes the WAL) only at that epoch; a group opened on modified text never restores clean. Why: save, type, undo used to show `**`, keep the WAL, and ask on quit, and a warning that is sometimes false teaches users to ignore it. Rejected: comparing text with the saved text (O(n) per edit up to `max-file-size`); content hashes (state the epoch makes unnecessary). Limit: after the log trims or resets the group, equal text can stay `**` (test `trimmed_history_never_reports_clean`). Revisit: if users report that false `**`, compare content hashes at group boundaries. (I-03, I-41, spec §7)

### 4.3 Editing

- `toggle-eol` is `Edit::SetEol`, so it goes through `Editor::apply`, enters undo, sets `modified` and `wal_dirty`, and is refused on a `view_only` buffer. `start_query_replace` refuses a `view_only` buffer before any answer. The whole session is one undo group, and `C-g`/`q` keep the replacements made so far. Why: a toggle that bypassed undo was lost on crash, and a late read-only check could refuse a replacement's delete after the session started. Rejected: a separate replace edit for query-replace, which would exist only for a state the early check makes unreachable. (I-06, I-31, spec §8 to §10)
- Replacing text outside `apply` (`Buffer::with_text`) resets undo, so undo never replays stale offsets. `undo-limit` lives only in `Config`, so `set` takes effect. An applied edit ends `LastCommand::Yank`, so `yank-pop` never deletes a stale range. (I-26, I-34, I-35, I-38, spec §6)
- `text.lua` implements only the frequent commands: word motion and kill, `kill-line`, buffer ends, `fill-paragraph`, `comment-dwim`, `mark-whole-buffer`, `exchange-point-and-mark`. They use the `buffer` module, whose positions are byte offsets snapped to character boundaries. Why: about 270 lines cover the common editing habits; an earlier 1,100-line version added transposition, sentence and paragraph motion, region case, word counts, cursor inspection, and a `buffer.count_lines` FFI that few used. Rejected: those commands and the FFI; users can add them with `command` in `init.lua`. (I-01, I-09, spec §6)

### 4.4 Lua and configuration

- Lua gets global functions (`bind`, `unbind`, `command`, `run`, `get`, `set`, `key_alias`, `key_unalias`, `message`) and the `syntax` and `buffer` modules listed in spec §12. Every invalid input is a Lua error at the boundary and a failed call changes nothing. Built-in command names cannot be redefined. Lua commands register into `CommandRegistry`, so `M-x` completion lists built-in and Lua commands together. Why: public API is a compatibility promise, so it stays small; `run` is needed so a command can do more than change settings. Rejected: event hooks, access to buffers other than the active one, direct file and process access; each needs a new design choice. (I-11, I-12, spec §4, §12)
- Language rules, default keys beyond the survival set, and layout aliases live in bundled Lua (`syntax.lua`, `keymap.lua`), run before `init.lua` in the order `syntax.lua`, `text.lua`, `keymap.lua`. The core keeps `Keymap::survival()` so it works without Lua. Markdown headings use `heading_prefix` instead of a language-name branch. A bundled script failure panics at start-up, so defaults cannot vanish silently. Why: adding a language or layout should not need a Rust change, and a core without Lua stays testable. Rejected: running Lua per line during redisplay (breaks core purity and speed); regex engines, multi-line block comments, and AST parsing (the lexer stays one stateless pass per line). (I-27, I-49 to I-51, spec §3, §5, §12)
- Every setting is one `Setting` variant. `Config::get` is total, and `Config::set` matches exhaustively and returns a new `Config`. Everything outside the panic room is configurable: through `set`, `bind`/`unbind` (turning a command off is unbinding its key), `key_alias`, `syntax.register`, or by registering a bundled Lua command again. `C-g` is handled before the keymap. Why: string matching in three places drifted, and fixed behavior (shell program, ambiguous width, default EOL, kill ring size) had no switch. Criterion: a setting that only exposes a constant with no user need is cost without benefit, and behavior a user can already replace needs no second switch, so there are no `face-*`, glyph, or `comment-padding` settings and no `disable` API. Adding a setting takes one `Setting` variant, its default, its check in `set`, and one spec §12 row; the `Setting::ALL` round-trip test covers it. (I-11, I-26, I-52, I-53)
- `configure` runs the bundled scripts and `init.lua` on a fresh `Editor` and Lua state, and on failure drops the candidate and builds the bundled state again. A rejection is logged whole in `*init-log*` and echoed with its `init.lua:<line>:` location (chunk name `=init.lua`, innermost frame for API errors). `reload-init` goes through the session, as `RunShell` does, because the Lua state is borrowed while effects run; from inside Lua it answers `InitLoaded(None)`. The Lua state loads only `string`, `table`, `math`, `utf8`, and the base functions without `dofile` and `loadfile`. A buffer that visits a file is never taken for `*init-log*` (`BufferPool::find_named`). Why: a Lua state cannot be copied or rolled back, so a failed candidate is thrown away instead of repaired; partial application, silent unreadable files, and a full standard library contradicted spec §12. Rejected: a `Notice` list type (the echo area is one string); a `view-init-log` command (the buffer is the view); resetting settings in place before a reload (moving a new editor's `Settings` does the same with no reset code). Accepted limit: effects outside the editor that `init.lua` causes, such as a file written by `run("save-buffer")`, are not undone, and a later Lua command error is shown by its first line only. (I-54 to I-58, spec §4, §12, §15)

### 4.5 Keys

- `KeyAliases` serves both modal answers and binding lookups. A lookup tries the exact keys first; only on a miss, and only for a prefixed or `ctrl`/`meta` key, it retries with the last key rewritten. Self-insert, prompt input, and search text never use aliases. The bundled `keymap.lua` registers the two-set Korean layout. Aliasing an ASCII character is allowed. Why: with a Korean layout active, `C-x b` arrives as `C-x ㅠ` and answers as `ㅛ`; both need the same layout-to-QWERTY map, and the map belongs in Lua. Limit: lookups match exactly, so a layout with case pairs registers both cases; what `Alt` plus a jamo sends depends on the terminal. (I-46, I-48, spec §3, §8, §9, §15)

### 4.6 Command line

- `parse_args` reads arguments once, left to right, and returns an `Invocation`. `-v`/`--version` and `-h`/`--help` print and exit `0` before any Lua, WAL, or terminal setup. `--` ends options. Any other `-` argument, a lone `-` included, exits `2` with `unknown option`. Why: typos used to open files named `--versoin`. Rejected: an argument parser crate; three flags do not justify one. Revisit: when there are more than about five flags or an option takes a value. (I-39, spec §12)

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
