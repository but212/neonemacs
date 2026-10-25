# ADR 0001: Lua public API

- Status: accepted
- Date: 2026-10-24
- Spec: `docs/spec.md` §12

## Context

Spec §12 limits the public API to key bindings, command registration, and settings. A command can only do useful work if it can also run other commands. Without `run`, a Lua command can only change settings. Public API is a compatibility promise, so the set is kept small and fixed here.

## Decision

Five functions, global in Lua:

| Function | Signature | Effect |
| --- | --- | --- |
| `bind` | `bind(keys: string, command: string)` | Binds a key sequence such as `"C-x C-f"` or `"M-x"` to a command name. Re-binding replaces the old binding. |
| `command` | `command(name: string, fn: function)` | Defines a command. `name` must not be a built-in name. |
| `run` | `run(name: string)` | Runs a built-in or Lua command, as if the user had invoked it. |
| `get` | `get(name: string) -> integer \| boolean` | Reads a setting by its spec name, such as `"fill-column"`. |
| `set` | `set(name: string, value: integer \| boolean)` | Writes a setting. Type and range are checked. |

Settings: `fill-column` (1–65535), `tab-width` (1–16), `case-fold-search` (boolean), `max-file-size` (bytes, at least 1; a file longer than this is refused on open, ADR 0009), `undo-limit` (bytes, at least 0).

## Error contract

- Every invalid input raises a Lua error at the boundary: an unparseable key string, an unknown command, an unknown setting, a wrong value type, an out-of-range value, or a `command` name that is already built in.
- A failed call changes nothing.
- An error in `init.lua` shows its first line in the echo area. The editor still starts.
- An error in a command shows its first line in the echo area.

## Scope

Not public: buffer text, event hooks, buffer, window, and file-system access. Adding any of these needs a new ADR.

## Consequences

- The API is small enough to keep compatible across versions.
- Lua state is per process. `init.lua` runs once at startup.
- Lua commands run inside the editor's undo grouping, the same as built-in commands.
