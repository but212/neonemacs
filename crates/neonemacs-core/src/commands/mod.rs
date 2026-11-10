//! Built-in commands by name (§4). Lua commands join this table through `lua.rs`.
//! Every command takes the editor and its effects by value and returns both.

pub mod buffer_window;
pub mod edit;
pub mod file;
pub mod load;
pub mod motion;
pub mod prompt;
pub mod search;
pub mod shell_output;
pub mod table;
pub mod wal;

pub use buffer_window::switch_to;
pub use file::{confirmed, disk_checked, written};
pub use load::{file_read, request_open, wal_probed};
pub use prompt::prompt_key;
pub use search::{isearch_key, query_key};
pub use shell_output::{init_loaded, lua_done, record_init_error, shell_done};
pub use table::find;
pub use wal::{idle, wal_flushed};

/// Whether `name` is a built-in command.
#[must_use]
pub fn is_builtin(name: &str) -> bool {
    find(name).is_some()
}
