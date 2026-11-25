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
    kill_region, kill_ring_save, move_beginning_of_line, move_end_of_line, next_line, other_window,
    previous_line, recenter, set_mark_command, split_window_below, split_window_right, view_mode,
    yank, yank_pop,
};
use crate::commands::file::{save_buffer_cmd, save_buffers_kill_emacs, toggle_eol};
use crate::commands::load::revert_buffer;
use crate::commands::prompt::prompt;
use crate::commands::search::{
    query_replace_cmd, query_replace_regexp_cmd, start_isearch,
};
use crate::commands::shell_output::{reload_init_cmd, shell_command_cmd};
use crate::editor::{Direction, Editor, MatchKind, Purpose};
use crate::effect::Effects;

#[derive(Debug, Clone)]
pub enum Builtin {
    Fn(fn(Editor, Effects) -> (Editor, Effects)),
    Prompt(&'static str, Purpose),
    Isearch(Direction, MatchKind),
}

impl Builtin {
    #[must_use]
    pub fn run(&self, ed: Editor, fx: Effects) -> (Editor, Effects) {
        match self {
            Self::Fn(f) => f(ed, fx),
            Self::Prompt(label, purpose) => (prompt(ed, label, purpose.clone()), fx),
            Self::Isearch(dir, kind) => (start_isearch(ed, *dir, *kind), fx),
        }
    }
}

/// Every built-in command, by name in alphabetical order (§4).
/// Kept sorted so lookup is O(log N) via binary search.
pub const BUILTINS: &[(&str, Builtin)] = &[
    ("backward-char", Builtin::Fn(backward_char)),
    ("delete-backward-char", Builtin::Fn(delete_backward_char)),
    ("delete-char", Builtin::Fn(delete_char)),
    ("delete-other-windows", Builtin::Fn(delete_other_windows)),
    (
        "execute-extended-command",
        Builtin::Prompt("M-x ", Purpose::ExecuteCommand),
    ),
    ("find-file", Builtin::Prompt("Find file: ", Purpose::FindFile)),
    ("forward-char", Builtin::Fn(forward_char)),
    ("goto-line", Builtin::Prompt("Goto line: ", Purpose::GotoLine)),
    (
        "isearch-backward",
        Builtin::Isearch(Direction::Backward, MatchKind::Literal),
    ),
    (
        "isearch-backward-regexp",
        Builtin::Isearch(Direction::Backward, MatchKind::Regex),
    ),
    (
        "isearch-forward",
        Builtin::Isearch(Direction::Forward, MatchKind::Literal),
    ),
    (
        "isearch-forward-regexp",
        Builtin::Isearch(Direction::Forward, MatchKind::Regex),
    ),
    (
        "kill-buffer",
        Builtin::Prompt("Kill buffer: ", Purpose::KillBuffer),
    ),
    ("kill-region", Builtin::Fn(kill_region)),
    ("kill-ring-save", Builtin::Fn(kill_ring_save)),
    ("move-beginning-of-line", Builtin::Fn(move_beginning_of_line)),
    ("move-end-of-line", Builtin::Fn(move_end_of_line)),
    ("next-line", Builtin::Fn(next_line)),
    ("other-window", Builtin::Fn(other_window)),
    ("previous-line", Builtin::Fn(previous_line)),
    ("query-replace", Builtin::Fn(query_replace_cmd)),
    ("query-replace-regexp", Builtin::Fn(query_replace_regexp_cmd)),
    ("recenter", Builtin::Fn(recenter)),
    ("reload-init", Builtin::Fn(reload_init_cmd)),
    ("revert-buffer", Builtin::Fn(revert_buffer)),
    ("save-buffer", Builtin::Fn(save_buffer_cmd)),
    (
        "save-buffers-kill-emacs",
        Builtin::Fn(save_buffers_kill_emacs),
    ),
    ("set-mark-command", Builtin::Fn(set_mark_command)),
    ("shell-command", Builtin::Fn(shell_command_cmd)),
    ("split-window-below", Builtin::Fn(split_window_below)),
    ("split-window-right", Builtin::Fn(split_window_right)),
    (
        "switch-to-buffer",
        Builtin::Prompt("Switch to buffer: ", Purpose::SwitchBuffer),
    ),
    ("toggle-eol", Builtin::Fn(toggle_eol)),
    ("undo", Builtin::Fn(Editor::undo)),
    ("view-mode", Builtin::Fn(view_mode)),
    (
        "write-file",
        Builtin::Prompt("Write file: ", Purpose::WriteFile),
    ),
    ("yank", Builtin::Fn(yank)),
    ("yank-pop", Builtin::Fn(yank_pop)),
];

/// Looks up a built-in command by its exact name via binary search.
pub fn find(name: &str) -> Option<Builtin> {
    BUILTINS
        .binary_search_by_key(&name, |(n, _)| *n)
        .ok()
        .map(|idx| BUILTINS[idx].1.clone())
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
        for (name, _) in BUILTINS {
            assert!(find(name).is_some(), "Failed to find builtin: {name}");
        }
        assert!(find("nonexistent-command").is_none());
    }
}
