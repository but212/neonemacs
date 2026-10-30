# ADR 0010: Save-As Transitions, Path Identity, and Bounded History

- Status: accepted
- Date: 2026-10-26
- Spec: `docs/spec.md` §6, §7, §8, §12

## Context

A review of the whole repository (`/lean-review repo`) found eleven issues. Six change observable behavior.

1. **`write-file` dropped the WAL before writing.** The old WAL was removed first, and the buffer took the new path before the write ran. A failed write left unsaved edits with no WAL.
2. **Replacing the shell output buffer kept its undo log.** `show_output` called `with_text`, which kept the old undo history. Undo then deleted bytes at old offsets in the new text.
3. **One file could have two buffers.** `neonemacs a.txt ./a.txt`, or `find-file` of an already-visited path, created a second buffer. Their WALs then overwrote each other.
4. **The undo limit was stored twice.** `Config.undo_limit` and each `UndoLog` held a copy, so `set("undo-limit", n)` did not reach existing buffers.
5. **Yank offsets went stale.** After a Lua `buffer.insert`, `yank-pop` deleted the range it had recorded for the old text.
6. **The kill ring had no bound.** Every kill stayed in memory.

Three more are documentation or dead-code drift: the spec said `run` depth 64 while the code and ADR 0002 say 50; several functions and types were dead or only tested; `SavePrecondition` and `ShellNotice` were indirection with one useful case.

## Decision

1. **Save-as transitions after success.** `write_file` only requests the write. `Event::Written { id, path, result }` carries the target path. On `Ok`, `written` moves the buffer to `path` (name and backing), and removes the WAL of the old path if the path changed. On `Err`, it changes nothing.
2. **Replacing text outside `apply` resets undo.** `Buffer::with_text` starts a new `UndoLog`. `Editor::replace_text(id, text)` adds the window snap, and `show_output` uses it.
3. **File paths are resolved once into `FilePath`.** `FilePath` has one constructor, `FilePath::resolve(cwd, input)`. It joins the input to `cwd`, drops `.` components, and folds each `..` into the component before it. A `..` with nothing to fold stays only in a relative path. It is lexical, so no link is followed and the core stays free of I/O (I-14). The working directory is `Settings.cwd`, set by `Editor::with_cwd` in `main`. It is empty in tests, so relative paths keep their spelling there.
   - Empty cwd (`/lean-review diff` D2): `main` is the only production caller of `with_cwd`, so the binary always has an absolute cwd. `Settings.cwd` stays empty by default, and `Editor::new` keeps its signature. A second production entry point takes the cwd in `Editor::new`.
   - `request_open`, `find_file`, `write_file`, `written`, `file_read`, and the startup recovery path all resolve through `Editor::resolve_path`.
   - `Backing::File.path` is a `FilePath`. Effects and events keep `PathBuf`. Core rebuilds the `FilePath` from the echoed path, which is idempotent for an already resolved path.
4. **One buffer per path.** `file_read(Open)` reuses the buffer that visits the path. `show` switches to it, and `line` does not move it. `write_file` refuses a path that another buffer visits and sets `Buffer <name> is visiting <path>`.
5. **One undo limit.** `UndoLog` has no `limit` field. `UndoLog::record(edit, limit)` takes `Config.undo_limit`, passed from `Editor::apply`. Buffer and pool constructors lose their `undo_limit` argument.
6. **A yank ends at the next applied edit.** `Editor::apply` sets `LastCommand::Yank` to `Other` before it changes text. A refused edit (read-only) changes nothing and keeps the yank.
7. **The kill ring is bounded.** `KILL_RING_MAX = 120`. `KillRing::push` drops the oldest entry when the ring is full.
8. **Spec matches code.** `run` depth is 50 (`MAX_RUN_DEPTH`), and `docs/spec.md` says 50.
9. **Dead surface removed.** Deleted: `Editor::{set_buffers, set_keymap, set_prev}`, `WindowManager::from_layout`, `CommandRegistry::iter`, the unused `_lua` parameter of `install_command_api`, `BufferPool::find_by_id`, `Text::new`, `Keymap::unbind_str`, `SyntaxRegistry::{detect_lang, find, is_keyword, is_type, line_comment}`, `SavePrecondition`, `check_save_precondition`, `file_state`. `ShellNotice` becomes a `bool` (`already_running`). `line_comment_for_filename` uses `find_by_filename` directly.

## Invariants

- **I-34 (runtime, `Buffer::with_text`):** The undo log describes the current text. A whole-text replacement outside `Editor::apply` resets it. Enforced in one place: `Buffer.text` is private, so every replacement goes through `with_text`.
- **I-35 (type, `UndoLog::record`):** The undo limit is `Config.undo_limit` only. `UndoLog` stores no limit, so there is no second copy to diverge.
- **I-36 (type, `Event::Written`):** Save-as changes backing, name, and the old WAL only after the write succeeds. A failed write leaves the buffer state as it was.
- **I-37 (type + runtime):** The only constructor of `FilePath` is `resolve`, and its output is lexically normal: no `.`, no `..` after a root, and only leading `..` in a relative path. One buffer per path is checked at the two points that can create a second one: `file_read(Open)` and `write_file`. The check is runtime because uniqueness across buffers cannot be expressed in one value. The absolute-path guarantee holds only when `Settings.cwd` is absolute (Decision 3).
- **I-38 (runtime, `Editor::apply`):** `LastCommand::Yank` is valid only until the next applied edit. `last` is in Interaction and the text is in Documents, so `Editor` (I-32) joins them.
- **I-26 (extended):** The kill ring keeps at most `KILL_RING_MAX` (120) entries. `KillRing::from_entries` skips that check, so it is `#[cfg(test)]`, and a production ring grows only through `push`.
- **I-25 (extended):** `write-file` deletes the WAL of the old path only after the write succeeds.
- **I-31 (revised):** Text and EOL change through `Editor::apply(Edit)`, or through a whole-text replacement that resets undo (I-34).

## Consequences

- Behavior change: a failed `write-file` keeps the buffer name and its WAL. Before, the name changed and the WAL disappeared.
- Behavior change: a file visited twice shows one buffer. `write-file` to another buffer's file is refused with a message.
- Behavior change: `Wrote <path>` and the file names in messages show the resolved absolute path when the path was relative.
- Behavior change: a `..` in the input is folded. From cwd `/w`, `../w/a.txt` is `/w/a.txt`, the same buffer as `a.txt`.
- Known limit (D1): folding is lexical. `a/../b` folds to `b` even when `a` is a symbolic link to another directory, where the OS would resolve it differently. Symbolic links are not followed.
- Behavior change: `yank-pop` after an edit reports `Previous command was not a yank` instead of changing text.
- Behavior change: `set("undo-limit", n)` applies to every buffer at its next edit.
- Behavior change: the kill ring drops the oldest entry past 120.
- API change (internal): `Event::Written` has a `path` field. `Buffer::with_undo` is removed. `UndoLog::new` and `record` change signature. `BufferPool::{new, add, kill}` and `Buffer::{new, replace_from_disk, apply_edit}` lose or gain the limit argument. `Documents::new` takes no limit. `KillRing::from_entries` is `#[cfg(test)]`.
- Accepted deviations from the plan: `BufferPool::is_empty` stays, because clippy's `len_without_is_empty` lint applies to a public `len`. `restore_buffer_content` keeps its existing path through `BufferPool::restore_content`, which already resets undo via `with_text`, instead of calling `replace_text`.
- Not in scope: a notice when a WAL cannot be written, double WAL reads in `has_wal`, symbolic-link resolution, and Lua API changes beyond the shell output buffer.
