# ADR 0012: Durable Scratch and One WAL Probe

- Status: accepted
- Date: 2026-11-05
- Spec: `docs/spec.md` §7, §12

## Context

Two gaps in crash recovery (ADR 0003, I-23, I-25):

1. **The scratch had no protection.** Only `Backing::File` buffers had a WAL. Text typed into `*scratch*` was lost on a crash, and `C-x C-s` does not save a buffer without a file.
2. **Recovery depended on how a file was opened, and failed silently.** Only start-up arguments looked for a WAL (`open_and_recover_single`). A file opened with `C-x C-f` was never recovered. A WAL whose disk state no longer matched was ignored without a word, and the next idle flush overwrote it.

## Decision

1. **`Backing::Scratch { wal_dirty }`.** The `*scratch*` buffer (`SCRATCH_NAME`) has no path, so `path()` is `None` and the save, revert, and write-file paths do not change. It is made only in `BufferPool::new`, when the last buffer is killed, and by `switch-to-buffer` for a missing buffer named `*scratch*`.
2. **A WAL slot for the scratch.** The session writes the scratch after one idle second to `$XDG_STATE_HOME/neonemacs/scratch.wal`, else `~/.local/state/neonemacs/scratch.wal` (`wal::scratch_path_from`, same shape as `lua::config_path`). The disk state is `Unknown`. `ShellSession::with_scratch_wal(slot)` injects the slot; without it the text stays in memory only, and tests never touch the real state directory.
3. **Lifecycle.** The session removes the slot when no scratch buffer is modified any more (clean again, killed and recreated) and when the editor quits. Quit after `y` is an explicit discard, like for file WALs. At start-up, `restore_scratch` restores a valid slot before the files open and shows `[Recovered *scratch* from WAL]`.
4. **One WAL probe.** `file_read(Open)` emits `Effect::ProbeWal { path, disk, line }` for a buffer it just made, and no other code looks for a WAL on open. The answer is `Event::WalProbed { path, line, probe }` with `WalProbe::{Absent, Valid(Recovered), Stale(Result<PathBuf, String>)}`. The start-up files go through the same path.
5. **Unusable WALs are moved aside.** The shell renames a WAL that does not match (changed file, damaged, old `NEONWAL1`) to `.name.wal.stale`, replacing an older stale copy, and the core shows `Stale WAL kept as .name.wal.stale` or `Stale WAL for NAME not kept: <error>`. The rename happens before any flush can overwrite the draft. The same applies to the scratch slot (`scratch.wal.stale`).
6. **Notices at start-up** are joined with `; `, one per file with a notice. An earlier message (for example an `init.lua` error) stays when no file has a notice.

## Invariants

- **I-42**: `Backing::Scratch` is made only for the buffer named `*scratch*`, and at most one such buffer exists. It is made in three places and the name is unique; enforced at runtime by name lookup in `switch-to-buffer`.
- **I-43**: The scratch slot exists only while a scratch buffer holds unsaved text (a crash may leave it). Enforced in the shell session, since it is file-system state.
- **I-44**: A WAL that cannot be used is never deleted or overwritten by inference. It is moved to `.stale` and reported before the next flush. Enforced at the boundary in `wal::probe_file`.
- **I-45**: A WAL is looked for in one place, when a file read creates a buffer. The effect is made only there, so every way to open a file recovers. Enforced by construction in `file_read(Open)`.

## Consequences

- `C-x C-f` of a file with a valid WAL now opens the recovered, modified buffer. A test that opened a file and then reverted it must answer the discard question first.
- `.stale` files are never removed by the editor. The user deletes them.
- Two editors on the same file, or two on the same state directory, share WAL files. This is not guarded (spec §15).
- A clean quit with an unsaved scratch is an explicit discard: the slot is removed. Only a crash leaves it for the next run.
