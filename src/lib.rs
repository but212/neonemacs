//! neonemacs as a library, so that `tests/` can use the editor and the shell.
//! `main.rs` is a thin entry point over it.
//!
//! `core` holds the editor state and its pure transitions. `shell` performs the
//! effects that `core` asks for: files, processes, Lua, and the terminal.

#![forbid(unsafe_code)]

pub use neonemacs_core as core;
pub mod shell;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
