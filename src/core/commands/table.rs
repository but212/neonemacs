//! Built-in command lookup table and dispatcher (§4).

use crate::core::commands::buffer_window::{
    delete_other_windows, other_window, split_window_below, split_window_right,
};
use crate::core::commands::edit::{
    delete_backward_char, delete_char, fill_paragraph, kill_line, kill_region, kill_ring_save,
    view_mode, yank, yank_pop,
};
use crate::core::commands::file::{
    revert_buffer, save_buffer_cmd, save_buffers_kill_emacs,
};
use crate::core::commands::motion::{
    backward_char, exchange_point_and_mark, forward_char, move_beginning_of_line,
    move_end_of_line, next_line, previous_line, set_mark_command,
};
use crate::core::commands::prompt::{
    execute_extended_command, find_file_cmd, goto_line_cmd, kill_buffer_cmd, shell_command_cmd,
    switch_to_buffer_cmd, write_file_cmd,
};
use crate::core::commands::search::{
    isearch_backward, isearch_backward_regexp, isearch_forward, isearch_forward_regexp,
    query_replace_cmd, query_replace_regexp_cmd,
};
use crate::core::editor::Editor;
use crate::core::effect::Effects;

pub type Builtin = fn(Editor, Effects) -> (Editor, Effects);

/// Every built-in command, by name. Lua commands are added through `lua.rs`.
pub const BUILTINS: &[(&str, Builtin)] = &[
    ("undo", Editor::undo as Builtin),
    ("fill-paragraph", fill_paragraph as Builtin),
    ("shell-command", shell_command_cmd as Builtin),
    ("forward-char", forward_char as Builtin),
    ("backward-char", backward_char as Builtin),
    ("next-line", next_line as Builtin),
    ("previous-line", previous_line as Builtin),
    ("move-beginning-of-line", move_beginning_of_line as Builtin),
    ("move-end-of-line", move_end_of_line as Builtin),
    ("delete-backward-char", delete_backward_char as Builtin),
    ("delete-char", delete_char as Builtin),
    ("set-mark-command", set_mark_command as Builtin),
    ("kill-region", kill_region as Builtin),
    ("kill-ring-save", kill_ring_save as Builtin),
    ("kill-line", kill_line as Builtin),
    ("yank", yank as Builtin),
    ("yank-pop", yank_pop as Builtin),
    ("save-buffer", save_buffer_cmd as Builtin),
    ("split-window-below", split_window_below as Builtin),
    ("split-window-right", split_window_right as Builtin),
    ("other-window", other_window as Builtin),
    ("delete-other-windows", delete_other_windows as Builtin),
    ("isearch-forward", isearch_forward as Builtin),
    ("isearch-backward", isearch_backward as Builtin),
    ("isearch-forward-regexp", isearch_forward_regexp as Builtin),
    (
        "isearch-backward-regexp",
        isearch_backward_regexp as Builtin,
    ),
    (
        "save-buffers-kill-emacs",
        save_buffers_kill_emacs as Builtin,
    ),
    (
        "exchange-point-and-mark",
        exchange_point_and_mark as Builtin,
    ),
    ("find-file", find_file_cmd as Builtin),
    ("write-file", write_file_cmd as Builtin),
    ("switch-to-buffer", switch_to_buffer_cmd as Builtin),
    ("kill-buffer", kill_buffer_cmd as Builtin),
    ("goto-line", goto_line_cmd as Builtin),
    ("query-replace", query_replace_cmd as Builtin),
    ("query-replace-regexp", query_replace_regexp_cmd as Builtin),
    ("revert-buffer", revert_buffer as Builtin),
    ("view-mode", view_mode as Builtin),
    (
        "execute-extended-command",
        execute_extended_command as Builtin,
    ),
];

pub fn find(name: &str) -> Option<Builtin> {
    BUILTINS.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}
