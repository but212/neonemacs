//! Built-in commands by name (§4). Lua commands join this table through `lua.rs`.
//! Every command takes the editor and its effects by value and returns both.

pub mod buffer_window;
pub mod edit;
pub mod file;
pub mod motion;
pub mod prompt;
pub mod search;
pub mod table;
pub mod util;

pub use buffer_window::{kill_buffer, switch_to};
pub use file::{
    confirmed, disk_checked, file_read, save_buffer, undo_read, undo_written, written,
};
pub use prompt::{completions, lua_done, prompt_key, shell_done};
pub use search::{isearch_key, query_key, start_query_replace};
pub use table::{BUILTINS, Builtin, find};
pub use util::{column, request_open};
