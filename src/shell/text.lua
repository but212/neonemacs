-- text.lua: Standard text transformation, movement, and editing commands (§6, ADR 0006, ADR 0007)

--------------------------------------------------------------------------------
-- 1. Paragraph Formatting
--------------------------------------------------------------------------------

local function is_blank_line(line_start, line_end)
    local s = buffer.get_text(line_start, line_end)
    return s:match("^%s*$") ~= nil
end

local function get_paragraph_bounds(p)
    local l_start = buffer.line_start(p)
    local l_end = buffer.line_end(p)
    if is_blank_line(l_start, l_end) then
        return nil
    end

    local p_start = l_start
    while p_start > 0 do
        local prev_end = p_start - 1
        local prev_start = buffer.line_start(prev_end)
        if is_blank_line(prev_start, prev_end) then
            break
        end
        p_start = prev_start
    end

    local total_len = buffer.len()
    local p_end = l_end
    while p_end < total_len do
        local next_start = p_end + 1
        local next_end = buffer.line_end(next_start)
        if is_blank_line(next_start, next_end) then
            break
        end
        p_end = next_end
    end

    return p_start, p_end
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

--------------------------------------------------------------------------------
-- 2. Word Boundary Primitives & Motion
--------------------------------------------------------------------------------

local function is_word_byte(b)
    if not b then return false end
    -- ASCII alphanumeric or underscore, or UTF-8 multibyte (> 127)
    return (b >= 48 and b <= 57) or (b >= 65 and b <= 90) or (b >= 97 and b <= 122) or (b == 95) or (b >= 128)
end

-- Scans forward from `from_pos` for the next word.
-- Returns `w_start, w_end` (0-based byte offsets), or `nil, nil` if no word follows.
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

-- Scans backward from `from_pos` for the previous word.
-- Returns `w_start, w_end` (0-based byte offsets), or `nil, nil` if no word precedes.
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

--------------------------------------------------------------------------------
-- 3. Word Transformations & Kills
--------------------------------------------------------------------------------

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
-- 4. Whitespace Manipulation & Line Operations
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
    local prev_line = buffer.get_text(prev_start, prev_end)
    local l_end = buffer.line_end(p)
    local curr_line = buffer.get_text(l_start, l_end)

    -- Scan backward across trailing spaces on previous line
    local prev_trailing = 0
    while prev_trailing < #prev_line do
        local b = string.byte(prev_line, #prev_line - prev_trailing)
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
-- 5. Transposition (Chars & Words)
--------------------------------------------------------------------------------

local function utf8_char_len(byte_val)
    if not byte_val then return 1 end
    if byte_val < 128 then return 1
    elseif byte_val >= 192 and byte_val < 224 then return 2
    elseif byte_val >= 224 and byte_val < 240 then return 3
    elseif byte_val >= 240 and byte_val < 248 then return 4
    else return 1
    end
end

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

-- transpose-chars: swap adjacent characters around point with UTF-8 and EOL safety
command("transpose-chars", function()
    local total_len = buffer.len()
    if total_len < 2 then return end

    local p = buffer.point()
    local l_start = buffer.line_start(p)
    local l_end = buffer.line_end(p)
    local line = buffer.get_text(l_start, l_end)
    local rel_p = p - l_start + 1

    local c1_start, c1_end
    local c2_start, c2_end
    local target_point

    if rel_p > #line then
        -- At EOL / EOB: swap the two characters preceding point
        c2_start, c2_end = prev_char_span(line, rel_p)
        if not c2_start then return end
        c1_start, c1_end = prev_char_span(line, c2_start)
        if not c1_start then return end
        target_point = p
    else
        -- Inside line: swap character before point with character at point
        c1_start, c1_end = prev_char_span(line, rel_p)
        if not c1_start then return end
        c2_start, c2_end = next_char_span(line, rel_p)
        if not c2_start then return end
        target_point = l_start + c2_end
    end

    local abs_start = l_start + c1_start - 1
    local abs_end = l_start + c2_end
    local c1 = string.sub(line, c1_start, c1_end)
    local c2 = string.sub(line, c2_start, c2_end)

    buffer.replace(abs_start, abs_end, c2 .. c1)
    buffer.set_point(target_point)
end)

-- transpose-words: swap adjacent words around point
command("transpose-words", function()
    local p = buffer.point()
    local l_start = buffer.line_start(p)
    local l_end = buffer.line_end(p)
    local line = buffer.get_text(l_start, l_end)
    local rel_p = p - l_start + 1

    local is_cur_word = false
    if rel_p <= #line then
        is_cur_word = is_word_byte(string.byte(line, rel_p))
    end

    local w1_start, w1_end
    local w2_start, w2_end

    if is_cur_word then
        -- Point is inside a word: that word is word 1
        local text_before = buffer.get_text(0, p)
        local i = #text_before
        while i >= 1 and is_word_byte(string.byte(text_before, i)) do
            i = i - 1
        end
        w1_start = i
        w1_end = forward_word_end(p)
        w2_start, w2_end = scan_word_forward(w1_end)
    else
        -- Point is outside a word
        w1_start, w1_end = scan_word_backward(p)
        w2_start, w2_end = scan_word_forward(p)
    end

    if not w1_start or not w2_start or w1_start >= w2_start then
        return
    end

    local w1_text = buffer.get_text(w1_start, w1_end)
    local sep = buffer.get_text(w1_end, w2_start)
    local w2_text = buffer.get_text(w2_start, w2_end)

    buffer.replace(w1_start, w2_end, w2_text .. sep .. w1_text)
    buffer.set_point(w2_end)
end)
