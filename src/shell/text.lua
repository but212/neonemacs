-- text.lua: Standard text transformation and editing commands (§6)

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

local function is_word_byte(b)
    if not b then return false end
    -- ASCII alphanumeric or underscore, or UTF-8 multibyte (> 127)
    return (b >= 48 and b <= 57) or (b >= 65 and b <= 90) or (b >= 97 and b <= 122) or (b == 95) or (b >= 128)
end

local function forward_word_end(p)
    local len = buffer.len()
    if p >= len then return p end
    local text = buffer.get_text(p, len)
    local i = 1
    local text_len = #text

    -- Skip non-word chars
    while i <= text_len and not is_word_byte(string.byte(text, i)) do
        i = i + 1
    end
    -- Skip word chars
    while i <= text_len and is_word_byte(string.byte(text, i)) do
        i = i + 1
    end
    return p + (i - 1)
end

local function backward_word_start(p)
    if p <= 0 then return 0 end
    local text = buffer.get_text(0, p)
    local i = #text

    -- Skip non-word chars backward
    while i >= 1 and not is_word_byte(string.byte(text, i)) do
        i = i - 1
    end
    -- Skip word chars backward
    while i >= 1 and is_word_byte(string.byte(text, i)) do
        i = i - 1
    end
    return i
end

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
