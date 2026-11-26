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
bind("C-M-_", "redo")
bind("M-y", "yank-pop")
bind("M-q", "fill-paragraph")
bind("M-d", "kill-word")
bind("M-DEL", "backward-kill-word")
bind("M-u", "upcase-word")
bind("M-l", "downcase-word")
bind("M-c", "capitalize-word")
bind("M-;", "comment-dwim")

-- Search & Navigation
bind("C-r", "isearch-backward")
bind("M-%", "query-replace")
bind("M-g", "goto-line")
bind("M-<", "beginning-of-buffer")
bind("M->", "end-of-buffer")
bind("C-l", "recenter")

-- Shell execution
bind("M-!", "shell-command")
bind("C-z", "suspend-emacs")

-- Korean two-set (두벌식) jamo stand for the QWERTY key they share (I-46, I-48)
local function alias_row(jamo, ascii)
    local i = 1
    for _, j in utf8.codes(jamo) do
        key_alias(utf8.char(j), ascii:sub(i, i))
        i = i + 1
    end
end
alias_row("ㅂㅈㄷㄱㅅㅛㅕㅑㅐㅔ", "qwertyuiop")
alias_row("ㅁㄴㅇㄹㅎㅗㅓㅏㅣ", "asdfghjkl")
alias_row("ㅋㅌㅊㅍㅠㅜㅡ", "zxcvbnm")
alias_row("ㅃㅉㄸㄲㅆㅒㅖ", "QWERTOP")
