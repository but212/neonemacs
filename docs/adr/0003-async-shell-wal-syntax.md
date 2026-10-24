# ADR 0003: Async Shell, Idle WAL Crash Recovery, and 1-Pass Syntax Highlighting

- Status: accepted
- Date: 2026-10-24
- Spec: `docs/spec.md` §4, §5, §7

## Context

neonemacs operates under strict architectural invariants:

- Binary size under 5MB (Spec §2).
- Pure functional core with no `&mut` in signatures (I-18, ADR 0002).
- Zero input latency and minimal cognitive load (Spec §14).

Previously:

- Shell commands (`shell-command`) executed synchronously, completely blocking the terminal UI until process completion.
- Unsaved edits were lost if a crash occurred before saving (Spec §7 limit).
- Code display lacked syntax highlighting, with pure monochrome text rows.

## Decision

### 1. 1-Pass Lightweight Syntax Highlighter (I-22)

- Implemented in `src/core/syntax.rs` without regex backtracking or external parsers.
- Detects keywords, strings, comments, and numbers for Rust, C/C++, Python, JavaScript/TypeScript, Lua, Go, Shell, and Markdown.
- Frame in `src/core/render.rs` models the terminal grid with `rows: Vec<Vec<Cell>>` where each `Cell` encapsulates both display glyph and `TokenKind` syntax style.
- Terminal color escapes (ANSI) are injected only during final terminal queue emission in `main.rs`, ensuring `char_width`, `column`, cursor coordinates, and `render::diff` are completely free from ANSI byte interference (**I-22**).

### 2. Async Shell Task Queue (I-24)

- Uses `std::thread::spawn` and `std::sync::mpsc` without pulling in heavy async runtimes like Tokio.
- `handle_interactive` in `src/shell/mod.rs` intercepts `Effect::RunShell`, dispatches it to a background thread, and immediately returns control to the editor with a `[Running...]` status.
- If a shell command is already active when another is submitted, the new request is rejected with `[Running... <spinner>] A shell command is already running`, preventing background thread leaks and silent result loss (**I-24**).
- The event loop polls crossterm with a 50ms timeout. Background shell tasks rotate a smooth spinner in the echo area and emit `Event::ShellDone` to the core upon completion (**I-24**).

### 3. Idle Delta WAL Crash Recovery (I-23, I-25)

- Does not synchronously write to disk on every keystroke (avoiding latency violations).
- When the user pauses typing for 1 second (idle), the shell persists buffer contents to sibling file `.<filename>.wal` (**I-23**).
- When regular file save (`Event::Written`) succeeds, the corresponding `.wal` file is immediately deleted (**I-25**).
- At startup, if an unmerged `.wal` file is detected, neonemacs restores the unsaved session with a clear notification.

## Invariant Additions

| ID | Invariant | Location |
| :--- | :--- | :--- |
| I-22 | ANSI codes never interfere with text width, cursor position, or diff | `src/core/render.rs`, `src/main.rs` |
| I-23 | WAL persistence is an asynchronous shell concern; core state transitions remain pure | `src/shell/wal.rs`, `src/main.rs` |
| I-24 | Non-blocking shell command execution keeps editor responsive and injects `Event::ShellDone` | `src/shell/mod.rs`, `src/main.rs` |
| I-25 | Regular save success deletes the sibling `.wal` file | `src/shell/mod.rs`, `src/shell/wal.rs` |

## Consequences

- Binary size increased by ~70KB to 992KB, remaining far below the 5MB limit.
- Core purity (I-18) is 100% maintained with 0 signatures using `&mut`.
- UI responsiveness is dramatically improved during long shell commands.
