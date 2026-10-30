# ADR 0011: CLI Flags, Version and Help, and End of Options

- Status: accepted
- Date: 2026-10-27
- Spec: `docs/spec.md` §12

## Context

Startup arguments were only `+n` and file paths (`parse_args`). A `-v` or `--help` argument was opened as a file, and a typo such as `--versoin` created a new buffer. Spec §12 said the only start option was `+n`. A user could not print the version or usage without starting the editor, and a file whose name starts with `-` could not be named without a path prefix.

## Decision

1. **`parse_args` returns an `Invocation`.** `Invocation` is `Run(Vec<FileArg>)`, `Version`, or `Help`. `Version` and `Help` carry no files, so they never coexist with a file list.
2. **`-v` and `--version`** print `neonemacs <CARGO_PKG_VERSION>` to stdout and exit `0`. The version comes from `Cargo.toml`, so no build script is needed.
3. **`-h` and `--help`** print the usage text to stdout and exit `0`.
4. **Version and help run before the editor.** `main` handles them before `run`, so no Lua state, `init.lua`, WAL recovery, or raw terminal mode happens.
5. **`--` ends option parsing.** Later arguments that start with `-` are files. `+n` rules are unchanged.
6. **Any other `-`-prefixed argument is an error.** This includes a lone `-`. It prints `unknown option: <arg>` and exits `2`.
7. **Arguments are read once, in order.** The first `-v` or `-h` wins. An earlier argument error (such as `+0`) still wins over it.
8. **No argument parser crate.** Three flags do not justify one.

## Invariants

- **I-39 (type + boundary, `Invocation`, `parse_args`):** A version or help request has no file list, and `parse_args` is the only place that classifies `-`-prefixed arguments. Later layers receive `FileArg` values or an `Invocation`, never raw argv.
- **I-40 (runtime, `parse_args`):** `--` makes later `-`-prefixed arguments files, and only when it comes before them. This is a rule about argument order, which a slice of strings cannot express in its type, so it is enforced by the single left-to-right loop.

## Consequences

- Breaking: a file whose name starts with `-` is no longer opened by name alone. Use `--`, as in `neonemacs -- -x`, or a path prefix, as in `neonemacs ./-x`. Before this change, `-v`, `-h`, and `-x` opened files of those names.
- Behavior change: `neonemacs -v` prints the version. It no longer opens a file named `-v`.
- Behavior change: any unknown `-` option exits `2` with `unknown option`, so typos fail instead of creating buffers.
- Not in scope: `--init`, `--no-init`, options that take a value, and a parser crate. Revisit when the flag count exceeds about five or an option needs a value. At that point a dedicated parser may be warranted.
