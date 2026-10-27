//! Built-in commands by name (§4). Lua commands join this table through `lua.rs`.
//! Every command takes the editor and its effects by value and returns both.

pub mod buffer_window;
pub mod edit;
pub mod file;
pub mod motion;
pub mod prompt;
pub mod registry;
pub mod search;
pub mod shell_output;
pub mod table;

pub use buffer_window::{kill_buffer, switch_to};
pub use file::{confirmed, disk_checked, file_read, request_open, save_buffer, written};
pub use prompt::{completions, prompt_key};
pub use registry::CommandRegistry;
pub use search::{isearch_key, query_key, start_query_replace};
pub use shell_output::{lua_done, shell_done};
pub use table::{BUILTINS, Builtin, find};
