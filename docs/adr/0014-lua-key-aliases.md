# ADR 0014: Lua key aliases

- Status: accepted
- Date: 2026-11-06
- Spec: `docs/spec.md` §3, §8, §9, §12, §15

## Context

`answer_char` in `src/core/dispatch.rs` hardcoded the two-set jamo `ㅛ`/`ㅜ`/`ㅂ` for yes-or-no questions and query-replace (I-46). Spec §15 listed this as a limit: no other input layout was supported, and each new layout needed a core change. ADR 0004 and ADR 0005 moved syntax rules and default keys out of the core into bundled Lua. Layout knowledge follows the same rule.

The same layout gap hits bindings. With a Korean two-set layout active, the key after a prefix arrives as a jamo, so `C-x b` is `C-x ㅠ` and `M-x` is `M-ㅌ`. `handle_normal_key` found no binding and reported `C-x ㅠ is undefined`. Answers and bindings need the same layout-to-QWERTY mapping, so one table serves both.

## Decision

Two global Lua functions:

| Function | Signature | Effect |
| --- | --- | --- |
| `key_alias` | `key_alias(from: string, to: string)` | Makes the character `from` stand for the ASCII key `to`. Re-aliasing replaces the old alias. |
| `key_unalias` | `key_unalias(from: string)` | Removes the alias for `from`. An absent alias is a no-op. |

- `from` is exactly one character. `to` is exactly one ASCII graphic character (`!`..=`~`).
- Core types in `src/core/keymap.rs`: `AliasTarget` (one ASCII graphic character; `AliasTarget::parse` is the only constructor from text) and `KeyAliases`, an ordered map `char -> AliasTarget`. `Settings` holds `key_aliases`, empty by default.
- `KeyAliases::rewrite(key)` returns the key with its character replaced, modifiers kept (`ctrl_key` normalization when `ctrl`), or `None` for a non-character key or an unaliased character.
- **Answers:** `answer_char` returns the alias target for the character or its single-character lowercase form (so an alias for `н` also answers `Н`), else the character, then folds ASCII case. A target such as `Q` therefore answers `q`. Callers: `dispatch::answer` and `commands::query_key`.
- **Bindings:** `handle_normal_key` looks up the exact keys first. On a miss, and only when the key is prefixed (`C-x`, `Esc`) or carries `ctrl` or `meta`, it looks up the keys again with the last key rewritten. An explicit binding such as `C-x ㅠ` wins. A key that stays unbound is reported by its original name.
- The bundled `keymap.lua` registers the full two-set table: 26 letters and the 7 Shift jamo (`ㅃ`→`Q`, `ㅉ`→`W`, `ㄸ`→`E`, `ㄲ`→`R`, `ㅆ`→`T`, `ㅒ`→`O`, `ㅖ`→`P`). The core has no layout knowledge. A bundled script failure panics at start-up, so the defaults cannot vanish silently.
- Aliasing an ASCII character (for example `key_alias("y", "n")`) is allowed. It is the user's choice.

## Error contract

- A `from` that is not exactly one character, or a `to` that is not one ASCII graphic character, raises a Lua error at the boundary.
- A failed call changes nothing.

## Invariants

- **I-46:** `y`/`n`/`q` fold ASCII case. `Esc` cancels. Other layouts answer through key aliases (I-48).
- **I-48:** A key alias maps one character to one ASCII graphic character. It applies only where a modal question is answered and to the binding lookup of a prefixed or modified key after an exact miss. It never applies to self-insert, prompt input, or search text. Other characters answer only through `KeyAliases`. Enforcement: Type (`AliasTarget`), Boundary (`key_alias`), Runtime (only `answer_char` and `lookup_binding` call the table). The call sites cannot be expressed in a type, so this part stays at runtime.

## Consequences

- With a Korean layout active, `C-x ㅠ`, `Esc ㅌ`, and `Alt+ㅌ` run `C-x b` and `M-x`. Users add other layouts with one `key_alias` line each in `init.lua`.
- A core editor without the bundled scripts (tests, headless use) has no aliases. Tests that need them set aliases explicitly.
- Bindings look up by exact match, so a layout with case pairs needs both cases registered. Answers keep the single-character lowercase fold.
- Aliases do not reach prompt input, incremental search, or `C-g`. Extending to those needs a new ADR.
- Terminals and IMEs differ in what `Alt` plus a jamo sends. A terminal that sends nothing usable is a limit (§15).
