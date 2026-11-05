# ADR 0013: Save Epoch and Clean-by-Undo

- Status: accepted
- Date: 2026-11-05
- Spec: `docs/spec.md` §7

## Context

`Buffer::apply_edit` set `modified = true` for every edit, undo included. After a save, typing `a` and undoing it left the buffer text equal to the saved text, yet the status line showed `**`, the file's WAL stayed, and `C-x C-c` asked to discard changes that did not exist. A warning that is sometimes false teaches the user to ignore it.

Comparing the text with the saved text costs O(n) per edit, which breaks the no-input-delay principle (spec §14.3) on files up to `max-file-size`. The undo log is bounded (`undo-limit`, I-35) and is reset when the text is replaced outside `apply` (I-34), so the log cannot be the only record of the saved text.

## Decision

1. **Save epoch.** `Buffer` keeps `epoch: u32`. It changes (wrapping add) whenever the text that `modified` is measured against changes: `record_saved`, `replace_from_disk`, `mark_clean`, and `with_text`.
2. **Groups remember the epoch they opened on.** `UndoLog` stores `Group { edits, clean_epoch }`. `clean_epoch` is `Some(epoch)` if the buffer was clean when the group opened, else `None`. `apply_edit` passes it to `UndoLog::record(edit, limit, clean_epoch)`; it is used only when a new group opens.
3. **Undo restores clean.** `Editor::undo` applies the inverse of a whole group. If every inverse edit was applied and `group.clean_epoch == Some(buffer.epoch())`, the text equals the saved text again. The buffer then becomes clean through `Buffer::restore_clean` (which keeps the epoch), and a file-backed buffer emits `Effect::RemoveWal`.
4. **No hash, no new state beyond the epoch.** A group opened on modified text never restores clean, which is the conservative side.

## Invariants

- **I-41**: An undo group records the buffer's save epoch if it opened on clean text. Undoing the whole group restores clean only if that epoch is the buffer's current one. A clean buffer never holds modified text. Text equal to the saved text may stay modified after the log was trimmed or replaced. Enforced at runtime in `Editor::undo`, because it relates a log entry to a later buffer state, which a type cannot express.

## Consequences

- Save, type, undo: the status line returns to `--`, the WAL is removed, and quit asks nothing.
- Undoing an undo reaches clean through the original group (the inverse group opened on modified text and never restores clean itself).
- After the log drops the group that opened on clean text, the same text can still show `**`. The test `trimmed_history_never_reports_clean` pins this limit.
- Revisit if users report this false positive: compare content hashes at group boundaries instead.
