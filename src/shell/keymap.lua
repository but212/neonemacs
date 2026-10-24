-- NeonEmacs default extended keymap (§3, §12)

-- Window management
bind("C-x 2", "split-window-below")
bind("C-x 3", "split-window-right")
bind("C-x 1", "delete-other-windows")
bind("C-x o", "other-window")

-- Buffer & File management
bind("C-x b", "switch-to-buffer")
bind("C-x k", "kill-buffer")
bind("C-x C-w", "write-file")
bind("C-x C-q", "view-mode")

-- Editing & Killing
bind("C-k", "kill-line")
bind("C-x u", "undo")
bind("M-y", "yank-pop")
bind("M-q", "fill-paragraph")

-- Search & Navigation
bind("C-r", "isearch-backward")
bind("M-%", "query-replace")
bind("M-g", "goto-line")

-- Shell execution
bind("M-!", "shell-command")
