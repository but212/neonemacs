# ADR 0009: Refuse Oversized Files, Undoable EOL Toggle, and Query-Replace Read-Only Check

- Status: accepted
- Date: 2026-10-26
- Spec: `docs/spec.md` §7, §8, §9, §10, §14, §15

## Context

Three gaps were found while reviewing save safety and line endings.

1. **Data loss on large files.** A file longer than `max-file-size` was read only in part and kept as `LoadExtent::Truncated`. A confirmed save then wrote the head-only buffer over the whole file (atomic replace), dropping the tail.
2. **`toggle-eol` was half-implemented.** It changed the save encoding through `Buffer::set_eol`, which bypassed the undo log, and the WAL header did not record the EOL. After a crash the recovered buffer fell back to the file's EOL.
3. **`query-replace` checked read-only status late.** A `view_only` buffer started a session, and the first `y` was refused with a Delete that could not be followed by its Insert.

Multi-line syntax highlighting is a separate, larger change (it revises ADR 0004 and I-22) and is out of scope here.

## Decision

1. **Oversized files are refused at open.** `max-file-size` is an open limit. A file with `len > max-file-size` produces `TooLarge { len, max }` and no buffer. The shell does not read its bytes; it reports only the length. The partial-read state is removed from the type: `LoadExtent`, `Backing::File.extent`, `Buffer::confirm_truncated`, and `Confirm::SaveTruncated` are deleted, so `loaded_from` returns `Result<Loaded, TooLarge>`.
   - `Revert` of a file that grew past the limit leaves the buffer and its WAL unchanged.
   - A successful revert emits `Effect::RemoveWal` after the reload. The shell no longer removes the WAL before reading, so a refused revert keeps its unsaved edits recoverable.
   - Startup WAL recovery runs only when the open created a buffer.
2. **EOL toggle is an edit.** `Edit::SetEol { from, to }` changes `Buffer::eol` and leaves the text unchanged. It is recorded in the undo log, sets `modified`, and sets `wal_dirty`, and it goes through `Editor::apply` like every other change. `apply` also refuses it on a `view_only` buffer.
3. **WAL v2 carries the EOL.** The magic changes from `NEONWAL1` to `NEONWAL2`. The header gains one byte after the magic: `0` for LF, `1` for CRLF. Any other value invalidates the header. `write_wal` takes the EOL, `read_wal` returns `Recovered { eol, bytes }`, and `restore_buffer_content` sets the EOL.
   - Old `.wal` files are ignored. Their content is an unsaved draft, so they are not migrated.
4. **Query-replace checks read-only once.** `start_query_replace` returns `Err("Buffer is read-only")` for a `view_only` buffer before any prompt answer is applied. `view_only` cannot change during a session, so a replacement's Delete is never refused halfway. `replace_match` keeps its `None` arms as defensive code.
   - Documented: the whole session is one undo group, and `C-g`/`q` keep the replacements made so far.

## Invariants

- **I-04 (revised):** The save sequence checks external change only. Oversized files never reach save because they are refused at open. The check is enforced by the type: `loaded_from` returns `Result`, and no partial-buffer variant exists.
- **I-23 (revised):** The WAL header stores the buffer's EOL (`0` = LF, `1` = CRLF, otherwise invalid).
- **I-31 (new):** Buffer state that save and recovery depend on (text and EOL) changes only through `Editor::apply(Edit)`. Undo, `modified`, and `wal_dirty` are updated there. `Buffer::set_eol` is used only when a file is loaded, reverted, or restored.
- **I-06 (reused):** `view_only` is checked by `apply` and at query-replace start.

## Consequences

- Behavior change: a file longer than `max-file-size` cannot be opened or edited. The user must raise the limit (`set("max-file-size", n)`) and reopen it. This is safer than saving a partial file.
- Behavior change: `toggle-eol` appears in undo history, and a toggle followed by a crash now recovers the EOL.
- Breaking API (approved): `LoadExtent` and `Confirm::SaveTruncated` are removed; `Backing::file(path, disk)` and `loaded_from(..) -> Result<_, TooLarge>` change signature; `wal::write_wal`, `wal::encode_wal`, `wal::read_wal` change signature; `restore_buffer_content` gains an `eol` argument.
- Rejected: an `Edit::Replace` variant for query-replace. It would add a variant only to represent a state that the read-only check makes unreachable.
- Rejected: loading the head of an oversized file. It makes the save path unsafe and needs a confirmation that the code cannot enforce.
