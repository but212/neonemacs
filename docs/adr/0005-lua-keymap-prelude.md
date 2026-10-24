# ADR 0005: Bundled Lua keymap prelude and unbind API

- Status: accepted
- Date: 2026-10-25
- Spec: `docs/spec.md` §3, §12

## Context

Prior to this ADR, `src/core/keymap.rs` hardcoded the minimal survival set (25 keys). Many standard built-in commands implemented in the core (`split-window-below`, `split-window-right`, `delete-other-windows`, `other-window`, `switch-to-buffer`, `kill-buffer`, `kill-line`, `write-file`, `view-mode`, `undo`, `fill-paragraph`, `yank-pop`, `isearch-backward`, `query-replace`, `goto-line`, `shell-command`) were unbound to any default key sequence and could only be executed via `M-x`.

Furthermore, Lua only exposed `bind(keys, name)`, with no way to unbind or remove a keybinding completely.

## Decision

- Keep the minimal 25-key survival set in `Keymap::survival()` for headless/core testing and failsafe emergency navigation without Lua.
- Expose pure `Keymap::unbind(self, keys: &[Key]) -> Self` and `Keymap::unbind_str(self, keys: &str) -> Result<Self, KeyError>` in `src/core/keymap.rs`. Zero `&mut` in signatures (I-18).
- Expose global Lua function `unbind(keys: string)` in `src/shell/lua.rs`.
- Extract extended default Emacs keybindings into `src/shell/keymap.lua` and embed it at compile time via `include_str!`.
- In `lua::bootstrap()`, load `syntax.lua` followed by `keymap.lua` before user `init.lua`.
- Users can override any keybinding via `bind(...)` or completely unbind with `unbind(...)` in `init.lua`.

## Keybindings in `keymap.lua`

- `C-x 2` -> `split-window-below`
- `C-x 3` -> `split-window-right`
- `C-x 1` -> `delete-other-windows`
- `C-x o` -> `other-window`
- `C-x b` -> `switch-to-buffer`
- `C-x k` -> `kill-buffer`
- `C-x C-w` -> `write-file`
- `C-x C-q` -> `view-mode`
- `C-k` -> `kill-line`
- `C-x u` -> `undo`
- `M-y` -> `yank-pop`
- `M-q` -> `fill-paragraph`
- `C-r` -> `isearch-backward`
- `M-%` -> `query-replace`
- `M-g` -> `goto-line`
- `M-!` -> `shell-command`

## Error contract

- `unbind(keys)` raises a Lua error if `keys` is not a valid single key or `C-x` 2-key sequence (I-12).
- Calling `unbind` on an unbound key is a safe no-op.

## Consequences

- All standard Emacs window, buffer, formatting, and killing commands are immediately usable without `M-x`.
- Core purity and single-binary packaging remain 100% intact.
