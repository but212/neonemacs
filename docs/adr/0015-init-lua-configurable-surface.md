# ADR 0015: Everything outside the panic room is configurable from init.lua

- Status: accepted
- Date: 2026-11-10
- Spec: `docs/spec.md` §6, §12, §15

## Context

The requirement: every feature except the panic room can be turned on, turned off, or changed from `init.lua`. Before this ADR `set` knew six names, matched as strings in three places (`get`, `set`, the type-error arms). Other behavior was fixed in the core: the shell program, how wide ambiguous characters are, the default line ending, the kill ring size, and a `name == "markdown"` branch in the highlighter that broke the rule of ADR 0004 (the core has no language knowledge).

Not every constant is a feature. A setting that only exposes a constant with no user need is cost without benefit, and a behavior that a user can already replace (`command`, `bind`, `unbind`, `syntax.register`) needs no second switch.

## Decision

**Panic room** = the safety nets that stop data loss or a broken terminal: terminal restore, WAL write and recovery (location, delay, scratch persistence), the save-time disk check, the overwrite, quit, kill and revert confirmations, and `C-g` and `Esc` cancel. No setting, Lua function, or unbinding turns these off. `C-g` is handled before the keymap, so `unbind` cannot remove it.

**Turning a command off** is `unbind` of its key. There is no `disable` API, and built-in names stay non-redefinable (ADR 0001).

**Settings are a closed sum type.** `Setting` replaces string matching:

| Piece | Rule |
| --- | --- |
| `Setting::parse(name)` | The only place a name becomes a `Setting`. Error: `unknown setting: {name}`. |
| `Config::get(&self, Setting) -> Value` | Total. |
| `Config::set(&self, Setting, Value) -> Result<Config, String>` | Exhaustive match, returns a new `Config`, so a failed call changes nothing. |
| `Value` | `Int(i64) \| Bool(bool) \| Str(String)`. Strings parse into domain types at `set` (`Eol`, non-empty strings). |

New settings: `kill-ring-max`, `amalgamating-undo-limit`, `syntax-highlight`, `isearch-wrap`, `echo-prefix`, `default-eol`, `shell-file-name`, `shell-command-switch`, `ambiguous-width`. Names, values, and defaults are in spec §12.

Plumbing:

- `KillRing::push(bytes, max)` takes the limit; `kill_ring.rs` no longer holds a constant.
- `Effect::RunShell { command, shell: ShellSpec }` carries the configured program and switch. The core stays pure; the shell only runs what the effect says.
- `Metrics { tab, cjk }` in `width.rs` replaces the bare tab width in width functions. They take `impl Into<Metrics>`, and `From<usize>` gives narrow ambiguous characters. `Config::metrics()` builds it. There is no global width state.
- `Editor::add_buffer` gives a new buffer `default-eol`; a file read then sets the detected EOL. `Eol::from_raw(raw, default)` uses `default` only when the file has no line break.
- `SyntaxDef.heading_prefix` (Lua `heading`) replaces the `markdown` branch. The bundled `syntax.lua` registers markdown with `heading = "#"`.

**Out of scope** (spec §12, "새 기능 후보"): settings that only expose a constant (`face-*`, truncation glyph, spinner glyphs) and behavior with no alternative yet (`scroll-margin`, smart-case, line wrapping, and similar). `comment-padding` is not added: `comment-dwim` is a Lua command and can be registered again.

## Invariants

- **I-52** (new; Type + Runtime): every setting is one `Setting` variant; `set(s, get(s))` yields an equal `Config`.
- **I-53** (new; Type + Runtime): nothing disables a panic-room behavior. The list is in spec §12.
- **I-11** (extended; Runtime boundary): string settings parse into domain types at `set`, and a failed `set` changes nothing.
- **I-26** (revised; Data structure): the limit is `kill-ring-max` (default 120) and applies on each `push`.
- **I-49** (extended; Runtime): a definition with `heading_prefix` draws a matching line whole as a keyword and highlights nothing else.

I-53 holds by type for settings and by dispatch order for `C-g`. A future key that is handled before the keymap is a new panic-room entry and must be listed in spec §12.

## Consequences

- Lowering `kill-ring-max` keeps extra entries until the next kill; they drop on that `push`.
- `Config` is no longer `Copy` (it holds strings); callers clone.
- `default-eol` does not reach `*scratch*`, which exists before `init.lua` runs (spec §15).
- A shell program that does not exist makes `shell-command` report `Cannot run command: ...`; the editor keeps running.
- A markdown-like language needs `heading = "..."`; a definition with `heading` does no keyword or string highlighting.
- Adding a setting means adding one `Setting` variant, its default, its range check in `set`, and one spec row. The round-trip test covers it automatically.
