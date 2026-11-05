# ADR 0014: Lua answer aliases

- Status: accepted
- Date: 2026-11-06
- Spec: `docs/spec.md` §8, §9, §12, §15

## Context

`answer_char` in `src/core/dispatch.rs` hardcoded the two-set jamo `ㅛ`/`ㅜ`/`ㅂ` for yes-or-no questions and query-replace (I-46). Spec §15 listed this as a limit: no other input layout was supported, and each new layout would need a core change. ADR 0004 and ADR 0005 moved syntax rules and default keys out of the core into bundled Lua. Answer keys follow the same rule.

## Decision

Two global Lua functions:

| Function | Signature | Effect |
| --- | --- | --- |
| `answer_alias` | `answer_alias(from: string, to: string)` | Makes the character `from` mean the answer `to`. Re-aliasing replaces the old alias. |
| `answer_unalias` | `answer_unalias(from: string)` | Removes the alias for `from`. An absent alias is a no-op. |

- `from` is exactly one character. `to` is one of `y`, `n`, `q`, `!`.
- Core types in `src/core/keymap.rs`: `AnswerKey` (`Yes`, `No`, `Quit`, `All`) and `AnswerAliases`, an ordered map `char -> AnswerKey`. `AnswerKey::parse` is the only constructor from text.
- `Settings` holds `answer_aliases`, empty by default. `answer_char(&AnswerAliases, Key)` returns the alias target if one exists for the character or for its single-character lowercase form (so an alias for `н` also answers `Н`), else the ASCII-lowercased character. Its two callers are `dispatch::answer` and `commands::query_key`.
- The bundled `keymap.lua` registers `ㅛ`→`y`, `ㅜ`→`n`, `ㅂ`→`q`. The core has no layout knowledge. A bundled script failure panics at start-up, so the defaults cannot vanish silently.
- Aliasing an ASCII answer character (for example `answer_alias("y", "n")`) is allowed. It is the user's choice.

## Error contract

- A `from` that is not exactly one character, or a `to` outside `y`, `n`, `q`, `!`, raises a Lua error at the boundary.
- A failed call changes nothing.

## Invariants

- **I-46 (amended):** `y`/`n`/`q` fold ASCII case. Other characters answer only through `AnswerAliases`. `Esc` cancels.
- **I-48:** An answer alias targets only `y`/`n`/`q`/`!` and applies only where a modal question is answered, never to self-insert. Enforcement: Type (`AnswerKey`), Boundary (`answer_alias`), Runtime (only `answer` and `query_key` call `answer_char`).

## Consequences

- Users add layouts such as Cyrillic with one line in `init.lua`.
- A core editor without the bundled scripts (tests, headless use) no longer accepts jamo answers. Tests that need them set aliases explicitly.
- Public Lua API grows by two functions (ADR 0001 lists five; later ADRs added more).
- Aliases do not reach prompt input, incremental search, or `Esc`/`C-g` handling. Extending to those needs a new ADR.
