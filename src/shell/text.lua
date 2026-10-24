-- text.lua: Standard text transformation, movement, and editing commands (§6, ADR 0006, ADR 0007, ADR 0008)

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

local function is_sentence_punct(b)
    return b == 46 or b == 63 or b == 33 -- . ? !
end

local function is_sentence_closer(b)
    return b == 39 or b == 34 or b == 41 or b == 93 or b == 125 -- ' " ) ] }
end

local function is_sentence_space(b)
    return b == 32 or b == 9 or b == 10 or b == 13
end

local function utf8_char_len(byte_val)
    if not byte_val then return 1 end
    if byte_val < 128 then return 1
    elseif byte_val >= 192 and byte_val < 224 then return 2
    elseif byte_val >= 224 and byte_val < 240 then return 3
    elseif byte_val >= 240 and byte_val < 248 then return 4
    else return 1
    end
end

--------------------------------------------------------------------------------
-- 2. Word Operations (Motion, Transformations & Kills)
--------------------------------------------------------------------------------

local function scan_word_forward(from_pos)
    local len = buffer.len()
    if from_pos >= len then return nil, nil end
    local text = buffer.get_text(from_pos, len)
    local i = 1
    local text_len = #text

    while i <= text_len and not is_word_byte(string.byte(text, i)) do
        i = i + 1
    end
    if i > text_len then return nil, nil end
    local w_start = from_pos + i - 1

    while i <= text_len and is_word_byte(string.byte(text, i)) do
        i = i + 1
    end
    local w_end = from_pos + i - 1
    return w_start, w_end
end

local function scan_word_backward(from_pos)
    if from_pos <= 0 then return nil, nil end
    local text = buffer.get_text(0, from_pos)
    local i = #text

    while i >= 1 and not is_word_byte(string.byte(text, i)) do
        i = i - 1
    end
    if i == 0 then return nil, nil end
    local w_end = i

    while i >= 1 and is_word_byte(string.byte(text, i)) do
        i = i - 1
    end
    local w_start = i
    return w_start, w_end
end

local function forward_word_end(p)
    local _, w_end = scan_word_forward(p)
    return w_end or buffer.len()
end

local function backward_word_start(p)
    local w_start, _ = scan_word_backward(p)
    return w_start or 0
end

-- forward-word: move point forward to end of next word
command("forward-word", function()
    local p = buffer.point()
    buffer.set_point(forward_word_end(p))
end)

-- backward-word: move point backward to start of previous word
command("backward-word", function()
    local p = buffer.point()
    buffer.set_point(backward_word_start(p))
end)

-- beginning-of-buffer: move point to beginning of buffer
command("beginning-of-buffer", function()
    buffer.set_point(0)
end)

-- end-of-buffer: move point to end of buffer
command("end-of-buffer", function()
    buffer.set_point(buffer.len())
end)

local function transform_word(fn)
    local p = buffer.point()
    local w_end = forward_word_end(p)
    if w_end > p then
        local target = buffer.get_text(p, w_end)
        buffer.replace(p, w_end, fn(target))
        buffer.set_point(w_end)
    end
end

-- upcase-word: convert from point to end of word to uppercase
command("upcase-word", function()
    transform_word(string.upper)
end)

-- downcase-word: convert from point to end of word to lowercase
command("downcase-word", function()
    transform_word(string.lower)
end)

-- capitalize-word: capitalize word starting at point
command("capitalize-word", function()
    transform_word(function(s)
        return string.upper(string.sub(s, 1, 1)) .. string.lower(string.sub(s, 2))
    end)
end)

-- kill-word: kill from point to end of word
command("kill-word", function()
    local p = buffer.point()
    local w_end = forward_word_end(p)
    if w_end > p then
        buffer.kill(p, w_end)
    end
end)

-- backward-kill-word: kill from start of word to point
command("backward-kill-word", function()
    local p = buffer.point()
    local w_start = backward_word_start(p)
    if p > w_start then
        buffer.kill(w_start, p)
        buffer.set_point(w_start)
    end
end)

--------------------------------------------------------------------------------
-- 3. Sentence Operations (Motion & Kills)
--------------------------------------------------------------------------------

local function scan_sentence_forward(from_pos)
    local total_len = buffer.len()
    if from_pos >= total_len then return total_len end
    local text = buffer.get_text(from_pos, total_len)
    local text_len = #text

    local i = 1
    while i <= text_len do
        local b = string.byte(text, i)
        if is_sentence_punct(b) then
            local j = i + 1
            while j <= text_len and is_sentence_closer(string.byte(text, j)) do
                j = j + 1
            end
            if j > text_len then
                return from_pos + text_len
            end
            local next_b = string.byte(text, j)
            if is_sentence_space(next_b) then
                local target = from_pos + j - 1
                if target > from_pos then
                    return target
                end
            end
        end
        i = i + 1
    end
    return total_len
end

local function scan_sentence_backward(from_pos)
    if from_pos <= 0 then return 0 end
    local text = buffer.get_text(0, from_pos)
    local text_len = #text

    local i = text_len
    while i >= 1 and is_sentence_space(string.byte(text, i)) do
        i = i - 1
    end
    if i == 0 then return 0 end

    while i >= 1 do
        local b = string.byte(text, i)
        if is_sentence_punct(b) then
            local j = i + 1
            while j <= text_len and is_sentence_closer(string.byte(text, j)) do
                j = j + 1
            end
            if j <= text_len and is_sentence_space(string.byte(text, j)) then
                local k = j
                while k <= text_len and is_sentence_space(string.byte(text, k)) do
                    k = k + 1
                end
                if k - 1 < from_pos then
                    return k - 1
                end
            end
        end
        i = i - 1
    end
    return 0
end

-- forward-sentence: move point forward to end of sentence
command("forward-sentence", function()
    local p = buffer.point()
    buffer.set_point(scan_sentence_forward(p))
end)

-- backward-sentence: move point backward to beginning of sentence
command("backward-sentence", function()
    local p = buffer.point()
    buffer.set_point(scan_sentence_backward(p))
end)

-- kill-sentence: kill from point to end of sentence (including trailing space on same line, preserving newline)
command("kill-sentence", function()
    local p = buffer.point()
    local s_end = scan_sentence_forward(p)
    if s_end <= p then return end

    local l_end = buffer.line_end(p)
    local kill_end = s_end
    if kill_end < l_end then
        local rest = buffer.get_text(kill_end, l_end)
        local spaces = rest:match("^[ \t]+")
        if spaces then
            kill_end = kill_end + #spaces
        end
    end
    kill_end = math.min(kill_end, l_end)
    if kill_end > p then
        buffer.kill(p, kill_end)
    end
end)

--------------------------------------------------------------------------------
-- 4. Paragraph Operations (Formatting, Motion & Marking)
--------------------------------------------------------------------------------

local function skip_lines_while(s, e, dir_fn, cond_fn)
    local cur_s, cur_e = s, e
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

local function scan_paragraph_forward(p)
    local total_len = buffer.len()
    local cur_s = buffer.line_start(p)
    local cur_e = buffer.line_end(p)

    if not is_blank_line(cur_s, cur_e) then
        local _, last_text_e = skip_lines_while(cur_s, cur_e, next_line, function(s, e)
            return not is_blank_line(s, e)
        end)
        if last_text_e >= total_len then
            return total_len
        end
        return last_text_e + 1
    else
        local first_text_s, first_text_e = nil, nil
        local cur = cur_e
        while cur < total_len do
            local ns, ne = next_line(cur)
            if not ns then break end
            if not is_blank_line(ns, ne) then
                first_text_s, first_text_e = ns, ne
                break
            end
            cur = ne
        end
        if not first_text_s then return total_len end

        local _, last_text_e = skip_lines_while(first_text_s, first_text_e, next_line, function(s, e)
            return not is_blank_line(s, e)
        end)
        if last_text_e >= total_len then
            return total_len
        end
        return last_text_e + 1
    end
end

local function scan_paragraph_backward(p)
    if p <= 0 then return 0 end
    local cur_s = buffer.line_start(p)
    local cur_e = buffer.line_end(p)

    if not is_blank_line(cur_s, cur_e) then
        local para_s, _ = skip_lines_while(cur_s, cur_e, prev_line, function(s, e)
            return not is_blank_line(s, e)
        end)
        if p > para_s then
            return para_s
        end
        local prev_text_s, prev_text_e = nil, nil
        local cur = para_s
        while cur > 0 do
            local ps, pe = prev_line(cur)
            if not ps then break end
            if not is_blank_line(ps, pe) then
                prev_text_s, prev_text_e = ps, pe
                break
            end
            cur = ps
        end
        if not prev_text_s then return 0 end
        local prior_para_s, _ = skip_lines_while(prev_text_s, prev_text_e, prev_line, function(s, e)
            return not is_blank_line(s, e)
        end)
        return prior_para_s
    else
        local prev_text_s, prev_text_e = nil, nil
        local cur = cur_s
        while cur > 0 do
            local ps, pe = prev_line(cur)
            if not ps then break end
            if not is_blank_line(ps, pe) then
                prev_text_s, prev_text_e = ps, pe
                break
            end
            cur = ps
        end
        if not prev_text_s then return 0 end
        local prior_para_s, _ = skip_lines_while(prev_text_s, prev_text_e, prev_line, function(s, e)
            return not is_blank_line(s, e)
        end)
        return prior_para_s
    end
end

-- fill-paragraph: formats current paragraph respecting fill-column and first-line indent
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

-- forward-paragraph: move point forward to end of paragraph (blank line or EOF)
command("forward-paragraph", function()
    local p = buffer.point()
    buffer.set_point(scan_paragraph_forward(p))
end)

-- backward-paragraph: move point backward to start of paragraph
command("backward-paragraph", function()
    local p = buffer.point()
    buffer.set_point(scan_paragraph_backward(p))
end)

-- mark-paragraph: set mark at end of paragraph and point at start
command("mark-paragraph", function()
    local p = buffer.point()
    local p_start, p_end = get_paragraph_bounds(p)
    if not p_start then
        return
    end
    local total_len = buffer.len()
    local m_end = math.min(total_len, p_end + 1)
    buffer.set_point(p_start)
    buffer.set_mark(m_end)
end)

--------------------------------------------------------------------------------
-- 5. Whitespace Manipulation & Line Operations
--------------------------------------------------------------------------------

-- open-line: insert newline at point without advancing point
command("open-line", function()
    local p = buffer.point()
    buffer.insert(p, "\n")
    buffer.set_point(p)
end)

local function join_line_handler()
    local p = buffer.point()
    local l_start = buffer.line_start(p)
    if l_start == 0 then
        return
    end
    local prev_end = l_start - 1
    local prev_start = buffer.line_start(prev_end)
    local prev_line_text = buffer.get_text(prev_start, prev_end)
    local l_end = buffer.line_end(p)
    local curr_line = buffer.get_text(l_start, l_end)

    -- Scan backward across trailing spaces on previous line
    local prev_trailing = 0
    while prev_trailing < #prev_line_text do
        local b = string.byte(prev_line_text, #prev_line_text - prev_trailing)
        if b == 32 or b == 9 then
            prev_trailing = prev_trailing + 1
        else
            break
        end
    end
    local del_start = prev_end - prev_trailing

    -- Scan forward across leading spaces on current line
    local curr_leading = 0
    while curr_leading < #curr_line do
        local b = string.byte(curr_line, curr_leading + 1)
        if b == 32 or b == 9 then
            curr_leading = curr_leading + 1
        else
            break
        end
    end
    local del_end = l_start + curr_leading

    buffer.replace(del_start, del_end, " ")
    buffer.set_point(del_start)
end

-- delete-indentation / join-line: join current line to previous and collapse whitespace
command("delete-indentation", join_line_handler)
command("join-line", join_line_handler)

local function horizontal_space_span(p)
    local l_start = buffer.line_start(p)
    local l_end = buffer.line_end(p)
    local line = buffer.get_text(l_start, l_end)
    local rel_p = p - l_start + 1

    local s_start = rel_p
    while s_start > 1 do
        local b = string.byte(line, s_start - 1)
        if b == 32 or b == 9 then
            s_start = s_start - 1
        else
            break
        end
    end

    local s_end = rel_p
    while s_end <= #line do
        local b = string.byte(line, s_end)
        if b == 32 or b == 9 then
            s_end = s_end + 1
        else
            break
        end
    end

    return l_start + (s_start - 1), l_start + (s_end - 1)
end

-- just-one-space: collapse horizontal whitespace around point into one space
command("just-one-space", function()
    local p = buffer.point()
    local abs_start, abs_end = horizontal_space_span(p)
    buffer.replace(abs_start, abs_end, " ")
    buffer.set_point(abs_start + 1)
end)

-- delete-horizontal-space: delete all horizontal whitespace around point
command("delete-horizontal-space", function()
    local p = buffer.point()
    local abs_start, abs_end = horizontal_space_span(p)
    if abs_end > abs_start then
        buffer.delete(abs_start, abs_end)
        buffer.set_point(abs_start)
    end
end)

--------------------------------------------------------------------------------
-- 6. Transposition (Chars & Words)
--------------------------------------------------------------------------------

local function prev_char_span(line, rel_p)
    if rel_p <= 1 then return nil, nil end
    local i = rel_p - 1
    while i > 1 and string.byte(line, i) >= 128 and string.byte(line, i) < 192 do
        i = i - 1
    end
    return i, rel_p - 1
end

local function next_char_span(line, rel_p)
    if rel_p > #line then return nil, nil end
    local b = string.byte(line, rel_p)
    local len = utf8_char_len(b)
    return rel_p, math.min(#line, rel_p + len - 1)
end

-- transpose-chars: swap adjacent characters around point
command("transpose-chars", function()
    local p = buffer.point()
    local len = buffer.len()
    if len < 2 then return end

    local l_start = buffer.line_start(p)
    local l_end = buffer.line_end(p)
    if l_start >= l_end then return end

    local line = buffer.get_text(l_start, l_end)
    local rel_p = p - l_start + 1

    local p1_s, p1_e, p2_s, p2_e
    if rel_p > #line then
        -- At end of line: swap the two preceding characters
        local prev2_s, prev2_e = prev_char_span(line, rel_p)
        if not prev2_s then return end
        local prev1_s, prev1_e = prev_char_span(line, prev2_s)
        if not prev1_s then return end
        p1_s, p1_e = prev1_s, prev1_e
        p2_s, p2_e = prev2_s, prev2_e
    else
        -- Middle of line: swap character before point with character at/after point
        local prev_s, prev_e = prev_char_span(line, rel_p)
        if not prev_s then return end
        local next_s, next_e = next_char_span(line, rel_p)
        if not next_s then return end
        p1_s, p1_e = prev_s, prev_e
        p2_s, p2_e = next_s, next_e
    end

    local c1 = string.sub(line, p1_s, p1_e)
    local c2 = string.sub(line, p2_s, p2_e)
    local abs_p1_s = l_start + (p1_s - 1)
    local abs_p2_e = l_start + p2_e

    buffer.replace(abs_p1_s, abs_p2_e, c2 .. c1)
    buffer.set_point(abs_p2_e)
end)

-- transpose-words: swap adjacent words around point
command("transpose-words", function()
    local p = buffer.point()
    local len = buffer.len()
    if len < 2 then return end

    local w1_s, w1_e, w2_s, w2_e
    local fw1_s, fw1_e = scan_word_forward(p)
    local bw1_s, bw1_e = scan_word_backward(p)

    if not bw1_s then
        if not fw1_s then return end
        local fw2_s, fw2_e = scan_word_forward(fw1_e)
        if not fw2_s then return end
        w1_s, w1_e = fw1_s, fw1_e
        w2_s, w2_e = fw2_s, fw2_e
    else
        if p <= bw1_e then
            local prev_w_s, prev_w_e = scan_word_backward(bw1_s)
            if prev_w_s then
                w1_s, w1_e = prev_w_s, prev_w_e
                w2_s, w2_e = bw1_s, bw1_e
            else
                if not fw1_s or fw1_s == bw1_s then
                    fw1_s, fw1_e = scan_word_forward(bw1_e)
                end
                if not fw1_s then return end
                w1_s, w1_e = bw1_s, bw1_e
                w2_s, w2_e = fw1_s, fw1_e
            end
        else
            if not fw1_s then
                local prev_w_s, prev_w_e = scan_word_backward(bw1_s)
                if not prev_w_s then return end
                w1_s, w1_e = prev_w_s, prev_w_e
                w2_s, w2_e = bw1_s, bw1_e
            else
                w1_s, w1_e = bw1_s, bw1_e
                w2_s, w2_e = fw1_s, fw1_e
            end
        end
    end

    if not w1_s or not w2_s or w1_s >= w2_s then return end

    local word1 = buffer.get_text(w1_s, w1_e)
    local sep = buffer.get_text(w1_e, w2_s)
    local word2 = buffer.get_text(w2_s, w2_e)

    buffer.replace(w1_s, w2_e, word2 .. sep .. word1)
    buffer.set_point(w1_s + #word2 + #sep + #word1)
end)

--------------------------------------------------------------------------------
-- 7. Comment Operations (comment-dwim & comment-line)
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

    -- Check if all non-blank lines are commented
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

    -- Process lines in reverse order (bottom to top) to preserve offsets of earlier lines
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
            -- If it's a non-blank line, or if it's the single line in a non-region call
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
command("comment-line", comment_dwim)

--------------------------------------------------------------------------------
-- 8. Buffer & Region Operations
--------------------------------------------------------------------------------

local function mark_whole_buffer()
    buffer.set_point(0)
    buffer.set_mark(buffer.len())
    message("Mark set")
end

-- Scans contiguous blank lines forward starting at line pos.
-- Returns the end offset (with trailing newline if present), or nil if pos is not blank.
local function scan_blank_lines_forward(from_line_pos, total)
    local cur_s = from_line_pos
    local cur_e = buffer.line_end(cur_s)
    if not is_blank_line(cur_s, cur_e) then return nil end

    local last_end_with_nl = cur_e
    while cur_s do
        local scan_e = buffer.line_end(cur_s)
        if not is_blank_line(cur_s, scan_e) then break end
        if scan_e < total then
            last_end_with_nl = scan_e + 1
        else
            last_end_with_nl = scan_e
        end
        cur_s, _ = next_line(cur_s)
    end
    return last_end_with_nl
end

-- Scans contiguous blank lines backward starting at line pos.
-- Returns the start offset of the earliest contiguous blank line, or nil if pos is not blank.
local function scan_blank_lines_backward(from_line_pos)
    local cur_s = from_line_pos
    local cur_e = buffer.line_end(cur_s)
    if not is_blank_line(cur_s, cur_e) then return nil end

    local earliest_s = cur_s
    local p_s, p_e = prev_line(cur_s)
    while p_s and is_blank_line(p_s, p_e) do
        earliest_s = p_s
        p_s, p_e = prev_line(p_s)
    end
    return earliest_s
end

-- Deletes all contiguous blank lines immediately following a non-blank line.
local function delete_following_blank_lines(cur_e, total, orig_pt)
    if cur_e >= total then return end
    local first_blank_s = cur_e + 1
    local end_with_nl = scan_blank_lines_forward(first_blank_s, total)
    if end_with_nl then
        buffer.delete(first_blank_s, end_with_nl)
        buffer.set_point(orig_pt)
    end
end

-- Deletes an isolated single blank line completely.
local function delete_single_blank_line(cur_s, cur_e, total)
    if cur_e < total then
        buffer.delete(cur_s, cur_e + 1)
        buffer.set_point(cur_s)
    elseif cur_s > 0 then
        buffer.delete(cur_s - 1, total)
        buffer.set_point(cur_s - 1)
    else
        buffer.delete(0, total)
        buffer.set_point(0)
    end
end

-- Compresses a cluster of blank lines down to a single blank line.
local function compress_blank_cluster(cur_s, cur_e, prev_blank_s, next_blank_end_with_nl)
    if next_blank_end_with_nl then
        local following_s = cur_e + 1
        if following_s < next_blank_end_with_nl then
            buffer.delete(following_s, next_blank_end_with_nl)
        end
    end
    if prev_blank_s then
        buffer.delete(prev_blank_s, cur_s)
        buffer.set_point(prev_blank_s)
    else
        buffer.set_point(cur_s)
    end
end

local function delete_blank_lines()
    local pt = buffer.point()
    local total = buffer.len()
    if total == 0 then return end

    local cur_s = buffer.line_start(pt)
    local cur_e = buffer.line_end(pt)

    if not is_blank_line(cur_s, cur_e) then
        delete_following_blank_lines(cur_e, total, pt)
    else
        local p_s, _ = prev_line(cur_s)
        local prev_blank_s = p_s and scan_blank_lines_backward(p_s) or nil

        local n_s, _ = next_line(cur_s)
        local next_blank_end_with_nl = n_s and scan_blank_lines_forward(n_s, total) or nil

        if not prev_blank_s and not next_blank_end_with_nl then
            delete_single_blank_line(cur_s, cur_e, total)
        else
            compress_blank_cluster(cur_s, cur_e, prev_blank_s, next_blank_end_with_nl)
        end
    end
end

local function with_active_region(action)
    local mark = buffer.mark()
    if not mark then
        message("The mark is not set now")
        return false
    end

    local pt = buffer.point()
    local s = math.min(pt, mark)
    local e = math.max(pt, mark)
    if s >= e then return false end

    action(s, e, pt, mark)
    return true
end

local function transform_region(fn)
    with_active_region(function(s, e, pt, mark)
        local text = buffer.get_text(s, e)
        buffer.replace(s, e, fn(text))
        buffer.set_point(pt)
        buffer.set_mark(mark)
    end)
end

local function upcase_region()
    transform_region(string.upper)
end

local function downcase_region()
    transform_region(string.lower)
end

local function kill_whole_line()
    local pt = buffer.point()
    local total = buffer.len()
    if total == 0 then return end

    local s = buffer.line_start(pt)
    local e = buffer.line_end(pt)
    local end_with_nl = (e < total) and (e + 1) or e
    if s >= end_with_nl then return end

    buffer.kill(s, end_with_nl)
    buffer.set_point(s)
end

command("mark-whole-buffer", mark_whole_buffer)
command("delete-blank-lines", delete_blank_lines)
command("upcase-region", upcase_region)
command("downcase-region", downcase_region)
command("kill-whole-line", kill_whole_line)
