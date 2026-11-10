//! Built-in commands by name (§4). Lua commands join this table through `lua.rs`.
//! Every command takes the editor and its effects by value and returns both.

pub mod basic;
pub mod file;
pub mod load;
pub mod prompt;
pub mod search;
pub mod shell_output;
pub mod wal;

pub use basic::switch_to;
pub use file::{confirmed, disk_checked, written};
pub use load::{file_read, request_open, wal_probed};
pub use prompt::prompt_key;
pub use search::{isearch_key, query_key};
pub use shell_output::{init_loaded, lua_done, record_init_error, shell_done};
pub use wal::{idle, wal_flushed};

use crate::commands::basic::{
    backward_char, delete_backward_char, delete_char, delete_other_windows, forward_char,
    kill_region, kill_ring_save, move_beginning_of_line, move_end_of_line, next_line,
    other_window, previous_line, recenter, set_mark_command, split_window_below,
    split_window_right, view_mode, yank, yank_pop,
};
use crate::commands::file::{save_buffer_cmd, save_buffers_kill_emacs, toggle_eol};
use crate::commands::load::revert_buffer;
use crate::commands::prompt::{
    execute_extended_command, find_file_cmd, goto_line_cmd, kill_buffer_cmd, switch_to_buffer_cmd,
    write_file_cmd,
};
use crate::commands::search::{
    isearch_backward, isearch_backward_regexp, isearch_forward, isearch_forward_regexp,
    query_replace_cmd, query_replace_regexp_cmd,
};
use crate::commands::shell_output::{reload_init_cmd, shell_command_cmd};
use crate::editor::Editor;
use crate::effect::Effects;

pub type Builtin = fn(Editor, Effects) -> (Editor, Effects);

/// Every built-in command, by name in alphabetical order (§4).
/// Kept sorted so lookup is O(log N) via binary search.
pub const BUILTINS: &[(&str, Builtin)] = &[
    ("backward-char", backward_char as Builtin),
    ("delete-backward-char", delete_backward_char as Builtin),
    ("delete-char", delete_char as Builtin),
    ("delete-other-windows", delete_other_windows as Builtin),
    (
        "execute-extended-command",
        execute_extended_command as Builtin,
    ),
    ("find-file", find_file_cmd as Builtin),
    ("forward-char", forward_char as Builtin),
    ("goto-line", goto_line_cmd as Builtin),
    ("isearch-backward", isearch_backward as Builtin),
    (
        "isearch-backward-regexp",
        isearch_backward_regexp as Builtin,
    ),
    ("isearch-forward", isearch_forward as Builtin),
    ("isearch-forward-regexp", isearch_forward_regexp as Builtin),
    ("kill-buffer", kill_buffer_cmd as Builtin),
    ("kill-region", kill_region as Builtin),
    ("kill-ring-save", kill_ring_save as Builtin),
    ("move-beginning-of-line", move_beginning_of_line as Builtin),
    ("move-end-of-line", move_end_of_line as Builtin),
    ("next-line", next_line as Builtin),
    ("other-window", other_window as Builtin),
    ("previous-line", previous_line as Builtin),
    ("query-replace", query_replace_cmd as Builtin),
    ("query-replace-regexp", query_replace_regexp_cmd as Builtin),
    ("recenter", recenter as Builtin),
    ("reload-init", reload_init_cmd as Builtin),
    ("revert-buffer", revert_buffer as Builtin),
    ("save-buffer", save_buffer_cmd as Builtin),
    (
        "save-buffers-kill-emacs",
        save_buffers_kill_emacs as Builtin,
    ),
    ("set-mark-command", set_mark_command as Builtin),
    ("shell-command", shell_command_cmd as Builtin),
    ("split-window-below", split_window_below as Builtin),
    ("split-window-right", split_window_right as Builtin),
    ("switch-to-buffer", switch_to_buffer_cmd as Builtin),
    ("toggle-eol", toggle_eol as Builtin),
    ("undo", Editor::undo as Builtin),
    ("view-mode", view_mode as Builtin),
    ("write-file", write_file_cmd as Builtin),
    ("yank", yank as Builtin),
    ("yank-pop", yank_pop as Builtin),
];

/// Looks up a built-in command by its exact name via binary search.
pub fn find(name: &str) -> Option<Builtin> {
    BUILTINS
        .binary_search_by_key(&name, |(n, _)| *n)
        .ok()
        .map(|idx| BUILTINS[idx].1)
}

/// Whether `name` is a built-in command.
#[must_use]
pub fn is_builtin(name: &str) -> bool {
    find(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_strictly_sorted_without_duplicates() {
        for window in BUILTINS.windows(2) {
            assert!(
                window[0].0 < window[1].0,
                "BUILTINS out of order or duplicate: '{}' >= '{}'",
                window[0].0,
                window[1].0
            );
        }
    }

    #[test]
    fn find_locates_every_builtin() {
        for &(name, func) in BUILTINS {
            let found = find(name);
            assert!(found.is_some(), "Failed to find builtin: {name}");
            assert_eq!(
                found.unwrap() as usize,
                func as usize,
                "Mismatched function pointer for: {name}"
            );
        }
        assert!(find("nonexistent-command").is_none());
    }
}
