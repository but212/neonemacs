-- text.lua: Pareto-core text transformation, movement, and editing commands (§6, ADR 0006)

--------------------------------------------------------------------------------
-- 1. Core Primitives (Line, Char, Byte Predicates)
--------------------------------------------------------------------------------

local function is_blank_line(line_start, line_end)
    local s = buffer.get_text(line_start, line_end)
    return s:match("^%s*$") ~= nil
end

local function next_line(pos)
    local total = buffer.len()
    if pos >= total then return nil, nil end
    local e = buffer.line_end(pos)
    if e >= total then return nil, nil end
    local next_s = e + 1
    local next_e = buffer.line_end(next_s)
    return next_s, next_e
end

local function prev_line(pos)
    if pos <= 0 then return nil, nil end
    local cur_s = buffer.line_start(pos)
    if cur_s <= 0 then return nil, nil end
    local prev_e = cur_s - 1
    local prev_s = buffer.line_start(prev_e)
    return prev_s, prev_e
end

local function collect_lines(from_pos, to_pos)
    local s = buffer.line_start(from_pos)
    local last_s = buffer.line_start(to_pos)
    local lines = {}
    local cur = s
    while true do
        table.insert(lines, { s = cur, e = buffer.line_end(cur) })
        if cur >= last_s then break end
        local next_s, _ = next_line(cur)
        if not next_s then break end
        cur = next_s
    end
    return lines
end

local function is_word_byte(b)
    if not b then return false end
    return (b >= 48 and b <= 57) or (b >= 65 and b <= 90) or (b >= 97 and b <= 122) or (b == 95) or (b >= 128)
end

--------------------------------------------------------------------------------
-- 2. Word Operations & Buffer Bounds (M-f, M-b, M-d, M-DEL, M-<, M->)
--------------------------------------------------------------------------------

local function scan_word_forward(from_pos)
    local len = buffer.len()
    if from_pos >= len then return nil, nil end

    local cur_pos = from_pos
    local w_start = nil

    while cur_pos < len do
        local l_end = buffer.line_end(cur_pos)
        local scan_end = l_end
        -- If current line has content to scan
        if scan_end > cur_pos then
            local text = buffer.get_text(cur_pos, scan_end)
            local text_len = #text
            local i = 1
            if not w_start then
                while i <= text_len and not is_word_byte(string.byte(text, i)) do
                    i = i + 1
                end
                if i <= text_len then
                    w_start = cur_pos + i - 1
                    while i <= text_len and is_word_byte(string.byte(text, i)) do
                        i = i + 1
                    end
                    local w_end = cur_pos + i - 1
                    return w_start, w_end
                end
            else
                while i <= text_len and is_word_byte(string.byte(text, i)) do
                    i = i + 1
                end
                local w_end = cur_pos + i - 1
                return w_start, w_end
            end
        elseif w_start then
            return w_start, cur_pos
        end

        if l_end >= len then
            break
        end
        -- Move past the newline
        cur_pos = l_end + 1
    end

    if w_start then
        return w_start, len
    end
    return nil, nil
end

local function scan_word_backward(from_pos)
    if from_pos <= 0 then return nil, nil end

    local cur_pos = from_pos
    local w_end = nil

    while cur_pos > 0 do
        local l_start = buffer.line_start(cur_pos)
        if cur_pos > l_start then
            local text = buffer.get_text(l_start, cur_pos)
            local i = #text
            if not w_end then
                while i >= 1 and not is_word_byte(string.byte(text, i)) do
                    i = i - 1
                end
                if i >= 1 then
                    w_end = l_start + i
                end
            end
            if w_end then
                while i >= 1 and is_word_byte(string.byte(text, i)) do
                    i = i - 1
                end
                if i >= 1 then
                    local w_start = l_start + i
                    return w_start, w_end
                end
            end
        end

        if l_start <= 0 then
            break
        end
        -- Move before newline
        cur_pos = l_start - 1
    end

    if w_end then
        return 0, w_end
    end
    return nil, nil
end

local function forward_word_end(p)
    local _, w_end = scan_word_forward(p)
    return w_end or buffer.len()
end

local function backward_word_start(p)
    local w_start, _ = scan_word_backward(p)
    return w_start or 0
end

-- forward-word: move point forward to end of next word (M-f)
command("forward-word", function()
    local p = buffer.point()
    buffer.set_point(forward_word_end(p))
end)

-- backward-word: move point backward to start of previous word (M-b)
command("backward-word", function()
    local p = buffer.point()
    buffer.set_point(backward_word_start(p))
end)

-- beginning-of-buffer: move point to beginning of buffer (M-<)
command("beginning-of-buffer", function()
    buffer.set_point(0)
end)

-- end-of-buffer: move point to end of buffer (M->)
command("end-of-buffer", function()
    buffer.set_point(buffer.len())
end)

-- kill-word: kill from point to end of word (M-d)
command("kill-word", function()
    local p = buffer.point()
    local w_end = forward_word_end(p)
    if w_end > p then
        buffer.kill(p, w_end)
    end
end)

-- backward-kill-word: kill from start of word to point (M-DEL)
command("backward-kill-word", function()
    local p = buffer.point()
    local w_start = backward_word_start(p)
    if p > w_start then
        buffer.kill(w_start, p)
        buffer.set_point(w_start)
    end
end)

--------------------------------------------------------------------------------
-- 3. Paragraph Formatting (fill-paragraph, M-q)
--------------------------------------------------------------------------------

local function skip_lines_while(cur_s, cur_e, dir_fn, cond_fn)
    while true do
        local next_s, next_e = dir_fn(cur_s)
        if not next_s or not cond_fn(next_s, next_e) then
            return cur_s, cur_e
        end
        cur_s, cur_e = next_s, next_e
    end
end

local function get_paragraph_bounds(p)
    local l_start = buffer.line_start(p)
    local l_end = buffer.line_end(p)
    if is_blank_line(l_start, l_end) then
        return nil
    end

    local p_start, _ = skip_lines_while(l_start, l_end, prev_line, function(s, e)
        return not is_blank_line(s, e)
    end)

    local _, p_end = skip_lines_while(l_start, l_end, next_line, function(s, e)
        return not is_blank_line(s, e)
    end)

    return p_start, p_end
end

-- fill-paragraph: formats current paragraph respecting fill-column and first-line indent (M-q)
command("fill-paragraph", function()
    local p = buffer.point()
    local p_start, p_end = get_paragraph_bounds(p)
    if not p_start then
        return
    end

    local fill = get("fill-column") or 70
    local text = buffer.get_text(p_start, p_end)
    local first_line_end = buffer.line_end(p_start)
    local first_line = buffer.get_text(p_start, first_line_end)
    local indent = first_line:match("^[ \t]*") or ""
    local indent_width = buffer.width(indent)

    local words = {}
    for w in text:gmatch("%S+") do
        table.insert(words, w)
    end
    if #words == 0 then
        return
    end

    local out = indent
    local line_width = indent_width
    local line_has_word = false

    for _, w in ipairs(words) do
        local w_width = buffer.width(w)
        if line_has_word and (line_width + 1 + w_width > fill) then
            out = out .. "\n" .. indent
            line_width = indent_width
            line_has_word = false
        end
        if line_has_word then
            out = out .. " "
            line_width = line_width + 1
        end
        out = out .. w
        line_width = line_width + w_width
        line_has_word = true
    end

    buffer.replace(p_start, p_end, out)
    buffer.set_point(p_start)
end)

--------------------------------------------------------------------------------
-- 4. Comment Operations (comment-dwim, M-;)
--------------------------------------------------------------------------------

local function split_indent(line_str)
    local indent = line_str:match("^(%s*)") or ""
    local content = line_str:sub(#indent + 1)
    return indent, content
end

local function is_commented_with(content, prefix)
    return content:sub(1, #prefix) == prefix
end

local function format_comment_line(indent, content, prefix)
    local new_line = indent .. prefix .. " " .. content
    local delta = #prefix + 1
    return new_line, delta
end

local function strip_comment_line(indent, content, prefix)
    local remove_len = #prefix
    if content:sub(#prefix + 1, #prefix + 1) == " " then
        remove_len = remove_len + 1
    end
    local new_line = indent .. content:sub(remove_len + 1)
    return new_line, remove_len
end

local function comment_dwim()
    local prefix = buffer.comment_prefix()
    if not prefix or prefix == "" then
        message("No comment syntax defined for buffer")
        return
    end

    local pt = buffer.point()
    local mark = buffer.mark()
    local has_region = (mark ~= nil and mark ~= pt)

    local range_s, eff_e
    if has_region then
        range_s = math.min(mark, pt)
        eff_e = math.max(mark, pt)
        if eff_e > range_s and buffer.line_start(eff_e) == eff_e then
            eff_e = eff_e - 1
        end
    else
        range_s = pt
        eff_e = pt
    end

    local lines = collect_lines(range_s, eff_e)

    local non_blank_count = 0
    local commented_count = 0
    for _, l in ipairs(lines) do
        local str = buffer.get_text(l.s, l.e)
        local _, content = split_indent(str)
        if content ~= "" then
            non_blank_count = non_blank_count + 1
            if is_commented_with(content, prefix) then
                commented_count = commented_count + 1
            end
        end
    end

    local should_uncomment = (non_blank_count > 0 and commented_count == non_blank_count)

    local orig_pt = pt
    local orig_mark = mark
    local delta_at_pt = 0

    for i = #lines, 1, -1 do
        local l = lines[i]
        local str = buffer.get_text(l.s, l.e)
        local indent, content = split_indent(str)

        if should_uncomment then
            if is_commented_with(content, prefix) then
                local new_line, removed = strip_comment_line(indent, content, prefix)
                buffer.replace(l.s, l.e, new_line)
                if orig_pt >= l.s then
                    delta_at_pt = delta_at_pt - removed
                end
            end
        else
            if content ~= "" or (not has_region and #lines == 1) then
                local new_line, added = format_comment_line(indent, content, prefix)
                buffer.replace(l.s, l.e, new_line)
                if orig_pt >= l.s then
                    delta_at_pt = delta_at_pt + added
                end
            end
        end
    end

    local new_pt = math.max(0, math.min(buffer.len(), orig_pt + delta_at_pt))
    buffer.set_point(new_pt)
    if has_region then
        buffer.set_mark(orig_mark)
    end
end

command("comment-dwim", comment_dwim)

--------------------------------------------------------------------------------
-- 5. Whole Buffer Selection (mark-whole-buffer, C-x h)
--------------------------------------------------------------------------------

local function mark_whole_buffer()
    buffer.set_point(0)
    buffer.set_mark(buffer.len())
    message("Mark set")
end

command("mark-whole-buffer", mark_whole_buffer)
