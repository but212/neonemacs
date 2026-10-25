# ADR 0002: Functional core, imperative shell

- Status: accepted
- Date: 2026-10-24
- Spec: `docs/spec.md` §2, §7, §8

## Context

Command functions performed file, process, and Lua I/O directly. Tests therefore needed real files for the save and revert paths. The save check order (I-04) and the save result (I-05) were spread across command functions. The Lua `run` function re-entered the editor from inside a Lua call, with no written rule about borrows or ordering.

## Decision

- `src/core/` holds the editor state and its pure transitions. It must not import `std::fs`, `std::process`, `mlua`, or `crossterm` (I-14). The check is a grep over `src/core`.
- `core::step(Editor, Event) -> (Editor, Effects)` applies one event and returns the new editor state and the effects the shell must perform.
- `src/core/` takes no `&mut` in any signature (I-18). State is passed by value: `Builtin = fn(Editor, Effects) -> (Editor, Effects)`.
- The shell owns the `Editor`. A Lua call reaches it through `RefCell<Option<Editor>>`: `step` takes the value out and puts it back, so the cell is `None` only during `step` (I-21).
- `Effect`: `StatDisk`, `WriteFile`, `ReadFile`, `RemoveWal`, `RunShell`, `RunLua`.
- `Event`: `Key`, `Run`, `Disk`, `Written`, `FileRead`, `WalRemoved`, `ShellDone`, `LuaDone`.
- `src/shell/` performs effects. `perform` answers each effect with exactly one event (I-15). The `match` is exhaustive, so a new effect fails to compile until it is handled.
- Quit is state (`ed.quit`), not an effect.
- A Lua `run` of a built-in command calls `step` synchronously and performs its effects inside the same Lua call. The cell holds no borrow across a Lua call (I-17); `step` never calls Lua, so the cell is `None` only inside `step` (I-21). Nested `run` calls are limited to 50 levels; deeper calls return a Lua error.

### Save sequence

1. `Key` → `save_buffer` emits `StatDisk`. A truncated file asks first and emits nothing (I-04).
2. `Disk` → if the disk state changed since the read, ask; otherwise emit `WriteFile` (I-04, I-13).
3. `Written` → record the new disk state (I-05) and show `Wrote <path>`. No further effect.

### Invariant locations

| ID | Invariant | Location |
| --- | --- | --- |
| I-04 | Save checks: truncated, then external change | `core::commands::save_buffer`, `disk_checked` (runtime) |
| I-05 | A successful write records the new disk state | `Event::Written` carries `DiskState` (type) |
| I-13 | `WriteFile` is emitted only after I-04 or a confirmation | `save_buffer`, `confirmed` (runtime) |
| I-14 | `core` imports no I/O crate | grep check (runtime) |
| I-15 | One effect, one event | exhaustive `match` in `shell::perform` (type) |
| I-17 | No `RefCell` borrow is held across a Lua call | `shell::perform_in`, Lua `run` (runtime) |
| I-18 | No `&mut` in any `src/core` signature (parameters, return types, `&mut self`, type aliases, fields). `let mut` in bodies is allowed | `tests/core/purity.rs` (runtime, text scan) |
| I-19 | The selected window index is below the number of `Layout` leaves | `Editor::window`, `Editor::with_window` (runtime) |
| I-20 | An id passed to `Editor::with_buffer` exists in `buffers` | `Editor::with_buffer` (runtime) |
| I-21 | The shell's `RefCell<Option<Editor>>` is `None` only during `step` | `shell::step_in`, take/put pair (runtime) |

## Consequences

- I-18 is a text scan, not a type. Rust has no type-level ban on `&mut` in a signature, so the scan is the enforcement. It does not see `&mut` used inside a body.
- Core unit tests can check effects without touching files (for example, a save first requests `StatDisk`). File-backed tests live in `tests/`.
- Core unit tests may drive the shell as a test harness. The core itself does not depend on the shell.
- Each effect needs a `core` arm and a `shell` arm. The compiler checks both.
- The model is synchronous. All effects of one event finish before the event returns. A long shell command blocks the editor, as before.
- The undo log is in memory only. Persistent undo (`.<name>.undo`, formerly I-10 and I-16) was removed, so `Effect` and `Event` no longer carry undo variants.
- Behavior changes from the previous implementation: a Lua command that runs itself now stops with a Lua error at depth 50, where it used to crash the process. The undo boundary for Lua commands closes when the Lua call ends.
