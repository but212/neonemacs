# ADR 0016: init.lua loads whole or not at all, and can be reloaded

- Status: accepted
- Date: 2026-11-10
- Spec: `docs/spec.md` §4, §12, §15

## Context

Drawing the user flow of changing `init.lua` showed seven faults in how the file was loaded:

1. A file that could not be read (permission, a directory, bytes that are not UTF-8 such as CP949) was treated like a missing file: no message.
2. Start-up notices were combined in two ways. The scratch recovery notice followed an earlier message, but a file notice replaced an `init.lua` error.
3. A runtime error left the statements before it applied, while a syntax error applied nothing.
4. Only the first line of an error was shown, once, in the echo area.
5. `default-eol` never reached `*scratch*`, which was made before `init.lua` ran.
6. Spec §12 said that file I/O is not public, but the Lua state loaded the whole standard library (`io`, `os`, `package`).
7. The chunk had no name, so an error did not say `init.lua:<line>`.

A change to the file also needed a restart to try.

## Decision

**The read is classified once.** `read_init` returns `InitSource`: `Absent` (no config directory, or `NotFound`), `Unreadable { reason }` (any other I/O error, or bytes that are not UTF-8), or `Loaded(text)`.

**`configure(cwd, source)` builds a configured editor.** It returns the `Editor`, the `Lua` state, and an `InitLoad` (`Absent`, `Applied`, or `Rejected(text)`):

| `source` | Editor and Lua state | `InitLoad` |
| --- | --- | --- |
| `Absent` | bundled scripts only | `Absent` |
| `Unreadable { reason }` | bundled scripts only | `Rejected(reason)` |
| `Loaded`, runs without error | bundled scripts plus the whole file; `*scratch*` takes `default-eol` unless `init.lua` edited it | `Applied` |
| `Loaded`, fails | the candidate is dropped and the bundled state is built again | `Rejected(whole error text)` |

A Lua state cannot be copied or rolled back, so a failed candidate is thrown away instead of repaired. The second run of the three bundled scripts happens only on failure.

**An error is kept in full.** `record_init_error` writes the whole text to the buffer `*init-log*` (no file, not modified, not displayed) and adds `init.lua: <first line>` to the echo area. The chunk is named `=init.lua`, so the first line of an error that Lua raises holds `init.lua:<line>:`. An error that an API function returns (for example `set` with an unknown name) holds its line only in the traceback, so `configure` puts the innermost `init.lua:<line>:` frame in front of the text. There is no separate log command: `switch-to-buffer` opens the buffer. A buffer that visits a file is never taken for `*init-log*` or `*Shell Command Output*`, even when the file has that name. When both exist, `switch-to-buffer` and `kill-buffer` pick the buffer without a file (`BufferPool::find_named`).

**Notices are joined in one place.** `Editor::append_message` joins a notice to the message already shown with `; `. The `init.lua` error, the scratch recovery notice, and the file notices use it, in that order.

**The Lua state is restricted.** `lua::new_state` loads only `string`, `table`, `math`, and `utf8` next to the base functions, and removes `dofile` and `loadfile`. `io`, `os`, `package`, `require`, `coroutine`, and `debug` do not exist.

**`reload-init` reads the file again.** The command returns `Effect::ReloadInit`, answered by `Event::InitLoaded(Option<InitLoad>)`. `handle_interactive` hands the effect to the session, as it does `RunShell`, because the Lua state is borrowed while effects run. `ShellSession::reload_init` calls `configure` and, only for `Applied` or `Absent`, moves the new `Settings` into the running editor (`Editor::adopt_settings`) and replaces the Lua state. Everywhere else `perform` answers `InitLoaded(None)`: a Lua command runs inside the state that a reload would replace.

| `InitLoaded` after `reload-init` | Settings and Lua state | Echo area |
| --- | --- | --- |
| `Some(Applied)` | replaced | `Loaded init.lua`; `*init-log*` is emptied |
| `Some(Absent)` | replaced by the bundled ones | `No init.lua`; `*init-log*` is emptied |
| `Some(Rejected(text))` | unchanged | `init.lua: <first line>`; `*init-log*` holds `text` |
| `None` | unchanged | `reload-init: not available from Lua` |

Rejected alternatives: a `Notice` list type for start-up messages (the echo area is one string and the list had one use), a `view-init-log` command (the buffer is the view), and resetting the settings in place before a reload (moving the `Settings` of a new editor does the same with no reset code).

## Invariants

- **I-54** (new; Runtime): after start-up the settings are the bundled scripts plus all of `init.lua`, or the bundled scripts alone; after `reload-init` they are all of the new result or the previous settings.
- **I-55** (new; Type + Boundary): an `init.lua` that exists but is not applied is never silent at start-up. A rejected one is logged in full and echoed with its `init.lua:<line>:` location.
- **I-56** (new; Runtime): a start-up notice never replaces an earlier one.
- **I-57** (new; Boundary): Lua code reaches the file system and processes only through the API of spec §12.
- **I-58** (new; Type): a `reload-init` that applies gives the settings of a fresh start with the same file and keeps the documents and the interaction state.

I-56 is a runtime invariant because the echo area is a single `String`: the type cannot say which part came from which notice. `append_message` is the only function that joins notices.

## Consequences

- **Breaking:** an `init.lua` that uses `io`, `os`, `require`, `dofile`, `loadfile`, or `coroutine` now fails as a whole. A function such as `os.date` needs a new API function.
- **Breaking:** an `init.lua` with an error in the middle used to keep the statements before the error. It now applies nothing.
- **Breaking:** an `init.lua` that is not UTF-8 used to be ignored without a message. It is now reported.
- Effects that `init.lua` causes outside the editor state (for example `run("save-buffer")` writing a file) are not undone when the file is rejected.
- On reload, buffer edits, messages, and `run("save-buffers-kill-emacs")` made by `init.lua` are dropped with the candidate documents; only the settings move. `default-eol` reaches `*scratch*` only at start-up.
- A reload drops Lua commands and Lua globals that the new file does not define again. Keys bound to a dropped command report `No command named ...`.
- The error of a Lua command that fails later, when its key is pressed, is still shown by its first line only (`lua_done`); it is not written to `*init-log*`.
- `*init-log*` appears in the buffer list after the first rejected load.
- `print` remains (it is a base function) and writes to the terminal behind the screen; use `message`.
