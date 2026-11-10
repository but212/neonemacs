//! Built-in commands by name (§4). Lua commands join this table through `lua.rs`.
//! Every command takes the editor and its effects by value and returns both.

pub mod buffer_window;
pub mod edit;
pub mod file;
pub mod load;
pub mod motion;
pub mod prompt;
pub mod registry;
pub mod search;
pub mod shell_output;
pub mod table;

pub use buffer_window::{kill_buffer, switch_to};
pub use file::{confirmed, disk_checked, save_buffer, written};
pub use load::{file_read, request_open, wal_probed};
pub use prompt::{completions, prompt_key};
pub use registry::CommandRegistry;
pub use search::{isearch_key, query_key, start_query_replace};
pub use shell_output::{init_loaded, lua_done, record_init_error, shell_done};
pub use table::{BUILTINS, Builtin, find};
