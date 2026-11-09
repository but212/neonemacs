# System Invariants

> Single source for the current wording of every invariant. If an ADR's wording differs, this file governs; ADRs record when and why an invariant changed.

Rules:

- An ID is never reused. New IDs are appended after the highest one.
- A retired invariant keeps its row, marked `[Pruned]`, with the ADR that retired it.
- A change to an invariant updates its row here and adds an entry (`new`, `revised`, `extended`, or `pruned`) to the `## Invariants` section of the ADR that makes the change (`docs/adr/README.md` §4).

| ID | Invariant Summary | Enforcement Layer | Governed By |
| :--- | :--- | :---: | :--- |
| **I-01** | Cursor and mark are always snapped to valid UTF-8 boundaries in `0..=len` | Runtime (`snap_anchors`) | ADR 0002, ADR 0006 |
| **I-02** | Gap buffer byte boundaries are strictly local and continuous | Data structure | `src/core/text.rs` |
| **I-03** | A buffer is clean only when its text equals the text last read from or written to disk; edits set `modified`, and only save, revert or reload, `mark_clean`, or an undo that meets I-41 clears it | Runtime (`Buffer::apply_edit`, `record_saved`, `replace_from_disk`, `mark_clean`, `restore_clean`) | ADR 0013, `src/core/buffer.rs` |
| **I-04** | Save sequence checks external disk change, including a file created after a missing path was opened; `write-file` over an existing file asks first; files over `max-file-size` are refused at open, so no partial buffer exists; `WriteFile` is emitted only after this check or a confirmation (formerly I-13) | Type (`loaded_from` → `Result`) / Runtime; Runtime (`save_buffer`, `confirmed`) | ADR 0002, ADR 0009 |
| **I-05** | Successful disk write records fresh metadata (`DiskState`) | Type (`Event::Written`) | ADR 0002 |
| **I-06** | Read-only buffers reject mutations and EOL toggling | Runtime (`Editor::apply`, query-replace start; `view_only`) | ADR 0009, Spec §10 |
| **I-07** | [Pruned] merged into I-06 | Pruned | ADR 0009 |
| **I-08** | At least one buffer exists at all times; closing retargets windows safely | Runtime (`BufferPool`) | ADR 0002, Spec §5 |
| **I-09** | Consecutive kills append to the same kill-ring entry | Runtime (`append_last`) | ADR 0002, ADR 0006 |
| **I-10** | [Pruned] Persistent undo verified against disk state (persistent undo removed) | Pruned | ADR 0002, Spec §7 |
| **I-11** | Configuration settings validate ranges and types at ingress boundary; a string setting parses into its domain type (`Eol`, a non-empty string) there, and a failed `set` changes nothing | Runtime boundary (`Config::set`) | ADR 0001, ADR 0015 |
| **I-12** | Key binding syntax accepts only valid single keys or `C-x` 2-key sequences | Parser boundary | ADR 0001, ADR 0005 |
| **I-13** | [Pruned] merged into I-04 | Pruned | ADR 0002 |
| **I-14** | `src/core` never imports I/O crates (`fs`, `process`, `mlua`, `crossterm`) | Text scan (`tests/core/purity.rs`) | ADR 0002 |
| **I-15** | Every `Effect` is answered by exactly one `Event` (1:1 correspondence) | Type (`shell::perform`) | ADR 0002 |
| **I-16** | [Pruned] `WriteUndo` bytes encoded from `DiskState` (persistent undo removed) | Pruned | ADR 0002 |
| **I-17** | [Pruned] merged into I-21 | Pruned | ADR 0002 |
| **I-18** | Zero `&mut` in any `src/core` signature (parameters, returns, fields) | Pure scan (`tests/core/purity.rs`) | ADR 0002 |
| **I-19** | Selected window index is strictly bounded: `selected < layout.leaf_count()` | Runtime (`WindowTree`) | ADR 0002 |
| **I-20** | Any buffer id passed to `with_buffer` exists in the buffer pool | Runtime | ADR 0002 |
| **I-21** | The shell's `RefCell<Option<Editor>>` is `None` only during `step`, and no borrow of it is held across a Lua call or while an effect runs (formerly I-17) | Runtime (`shell::step_in` take/put, `perform_in`, Lua `run`) | ADR 0002 |
| **I-22** | ANSI color escapes never distort text display width, coordinates, or diff | Type / Render queue | ADR 0003 |
| **I-23** | WAL persistence is an asynchronous shell concern; core transitions remain pure. The header records the EOL (`NEONWAL2`) | Runtime (`wal.rs`) | ADR 0003, ADR 0009 |
| **I-24** | Background shell execution is non-blocking with active spinner feedback | Runtime / Thread | ADR 0003 |
| **I-25** | Regular save, revert, or close immediately deletes sibling WAL file; `write-file` deletes the old path's WAL only after its write succeeds | Runtime (`wal.rs`) | ADR 0003, ADR 0010 |
| **I-26** | Kill ring history is encapsulated in `KillRing` with ring rotation invariants, and after each `push` holds at most `kill-ring-max` (default 120) entries; `from_entries` (no check) is test-only | Data structure | ADR 0006, ADR 0010, ADR 0015, Spec §6 |
| **I-27** | Syntax definitions are purely declarative in `SyntaxRegistry` | Data structure | ADR 0004 |
| **I-28** | [Pruned] Line counting semantics (superseded by ADR 0006 Pareto Spec Normalization) (ID not reused; syntax rules are I-49 to I-51) | Pruned | ADR 0006, ADR 0007 |
| **I-29** | [Pruned] UTF-8 inspection slicing (superseded by ADR 0006 Pareto Spec Normalization) (ID not reused; syntax rules are I-49 to I-51) | Pruned | ADR 0006, ADR 0007 |
| **I-30** | [Pruned] Cursor inspection coordinates (superseded by ADR 0006 Pareto Spec Normalization) (ID not reused; syntax rules are I-49 to I-51) | Pruned | ADR 0006, ADR 0007 |
| **I-31** | Text and EOL change only through `Editor::apply(Edit)`, which updates undo, `modified`, and `wal_dirty`, or through a whole-text replacement that resets undo (I-34); `edit_seq` is owned by Documents and changed only there (formerly I-33) | Runtime (`Editor::apply`, `Buffer::with_text`, `Buffer::mark_clean`) | ADR 0009, ADR 0010 |
| **I-32** | `Editor` groups (Documents, Interaction, Settings) are private to `editor`; a group never writes another group's fields, and cross-group operations are composed in `Editor` methods | Module privacy (`src/core/editor/`) | ADR 0002 |
| **I-33** | [Pruned] merged into I-31 | Pruned | ADR 0002 |
| **I-34** | Replacing a buffer's text outside `Editor::apply` resets its undo log, so undo describes the current text | Runtime (`Buffer::with_text`) | ADR 0010 |
| **I-35** | The undo limit is `Config.undo_limit` only; `UndoLog` stores no limit | Type (`UndoLog::record(edit, limit)`) | ADR 0010 |
| **I-36** | `write-file` changes the buffer's backing, name, and old WAL only after a successful write | Type (`Event::Written { id, path, result }`) | ADR 0010 |
| **I-37** | `FilePath` has one constructor, `resolve`, which folds `.` and `..` lexically; one buffer visits one path (checked in `file_read(Open)` and `write_file`) | Type (`FilePath`) + Runtime (`commands/file.rs`) | ADR 0010 |
| **I-38** | `LastCommand::Yank` is valid only until the next applied edit | Runtime (`Editor::apply`) | ADR 0010 |
| **I-39** | A version or help request carries no file list, and `parse_args` is the only place that classifies `-`-prefixed arguments; `--` makes later `-`-prefixed arguments files, so the classification depends on argument order (formerly I-40) | Type (`Invocation`) + Boundary (`parse_args`) | ADR 0011 |
| **I-40** | [Pruned] merged into I-39 | Pruned | ADR 0011 |
| **I-41** | An undo group records the save epoch if it opened on clean text; undoing the whole group restores clean only at that epoch, and a clean buffer never holds modified text. Text equal to the saved text may stay modified after the log was trimmed or replaced | Runtime (`Editor::undo`) | ADR 0013 |
| **I-42** | `Backing::Scratch` is made only for the buffer named `*scratch*`, and at most one exists | Runtime (`BufferPool`, `switch-to-buffer`) | ADR 0012 |
| **I-43** | The scratch WAL slot exists only while a scratch buffer holds unsaved text (a crash may leave it) | Runtime (`ShellSession`) | ADR 0012 |
| **I-44** | An unusable WAL is never deleted by inference; it is moved to `.stale` (replacing an older `.stale`) and reported before the next flush | Boundary (`wal::probe_file`) | ADR 0012 |
| **I-45** | A WAL is looked for in one place, when a file read creates a buffer, so every way to open a file recovers | Type (`Effect::ProbeWal` made only in `file_read(Open)`) | ADR 0012 |
| **I-46** | In a yes-or-no question and in query-replace every key either changes the state or says why it did not; `y`/`n`/`q` fold ASCII case; `Esc` cancels; other layouts answer through key aliases (I-48) | Runtime (`answer`, `query_key`) | Spec §8, §9, ADR 0014 |
| **I-47** | Only an active mark makes a region, and only the selected window draws the region and the current search match in reverse video | Type (`Mark`) + Runtime (`render::frame`) | Spec §5, §6 |
| **I-48** | A key alias maps one character to one ASCII graphic character. It applies only where a modal question is answered and to the binding lookup of a prefixed or modified key after an exact miss, never to self-insert, prompt input, or search text. Other characters answer only through `KeyAliases`; bundled `keymap.lua` maps the two-set layout, so `ㅛ`/`ㅜ`/`ㅂ` answer | Type (`AliasTarget`) + Boundary (`key_alias`) + Runtime (`answer_char` and `lookup_binding` call sites) | ADR 0014 |
| **I-49** | A `SyntaxDef` has at least one extension or filename; a file or buffer name resolves to a definition by exact basename match in `filenames` first, then by extension; a definition with a `heading_prefix` draws a line that starts with it whole as a keyword and highlights nothing else | Runtime (`SyntaxDef::new`, `SyntaxRegistry::find_by_filename`, `highlight_line`) | ADR 0004, ADR 0015 |
| **I-50** | A `SyntaxDef` keeps its keyword and type lists sorted and deduplicated (ASCII-lowercased when case-insensitive), so lookups use binary search | Runtime (`SyntaxDef::new`) | ADR 0004 |
| **I-51** | The line lexer is stateless and works on one line at a time | Runtime (`LineLexer`) | ADR 0003, ADR 0004 |
| **I-52** | Every configuration setting is one `Setting` variant: `get` is total over `Setting`, `set` matches it exhaustively, and `set(s, get(s))` yields an equal `Config` | Type (`Setting`) + Runtime (`Setting::ALL` round-trip test) | ADR 0015, Spec §12 |
| **I-53** | No `Setting` variant, Lua function, or key unbinding disables a panic-room behavior: terminal restore, WAL write and recovery, the save-time disk check, the overwrite, quit, kill and revert confirmations, and `C-g` and `Esc` cancel. The list is in spec §12 | Type (`Setting` is closed) + Runtime (`C-g` is handled before the keymap) | ADR 0015, Spec §12 |
