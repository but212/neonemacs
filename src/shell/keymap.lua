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
bind("C-x h", "mark-whole-buffer")

-- Editing & Killing
bind("C-k", "kill-line")
bind("C-x u", "undo")
bind("C-x C-o", "delete-blank-lines")
bind("C-x C-u", "upcase-region")
bind("C-x C-l", "downcase-region")
bind("M-y", "yank-pop")
bind("M-q", "fill-paragraph")
bind("M-u", "upcase-word")
bind("M-l", "downcase-word")
bind("M-c", "capitalize-word")
bind("M-d", "kill-word")
bind("M-DEL", "backward-kill-word")
bind("M-k", "kill-sentence")
bind("M-;", "comment-dwim")
bind("M-SPC", "just-one-space")
bind("M-\\", "delete-horizontal-space")
bind("C-o", "open-line")
bind("M-^", "delete-indentation")
bind("C-t", "transpose-chars")
bind("M-t", "transpose-words")

-- Search & Navigation
bind("C-r", "isearch-backward")
bind("M-%", "query-replace")
bind("M-g", "goto-line")
bind("M-f", "forward-word")
bind("M-b", "backward-word")
bind("M-<", "beginning-of-buffer")
bind("M->", "end-of-buffer")
bind("M-}", "forward-paragraph")
bind("M-{", "backward-paragraph")
bind("M-h", "mark-paragraph")
bind("M-e", "forward-sentence")
bind("M-a", "backward-sentence")

-- Inspection & Information
bind("M-=", "count-words-region")
bind("C-x =", "what-cursor-position")

-- Shell execution
bind("M-!", "shell-command")
