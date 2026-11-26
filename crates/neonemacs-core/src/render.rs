//! Screen model: window rectangles, per-cell grid, and line diffs (§5, §11, A5).
//! `scroll` adjusts the view so point stays visible; `frame` is pure.

use crate::config::Toggles;
use crate::editor::{Buffer, Dir, Editor, Layout, Mode, Prefix, Window};
use crate::syntax::{LexState, SyntaxDef, TokenKind, highlight_line, highlight_line_with_state};
use crate::width::{Metrics, column, pos_and_column, unit_width};
use crate::{Doc, line_to_offset, offset_to_line, total_lines, unit_char};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Inline-first UTF-8 cell glyph storage (avoids heap allocation for ASCII/single chars,
/// falling back to `String` only when combining marks exceed 4 UTF-8 bytes).
#[derive(Debug, Clone)]
pub struct CellGlyph(CellGlyphInner);

#[derive(Debug, Clone)]
enum CellGlyphInner {
    Inline { len: u8, buf: [u8; 4] },
    Heap(String),
}

impl CellGlyph {
    pub const SPACE: Self = Self(CellGlyphInner::Inline {
        len: 1,
        buf: [b' ', 0, 0, 0],
    });
    pub const EMPTY: Self = Self(CellGlyphInner::Inline {
        len: 0,
        buf: [0; 4],
    });

    #[must_use]
    pub const fn from_ascii(b: u8) -> Self {
        assert!(b.is_ascii(), "CellGlyph::from_ascii requires ASCII byte");
        Self(CellGlyphInner::Inline {
            len: 1,
            buf: [b, 0, 0, 0],
        })
    }

    #[must_use]
    pub fn from_char(c: char) -> Self {
        let mut buf = [0u8; 4];
        let len = c.encode_utf8(&mut buf).len();
        Self(CellGlyphInner::Inline {
            len: len as u8,
            buf,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match &self.0 {
            CellGlyphInner::Inline { len, buf } => core::str::from_utf8(&buf[..usize::from(*len)])
                .expect("inline glyph is valid UTF-8"),
            CellGlyphInner::Heap(s) => s.as_str(),
        }
    }

    #[must_use]
    pub fn push(self, c: char) -> Self {
        let char_len = c.len_utf8();
        match self.0 {
            CellGlyphInner::Inline { len, mut buf } => {
                let cur = usize::from(len);
                if cur + char_len <= 4 {
                    c.encode_utf8(&mut buf[cur..]);
                    Self(CellGlyphInner::Inline {
                        len: (cur + char_len) as u8,
                        buf,
                    })
                } else {
                    let mut s = String::with_capacity(cur + char_len);
                    s.push_str(
                        core::str::from_utf8(&buf[..cur]).expect("inline glyph is valid UTF-8"),
                    );
                    s.push(c);
                    Self(CellGlyphInner::Heap(s))
                }
            }
            CellGlyphInner::Heap(mut s) => {
                s.push(c);
                Self(CellGlyphInner::Heap(s))
            }
        }
    }
}

impl core::ops::Deref for CellGlyph {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq for CellGlyph {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for CellGlyph {}

impl PartialEq<str> for CellGlyph {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for CellGlyph {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for CellGlyph {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl From<&str> for CellGlyph {
    fn from(s: &str) -> Self {
        if s.len() <= 4 {
            let mut buf = [0u8; 4];
            buf[..s.len()].copy_from_slice(s.as_bytes());
            Self(CellGlyphInner::Inline {
                len: s.len() as u8,
                buf,
            })
        } else {
            Self(CellGlyphInner::Heap(String::from(s)))
        }
    }
}

/// A single terminal cell containing display glyph and syntax style (I-22).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub glyph: CellGlyph,
    pub style: TokenKind,
    /// Drawn in reverse video: the active region or the current search match (I-47).
    pub reverse: bool,
}

impl Cell {
    #[must_use]
    pub const fn plain(glyph: CellGlyph) -> Self {
        Self {
            glyph,
            style: TokenKind::Normal,
            reverse: false,
        }
    }

    #[must_use]
    pub const fn space() -> Self {
        Self::plain(CellGlyph::SPACE)
    }
}

/// Byte ranges drawn in reverse video in the selected window: the active region, and the current
/// search or query-replace match.
fn reversed_ranges(ed: &Editor) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let buf = ed.buffer();
    if buf.mark_active()
        && let Some(mark) = buf.mark()
    {
        let point = ed.window().point;
        out.push((mark.min(point), mark.max(point)));
    }
    match ed.mode() {
        Mode::Isearch(s) => out.extend(s.found),
        Mode::QueryReplace(q) => out.push(q.current),
        _ => {}
    }
    out
}

/// A screen as a flat row-major cell grid (`cells.len() == cols * rows`, I-60), and the cursor as `(x, y)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    cells: Vec<Cell>,
    cols: usize,
    pub cursor: (usize, usize),
}

impl Frame {
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.cells.len().checked_div(self.cols).unwrap_or(0)
    }

    #[must_use]
    pub fn row(&self, y: usize) -> &[Cell] {
        if self.cols == 0 {
            return &[];
        }
        let start = y * self.cols;
        &self.cells[start..start + self.cols]
    }

    pub fn rows(&self) -> impl Iterator<Item = &[Cell]> {
        let step = self.cols.max(1);
        self.cells
            .chunks_exact(step)
            .take(if self.cols == 0 { 0 } else { usize::MAX })
    }

    #[must_use]
    pub fn into_cells(self) -> Vec<Cell> {
        self.cells
    }

    #[must_use]
    pub fn row_strings(&self) -> Vec<String> {
        self.rows()
            .map(|row| row.iter().map(|c| c.glyph.as_str()).collect::<String>())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

fn rects(layout: &Layout, rect: Rect) -> Vec<Rect> {
    match layout {
        Layout::Leaf(_) => vec![rect],
        Layout::Split { dir, first, second } => {
            let (first_rect, second_rect) = match dir {
                Dir::Below => {
                    let top = rect.h.div_ceil(2);
                    (
                        Rect { h: top, ..rect },
                        Rect {
                            y: rect.y + top,
                            h: rect.h - top,
                            ..rect
                        },
                    )
                }
                Dir::Right => {
                    let left = rect.w.div_ceil(2);
                    (
                        Rect { w: left, ..rect },
                        Rect {
                            x: rect.x + left,
                            w: rect.w - left,
                            ..rect
                        },
                    )
                }
            };
            let mut out = rects(first, first_rect);
            out.extend(rects(second, second_rect));
            out
        }
    }
}

/// Screen area of every window, in layout order. The last row is the echo area.
fn areas(ed: &Editor, cols: usize, rows: usize) -> Vec<Rect> {
    let body = Rect {
        x: 0,
        y: 0,
        w: cols,
        h: rows.saturating_sub(1),
    };
    rects(ed.windows().layout(), body)
}

/// Moves each window's `top` and `left_col` so that its point is visible, and warms
/// multi-line syntax state caches for the visible viewport.
#[must_use]
pub fn scroll(ed: Editor, cols: usize, rows: usize) -> Editor {
    let m = ed.config().metrics();
    let highlight_on = ed.config().toggles.contains(Toggles::SYNTAX_HIGHLIGHT);
    let win_areas: Vec<(Window, Rect)> = ed
        .windows()
        .windows()
        .into_iter()
        .copied()
        .zip(areas(&ed, cols, rows))
        .collect();
    let scrolled: Vec<(Window, Rect)> = win_areas
        .into_iter()
        .map(|(w, area)| {
            (
                scroll_window(ed.buffer_by_id(w.buffer).text(), w, area, m),
                area,
            )
        })
        .collect();
    let mut ed = ed.with_windows(|mut win| {
        for (i, (w, _)) in scrolled.iter().enumerate() {
            win = win.set_leaf(i, *w);
        }
        win
    });
    if highlight_on {
        for (w, area) in scrolled {
            let buf = ed.buffer_by_id(w.buffer);
            let huge = buf.text().total_bytes() >= HUGE_FILE_SYNTAX_BYTES;
            let syntax = ed.syntax_registry().find_by_filename(buf.name()).cloned();
            if !huge
                && let Some(def) = syntax
                && def.heading_prefix.is_none()
                && (def.block_comment.is_some() || def.raw_strings)
            {
                let first_line = crate::line_at(buf.text(), w.top);
                let through = first_line.saturating_add(area.h.saturating_sub(1));
                ed = ed.with_buffer(w.buffer, |b| b.ensure_syntax_cache(&def, through));
            }
        }
    }
    ed
}

fn scroll_window(text: &Doc, mut w: Window, area: Rect, m: Metrics) -> Window {
    let height = area.h.saturating_sub(1).max(1);
    w.height = u32::try_from(height).unwrap_or(u32::MAX);
    let width = area.w.max(1);
    let total = text.total_bytes();
    let p = text.floor_boundary(w.point.min(total));
    let p_line = offset_to_line(text, p).expect("floor_boundary is valid");
    let top_line = crate::line_at(text, w.top);
    if p < w.top || p_line.saturating_sub(top_line) >= height {
        w.top = line_to_offset(text, p_line).expect("p_line < total_lines");
    }
    let col = column(text, p, m);
    if col < w.left_col {
        w.left_col = col;
    } else if col >= w.left_col + width {
        w.left_col = col + 1 - width;
    }
    w
}

/// Builds the screen. Pure: call `scroll` first to bring point into view.
pub fn frame(ed: &Editor, cols: usize, rows: usize) -> Frame {
    let m = ed.config().metrics();
    let mut grid = vec![Cell::space(); cols * rows];
    let leaves = ed.windows().windows();
    let mut cursor = (0, 0);
    let reversed = reversed_ranges(ed);

    for (i, (w, area)) in leaves.into_iter().zip(areas(ed, cols, rows)).enumerate() {
        let buf = ed.buffer_by_id(w.buffer);
        let syntax = ed
            .config()
            .toggles
            .contains(Toggles::SYNTAX_HIGHLIGHT)
            .then(|| ed.syntax_registry().find_by_filename(buf.name()))
            .flatten();
        let marks = if i == ed.windows().selected() {
            reversed.as_slice()
        } else {
            &[]
        };
        let point = w.point.min(buf.text().total_bytes());
        let point_col = column(buf.text(), point, m);
        let (next_grid, (row, col)) =
            window_cells_into(grid, cols, rows, syntax, buf, w, area, m, marks, point_col);
        grid = next_grid;
        let status_row = area.y + area.h.saturating_sub(1);
        if status_row < rows && area.x < cols {
            let max_w = area.w.min(cols - area.x);
            let start = status_row * cols + area.x;
            grid = fit_into(
                grid,
                start,
                max_w,
                &status_text_with_col(buf, w.point, point_col, area.w, m),
                '-',
                m,
            );
        }
        if i == ed.windows().selected() {
            cursor = (area.x + col, area.y + row);
        }
    }

    if rows > 0 && cols > 0 {
        // While a prompt is shown, the cursor sits in the echo area and the tail of a long line stays visible.
        let message = match ed.mode() {
            Mode::Prompt(p) if p.matches_echo(ed.message()) => {
                let want = (p.cursor_width(m) + 1).saturating_sub(cols);
                let (rest, skipped) = skip_cells(ed.message(), want, m);
                let x = p.cursor_width(m).saturating_sub(skipped);
                cursor = (x.min(cols.saturating_sub(1)), rows - 1);
                rest
            }
            // A pending prefix key shows until the sequence ends.
            Mode::Normal {
                prefix: Prefix::CtlX,
            } if ed.config().toggles.contains(Toggles::ECHO_PREFIX) => "C-x-",
            Mode::Normal {
                prefix: Prefix::Meta,
            } if ed.config().toggles.contains(Toggles::ECHO_PREFIX) => "ESC-",
            _ => ed.message(),
        };
        let start = (rows - 1) * cols;
        grid = fit_into(grid, start, cols, message, ' ', m);
    }

    Frame {
        cells: grid,
        cols,
        cursor,
    }
}

/// Writes the text area of one window into `grid` and returns `(grid, (row, col))`.
#[allow(clippy::too_many_arguments)]
fn window_cells_into(
    mut grid: Vec<Cell>,
    grid_cols: usize,
    grid_rows: usize,
    syntax: Option<&SyntaxDef>,
    buf: &Buffer,
    window: &Window,
    area: Rect,
    metrics: Metrics,
    marks: &[(usize, usize)],
    point_col: usize,
) -> (Vec<Cell>, (usize, usize)) {
    let text = buf.text();
    let height = area.h.saturating_sub(1);
    let total = text.total_bytes();
    let total_lines = total_lines(text);
    let point = window.point.min(total);
    let mut cursor = (0, 0);
    let first_line = crate::line_at(text, window.top);
    let mut start = line_to_offset(text, first_line).expect("line exists");
    for (drawn_rows, line) in (first_line..total_lines).take(height).enumerate() {
        let next = if line + 1 < total_lines {
            Some(line_to_offset(text, line + 1).expect("next line exists"))
        } else {
            None
        };
        let end = next.map_or(total, |offset| offset - 1);
        if start <= point && point <= end {
            cursor = (drawn_rows, point_col.saturating_sub(window.left_col));
        }
        let target_row = area.y + drawn_rows;
        if target_row < grid_rows && area.x < grid_cols {
            let max_w = area.w.min(grid_cols - area.x);
            let row_start = target_row * grid_cols + area.x;
            let start_state = buf.syntax_start_state(line).unwrap_or(LexState::Normal);
            grid = line_cells_into(
                grid,
                row_start,
                max_w,
                syntax,
                start_state,
                text,
                start..end,
                window.left_col,
                metrics,
                marks,
            );
        }
        start = next.unwrap_or(total);
    }
    (grid, cursor)
}

const HUGE_FILE_SYNTAX_BYTES: usize = 100 << 20;
const SYNTAX_PREFIX_BUDGET: usize = 4096;
const LINE_SCAN_BYTES: usize = 64 * 1024;
const CELL_GLYPH_BYTES: usize = 8 * 1024;

/// Writes cells of one line from `left` across `width` columns at `grid[row_start..]`.
/// A line cut at the right edge or by the viewport scan limit shows `$` in the last column (A5).
#[allow(clippy::too_many_arguments)]
fn line_cells_into(
    mut grid: Vec<Cell>,
    row_start: usize,
    width: usize,
    syntax: Option<&SyntaxDef>,
    start_state: LexState,
    text: &Doc,
    line: core::ops::Range<usize>,
    left: usize,
    metrics: Metrics,
    marks: &[(usize, usize)],
) -> Vec<Cell> {
    let cells = &mut grid[row_start..row_start + width];
    let (start, end) = (line.start, line.end);
    let right = left + width;
    let huge_file = text.total_bytes() >= HUGE_FILE_SYNTAX_BYTES;
    let long_line = end - start > SYNTAX_PREFIX_BUDGET;
    let (mut pos, mut col) = if left > 0 && (huge_file || long_line) {
        pos_and_column(text, start, left - 1, metrics)
    } else {
        (start, 0)
    };

    let heading_style = syntax.and_then(|def| {
        let prefix = def.heading_prefix.as_deref()?;
        Some(if prefix.is_empty() {
            TokenKind::Keyword
        } else {
            let probe_end = start.saturating_add(prefix.len()).min(end);
            highlight_line(syntax, text, start..probe_end)
                .first()
                .copied()
                .unwrap_or(TokenKind::Normal)
        })
    });
    // Do not restart the lexer inside a token. For huge files, omit styles when
    // reaching the viewport with the correct context would exceed the prefix budget.
    let token_kinds = if let Some(def) = syntax
        && heading_style.is_none()
        && (!huge_file || pos - start <= SYNTAX_PREFIX_BUDGET)
    {
        let scan_end = if huge_file || long_line {
            // Combining marks occupy no columns. Bound the lexer input independently of width.
            let limit = text.floor_boundary(start.saturating_add(LINE_SCAN_BYTES).min(end));
            let (mut cap_end, mut scan_col) = (pos, col);
            while cap_end < limit && scan_col < right {
                let Some((unit, len)) = text.decode_at(cap_end) else {
                    break;
                };
                scan_col += unit_width(unit, scan_col, metrics);
                cap_end += len;
            }
            // Finish the token so truncation cannot turn an identifier into a keyword or versa.
            let max_word = def
                .keywords
                .iter()
                .chain(&def.types)
                .map(String::len)
                .max()
                .unwrap_or(0);
            let word_limit = cap_end.saturating_add(max_word).min(limit);
            while cap_end < word_limit {
                let Some(b) = text.byte_at(cap_end) else {
                    break;
                };
                if crate::syntax::is_ident_byte(b) {
                    cap_end += 1;
                } else {
                    break;
                }
            }
            // Prefixes beginning in the visible region must not be cut short.
            let prefix_len = def.line_comment.as_ref().map_or(0, String::len);
            let lookahead = cap_end
                .saturating_add(prefix_len.saturating_sub(1))
                .min(end);
            // If the budget truncates a token or prefix, do not color a partial match.
            (lookahead < limit || (lookahead == limit && limit == end)).then_some(lookahead)
        } else {
            Some(end)
        };
        scan_end.map_or_else(Vec::new, |end| {
            highlight_line_with_state(syntax, text, start..end, start_state).0
        })
    } else {
        Vec::new()
    };

    let draw_end = text.floor_boundary(pos.saturating_add(LINE_SCAN_BYTES).min(end));
    while pos < draw_end && col < right {
        let Some((unit, len)) = text.decode_at(pos) else {
            break;
        };
        let width_of_unit = unit_width(unit, col, metrics);
        if col + width_of_unit <= left {
            col += width_of_unit;
            pos += len;
            continue;
        }
        let style = heading_style.unwrap_or_else(|| {
            token_kinds
                .get(pos - start)
                .copied()
                .unwrap_or(TokenKind::Normal)
        });
        let reverse = marks.iter().any(|&(from, to)| from <= pos && pos < to);
        if width_of_unit == 0 {
            // Combining mark: attach to the cell before it.
            if col > left && col - left - 1 < width {
                let idx = col - left - 1;
                let glyph = core::mem::replace(&mut cells[idx].glyph, CellGlyph::EMPTY);
                let mark = unit_char(unit);
                cells[idx].glyph = if glyph.len() + mark.len_utf8() <= CELL_GLYPH_BYTES {
                    glyph.push(mark)
                } else {
                    glyph
                };
            }
        } else if col + width_of_unit > right {
            // Do not place a wide glyph that overflows the right edge; A5 marks the cut with `$`.
            break;
        } else {
            for (offset, piece) in glyph(unit_char(unit), width_of_unit)
                .into_iter()
                .enumerate()
            {
                let cell_col = col + offset;
                if cell_col >= left && cell_col < right {
                    cells[cell_col - left] = Cell {
                        glyph: piece,
                        style,
                        reverse,
                    };
                }
            }
        }
        col += width_of_unit;
        pos += len;
    }
    if pos < end && width > 0 {
        cells[width - 1] = Cell::plain(CellGlyph::from_ascii(b'$'));
    }
    grid
}

#[cfg(test)]
fn line_cells(
    syntax: Option<&SyntaxDef>,
    text: &Doc,
    line: core::ops::Range<usize>,
    left: usize,
    width: usize,
    metrics: Metrics,
    marks: &[(usize, usize)],
) -> Vec<Cell> {
    let cells = vec![Cell::space(); width];
    line_cells_into(
        cells,
        0,
        width,
        syntax,
        LexState::Normal,
        text,
        line,
        left,
        metrics,
        marks,
    )
}

/// `w` cells for one character: control characters as `^X`, C1 as `\HH`,
/// tabs as spaces, and wide characters followed by an empty cell.
fn glyph(c: char, w: usize) -> Vec<CellGlyph> {
    if c == '\t' {
        return vec![CellGlyph::SPACE; w];
    }
    if let Ok(b) = u8::try_from(c)
        && b < 0x20
    {
        return vec![CellGlyph::from_ascii(b'^'), CellGlyph::from_ascii(b + 64)];
    }
    let code = c as u32;
    if code == 0x7F {
        return vec![CellGlyph::from_ascii(b'^'), CellGlyph::from_ascii(b'?')];
    }
    if (0x80..=0x9F).contains(&code) {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let b = code as usize;
        return vec![
            CellGlyph::from_ascii(b'\\'),
            CellGlyph::from_ascii(HEX[b >> 4]),
            CellGlyph::from_ascii(HEX[b & 0xF]),
        ];
    }
    let mut cells = Vec::with_capacity(w);
    cells.push(CellGlyph::from_char(c));
    cells.extend(core::iter::repeat_n(CellGlyph::EMPTY, w.saturating_sub(1)));
    cells
}

/// The status line for a window `width` cells wide. Line and column are 1-based; the column is the display column.
/// What does not fit is dropped in this order: the EOL mode, then the percentage.
#[cfg(test)]
fn status_text(buf: &Buffer, point: usize, width: usize, m: Metrics) -> String {
    let col = column(buf.text(), point.min(buf.text().total_bytes()), m);
    status_text_with_col(buf, point, col, width, m)
}

fn status_text_with_col(
    buf: &Buffer,
    point: usize,
    display_col: usize,
    width: usize,
    m: Metrics,
) -> String {
    let mark = if buf.modified() { "**" } else { "--" };
    let view = if buf.view_only() { " (view)" } else { "" };
    let percent = (point as u128 * 100)
        .checked_div(buf.text().total_bytes() as u128)
        .unwrap_or(100);
    let text = buf.text();
    let line = crate::line_at(text, point) + 1;
    let col = display_col + 1;
    let head = format!("-{mark}-  {}{view}", buf.name());
    let eol = buf.eol().label();
    let pos = format!("{line}:{col}");
    [
        format!("{head}  {eol}  {pos}  {percent}%"),
        format!("{head}  {pos}  {percent}%"),
    ]
    .into_iter()
    .find(|s| crate::width::str_width(s, m) <= width)
    .unwrap_or_else(|| format!("{head}  {pos}"))
}

/// Drops leading characters until at least `cells` display columns are gone. Returns the rest and the columns dropped.
fn skip_cells(s: &str, cells: usize, m: Metrics) -> (&str, usize) {
    let mut skipped = 0;
    for (i, c) in s.char_indices() {
        if skipped >= cells {
            return (&s[i..], skipped);
        }
        skipped += m.char(c);
    }
    ("", skipped)
}

/// Fits `s` into `width` cells at `grid[start..start + width]`, padding with `fill`.
fn fit_into(
    mut grid: Vec<Cell>,
    start: usize,
    width: usize,
    s: &str,
    fill: char,
    m: Metrics,
) -> Vec<Cell> {
    let out = &mut grid[start..start + width];
    let mut col = 0;
    for c in s.chars() {
        let w = m.char(c);
        if w == 0 {
            if col > 0 {
                let prev = core::mem::replace(&mut out[col - 1].glyph, CellGlyph::EMPTY);
                out[col - 1].glyph = prev.push(c);
            }
            continue;
        }
        if col + w > width {
            break;
        }
        out[col] = Cell::plain(CellGlyph::from_char(c));
        for slot in &mut out[col + 1..col + w] {
            *slot = Cell::plain(CellGlyph::EMPTY);
        }
        col += w;
    }
    let fill_cell = Cell::plain(CellGlyph::from_char(fill));
    for slot in &mut out[col..] {
        *slot = fill_cell.clone();
    }
    grid
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::eol::Eol;
    use alloc::borrow::ToOwned;

    fn screen(text: &str, point: usize, cols: usize, rows: usize) -> Frame {
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(Doc::from_bytes(text.as_bytes())))
            .with_window(|w| Window { point, ..w });
        let ed = scroll(ed, cols, rows);
        frame(&ed, cols, rows)
    }

    fn screen_rows(text: &str, point: usize, cols: usize, rows: usize) -> Vec<String> {
        screen(text, point, cols, rows).row_strings()
    }

    use crate::test_support::{keys, set_mark, typ};

    fn reversed(frame: &Frame, row: usize) -> Vec<bool> {
        frame.row(row).iter().map(|c| c.reverse).collect()
    }

    fn drawn(ed: Editor, cols: usize, rows: usize) -> Frame {
        frame(&scroll(ed, cols, rows), cols, rows)
    }

    #[test]
    fn the_active_region_is_reversed_and_only_it() {
        let ed = set_mark(keys(typ(Editor::new(Config::default()), "abc def"), "C-a"));
        let ed = ed.with_window(|w| Window { point: 4, ..w });
        let f = drawn(ed.clone(), 10, 4);
        let want: Vec<bool> = (0..10).map(|i| (0..4).contains(&i)).collect();
        assert_eq!(reversed(&f, 0), want);

        // Point before the mark gives the same region.
        let ed = ed
            .with_current_buffer(|b| b.set_mark(Some(4)))
            .with_window(|w| Window { point: 0, ..w });
        assert_eq!(reversed(&drawn(ed, 10, 4), 0), want);
    }

    #[test]
    fn an_inactive_mark_shows_nothing() {
        let ed = set_mark(keys(typ(Editor::new(Config::default()), "abc def"), "C-a"));
        let ed = keys(ed, "C-g");
        let f = drawn(ed, 10, 4);
        assert!(f.rows().flatten().all(|c| !c.reverse));
    }

    fn cells_of(s: &str, left: usize, width: usize) -> Vec<String> {
        let text = Doc::from_bytes(s.as_bytes());
        line_cells(
            None,
            &text,
            0..text.total_bytes(),
            left,
            width,
            Metrics { tab: 8, cjk: false },
            &[],
        )
        .into_iter()
        .map(|c| c.glyph.as_str().to_owned())
        .collect()
    }

    #[test]
    fn viewport_streams_lines_across_slices_through_the_last_line() {
        use crate::{DocBuilder, MAX_SLICE_BYTES};

        let doc = DocBuilder::new()
            .push_bytes(&vec![b'\n'; MAX_SLICE_BYTES - 1])
            .push_bytes(b"abc")
            .push_bytes(b"def\n\nlast")
            .finish();
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(doc))
            .with_window(|w| Window {
                top: MAX_SLICE_BYTES - 1,
                point: MAX_SLICE_BYTES - 1 + b"abcdef\n\nla".len(),
                ..w
            });
        let f = frame(&ed, 8, 6);
        assert_eq!(
            &f.row_strings()[..4],
            ["abcdef  ", "        ", "last    ", "        "]
        );
        assert_eq!(f.cursor, (2, 2));
    }

    #[test]
    fn combining_mark_joins_previous_cell_or_drops_at_left_edge() {
        assert_eq!(cells_of("e\u{301}x", 0, 4), ["e\u{301}", "x", " ", " "]);
        // The base "e" is scrolled off; its mark sits at column 1 == left and has no cell to join.
        assert_eq!(cells_of("e\u{301}x", 1, 3), ["x", " ", " "]);
    }

    #[test]
    fn a_region_over_wide_hangul_reverses_both_cells_of_each_glyph() {
        // "가나다": 나 spans bytes 3..6 and cells 2..4.
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| {
                b.with_text(Doc::from_bytes("가나다".as_bytes()))
                    .set_mark(Some(3))
            })
            .with_window(|w| Window { point: 6, ..w });
        let f = drawn(ed, 10, 4);
        let want: Vec<bool> = (0..10).map(|i| (2..4).contains(&i)).collect();
        assert_eq!(reversed(&f, 0), want);
    }

    #[test]
    fn only_the_selected_window_shows_the_region() {
        let ed = set_mark(keys(typ(Editor::new(Config::default()), "abc"), "C-a"));
        let ed = ed.with_window(|w| Window { point: 2, ..w });
        let ed = ed.with_windows(|wm| wm.split(Dir::Below));
        let f = drawn(ed, 10, 8);
        let with_region = (0..8)
            .filter(|&r| f.row(r).iter().any(|c| c.reverse))
            .count();
        assert_eq!(with_region, 1);
    }

    #[test]
    fn the_current_search_match_is_reversed() {
        let ed = keys(typ(Editor::new(Config::default()), "abc def"), "C-a C-s");
        let ed = typ(ed, "de");
        let f = drawn(ed, 10, 4);
        let want: Vec<bool> = (0..10).map(|i| (4..6).contains(&i)).collect();
        assert_eq!(reversed(&f, 0), want);
    }

    #[test]
    fn the_query_replace_match_is_reversed() {
        use crate::commands::search::start_query_replace;
        use crate::editor::MatchKind;
        let ed = keys(typ(Editor::new(Config::default()), "abc def"), "C-a");
        let (ed, res) = start_query_replace(ed, "def", "x", MatchKind::Literal);
        res.unwrap();
        let want: Vec<bool> = (0..10).map(|i| (4..7).contains(&i)).collect();
        assert_eq!(reversed(&drawn(ed, 10, 4), 0), want);
    }

    #[test]
    fn echo_prefix_pending_keys_and_toggle_behavior() {
        let echo = |ed: Editor| drawn(ed, 12, 4).row_strings()[3].trim_end().to_owned();

        let off = Editor::new(Config {
            toggles: Toggles::DEFAULT.difference(Toggles::ECHO_PREFIX),
            ..Config::default()
        })
        .set_message("Wrote f");
        assert_eq!(echo(keys(off.clone(), "C-x")), "Wrote f");
        assert_eq!(echo(keys(off, "ESC")), "Wrote f");

        let on = Editor::new(Config::default()).set_message("Wrote f");
        assert_eq!(echo(on.clone()), "Wrote f");
        assert_eq!(echo(keys(on.clone(), "C-x")), "C-x-");
        assert_eq!(echo(keys(on.clone(), "ESC")), "ESC-");
        // The message returns once the sequence ends.
        assert_eq!(echo(keys(on, "C-x C-g")), "Quit");
    }

    #[test]
    fn syntax_highlight_off_draws_every_cell_plain() {
        let styled = |highlight: bool| {
            let ed = Editor::new(Config {
                toggles: if highlight {
                    Toggles::DEFAULT
                } else {
                    Toggles::DEFAULT.difference(Toggles::SYNTAX_HIGHLIGHT)
                },
                ..Config::default()
            })
            .with_syntax_registry(|reg| {
                reg.register(SyntaxDef::new(
                    "t",
                    vec!["t".to_owned()],
                    vec![],
                    None,
                    vec!["let".to_owned()],
                    vec![],
                    false,
                ))
            });
            let (ed, id) =
                ed.add_buffer("a.t", Doc::from_bytes(b"let x"), crate::file::Backing::None);
            let ed = ed.with_window(|w| Window {
                buffer: id,
                point: 0,
                ..w
            });
            drawn(ed, 10, 4)
                .row(0)
                .iter()
                .any(|c| c.style != TokenKind::Normal)
        };
        assert!(styled(true));
        assert!(!styled(false));
    }

    #[test]
    fn scroll_records_body_height_per_window() {
        let ed = Editor::new(Config::default());
        assert_eq!(ed.window().height, 0);
        let ed = scroll(ed, 20, 6);
        assert_eq!(ed.window().height, 4);
        let ed = scroll(ed.with_windows(|wm| wm.split(Dir::Below)), 20, 10);
        assert!(ed.windows().windows().iter().all(|w| w.height >= 1));
    }

    #[test]
    fn frame_hangul_and_tab() {
        let r = screen_rows("가나\na\tb", 0, 10, 4);
        assert_eq!(r[0], format!("가나{}", " ".repeat(6)));
        assert_eq!(r[1], "a       b ");
    }

    #[test]
    fn right_edge_overflow_clips_wide_and_long_lines_with_dollar() {
        assert_eq!(screen_rows("abcd가", 0, 5, 3)[0], "abcd$");
        assert_eq!(screen_rows("가", 0, 1, 3)[0], "$");
        assert_eq!(screen_rows("abcdefghijklmnop", 0, 10, 3)[0], "abcdefghi$");
    }

    #[test]
    fn frame_control_and_c1_glyphs() {
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(Doc::from_bytes(&[0x01, b'x', 0x85])));
        let ed = scroll(ed, 10, 3);
        let f = frame(&ed, 10, 3);
        assert_eq!(f.row_strings()[0], "^Ax\\85    ");
    }

    #[test]
    fn frame_status_line_shows_name_state_and_percent() {
        let ed = Editor::new(Config::default()).with_current_buffer(|b| {
            b.with_text(Doc::from_bytes(b"abc"))
                .mark_modified()
                .set_view_only(true)
                .set_name("f.txt")
        });
        let ed = scroll(ed, 40, 3);
        let f = frame(&ed, 40, 3);
        let r = f.row_strings();
        assert!(r[1].starts_with("-**-  f.txt (view)  LF  1:1  0%"));
        assert!(r[1].ends_with('-'));
    }

    fn status_editor(eol: Eol) -> Editor {
        Editor::new(Config::default()).with_current_buffer(|b| {
            b.with_text(Doc::from_bytes(b"abc"))
                .set_eol(eol)
                .set_name("f.txt")
        })
    }

    #[test]
    fn status_line_shows_eol_and_drops_segments_by_width() {
        let cases = [
            (Eol::Crlf, 26, "----  f.txt  CRLF  1:1  0%".to_owned()),
            (Eol::Lf, 24, "----  f.txt  LF  1:1  0%".to_owned()),
            // Full text is 26 cells. At 25 the EOL goes and the position and percent stay.
            (Eol::Crlf, 25, format!("{:-<25}", "----  f.txt  1:1  0%")),
            // Without EOL and percent the text is 16 cells.
            (Eol::Crlf, 19, format!("{:-<19}", "----  f.txt  1:1")),
        ];
        for (eol, width, expected) in cases {
            let ed = scroll(status_editor(eol), width, 3);
            assert_eq!(frame(&ed, width, 3).row_strings()[1], expected);
        }
    }

    #[test]
    fn status_line_position_counts_lines_and_display_columns() {
        // Line 2 is "가", a tab to column 8, and "b": point at the end sits in display column 9.
        let text = Doc::from_bytes(
            "a
가	b"
                .as_bytes(),
        );
        let buf = Buffer::new(
            crate::buffer::BufferId(1),
            "t",
            text,
            crate::file::Backing::None,
        );
        let end = buf.text().total_bytes();
        assert!(status_text(&buf, end, 60, Metrics { tab: 8, cjk: false }).contains("  2:10  "));
        assert!(status_text(&buf, 0, 60, Metrics { tab: 8, cjk: false }).contains("  1:1  "));
        // Point past the end is clamped.
        assert!(
            status_text(&buf, end + 99, 60, Metrics { tab: 8, cjk: false }).contains("  2:10  ")
        );
    }

    #[test]
    fn frame_cursor_and_scroll_follow_point() {
        let f = screen("가b", 3, 10, 4);
        assert_eq!(f.cursor, (2, 0));

        let r = screen_rows("1\n2\n3\n4\n5", 8, 10, 4);
        assert_eq!(r[0], "5         ");
    }

    #[test]
    fn status_line_large_point_percentage_calculation() {
        let text = Doc::from_bytes(b"hello world");
        let buf = Buffer::new(
            crate::buffer::BufferId(1),
            "test.txt",
            text,
            crate::file::Backing::None,
        );
        let s = status_text(&buf, 5, 40, Metrics { tab: 8, cjk: false });
        // 5 * 100 / 11 = 45%
        assert!(s.contains("45%"));
    }

    fn prompt_editor(before: &str, after: &str) -> Editor {
        use crate::editor::{Prompt, Purpose};
        let p = Prompt {
            label: "Find file: ".into(),
            before: before.into(),
            after: after.into(),
            purpose: Purpose::FindFile,
        };
        let msg = p.echo();
        Editor::new(Config::default())
            .set_mode(Mode::Prompt(p))
            .set_message(msg)
    }

    #[test]
    fn prompt_cursor_scroll_and_note_message_behavior() {
        // "Find file: " is 11 columns; "가" is 2 and "a" is 1.
        let ed = prompt_editor("가a", "bc");
        assert_eq!(frame(&ed, 40, 4).cursor, (14, 3));

        let ed = prompt_editor(&"x".repeat(30), "");
        let f = frame(&ed, 20, 4);
        assert_eq!(f.cursor, (19, 3));
        let echo = f.row_strings()[3].clone();
        assert!(echo.starts_with(&"x".repeat(19)), "{echo}");

        let ed = prompt_editor("is", "").set_message("No match");
        assert_eq!(frame(&ed, 40, 4).cursor, (0, 0));
    }

    #[test]
    fn long_line_highlighting_covers_visible_tokens_after_combining_marks() {
        let m = Metrics { tab: 8, cjk: false };
        let def = SyntaxDef::new(
            "t",
            vec!["t".to_owned()],
            vec![],
            None,
            vec!["let".to_owned()],
            vec![],
            false,
        );
        for left in [0, 4500] {
            let input = format!("{}a{} let", "x".repeat(left), "\u{0301}".repeat(3000));
            let text = Doc::from_bytes(input.as_bytes());
            let cells = line_cells(Some(&def), &text, 0..text.total_bytes(), left, 20, m, &[]);
            assert_eq!(cells[0].glyph, format!("a{}", "\u{0301}".repeat(3000)));
            assert_eq!(
                highlight_line(Some(&def), &text, 0..input.len())[input.len() - 3],
                TokenKind::Keyword
            );
            assert!(
                cells[2..5].iter().all(|c| c.style == TokenKind::Keyword),
                "left={left}"
            );
        }
    }

    #[test]
    fn huge_combining_line_bounds_lexer_input_and_cell_glyphs() {
        let m = Metrics { tab: 8, cjk: false };
        let def = SyntaxDef::new(
            "t",
            vec!["t".to_owned()],
            vec![],
            None,
            vec!["a".to_owned()],
            vec![],
            false,
        );
        let mut marks = Doc::from_bytes("\u{0301}".repeat(8192).as_bytes());
        for _ in 0..13 {
            let insert = marks.clone();
            (marks, _) = marks
                .apply(crate::Replace {
                    offset: 0,
                    delete_len: 0,
                    insert,
                })
                .unwrap();
        }
        let (text, _) = marks.replace(0, 0, b"a").unwrap();
        assert!(text.total_bytes() >= HUGE_FILE_SYNTAX_BYTES);
        let cells = line_cells(Some(&def), &text, 0..text.total_bytes(), 0, 20, m, &[]);
        assert!(cells[0].glyph.starts_with("a\u{0301}"));
        assert!(cells[0].glyph.len() <= CELL_GLYPH_BYTES);
        assert_eq!(cells[0].style, TokenKind::Normal);
        assert_eq!(cells[19].glyph, "$");
    }

    #[test]
    fn long_line_highlighting_preserves_comment_prefixes_crossing_right_edge() {
        let m = Metrics { tab: 8, cjk: false };
        for prefix in ["///", "<!--", "※※"] {
            let def = SyntaxDef::new(
                "t",
                vec!["t".to_owned()],
                vec![],
                Some(prefix.to_owned()),
                vec![],
                vec![],
                false,
            );
            for left in [0, 4500] {
                let input = format!("{}{prefix}{}", " ".repeat(left + 18), "x".repeat(5000));
                let text = Doc::from_bytes(input.as_bytes());
                let cells = line_cells(Some(&def), &text, 0..text.total_bytes(), left, 20, m, &[]);
                assert_eq!(
                    cells[18].style,
                    TokenKind::Comment,
                    "prefix={prefix}, left={left}"
                );
                assert_eq!(cells[19].glyph, "$");
            }
        }
    }

    #[test]
    fn long_line_highlighting_checks_full_heading_prefix_beyond_right_edge() {
        let m = Metrics { tab: 8, cjk: false };
        let prefix = format!("{}###", " ".repeat(18));
        let def = SyntaxDef::new(
            "t",
            vec!["t".to_owned()],
            vec![],
            None,
            vec![],
            vec![],
            false,
        )
        .with_heading_prefix(Some(prefix.clone()));
        for (ending, expected) in [("###", TokenKind::Keyword), ("##!", TokenKind::Normal)] {
            let input = format!("{}{ending}{}", " ".repeat(18), "x".repeat(5000));
            let text = Doc::from_bytes(input.as_bytes());
            let cells = line_cells(Some(&def), &text, 0..text.total_bytes(), 0, 20, m, &[]);
            assert_eq!(
                highlight_line(Some(&def), &text, 0..input.len())[18],
                expected
            );
            assert!(cells[..19].iter().all(|c| c.style == expected));
        }
    }

    #[test]
    fn long_line_highlighting_finishes_the_visible_token() {
        let m = Metrics { tab: 8, cjk: false };
        let keyword = "a".repeat(5000);
        let def = SyntaxDef::new(
            "t",
            vec!["t".to_owned()],
            vec![],
            None,
            vec![keyword.clone()],
            vec![],
            false,
        );
        for suffix in ["", "b"] {
            let text = Doc::from_bytes(format!("{keyword}{suffix}").as_bytes());
            let cells = line_cells(Some(&def), &text, 0..text.total_bytes(), 0, 20, m, &[]);
            let expected = if suffix.is_empty() {
                TokenKind::Keyword
            } else {
                TokenKind::Normal
            };
            assert!(cells[..19].iter().all(|c| c.style == expected));
        }
    }

    #[test]
    fn scrolled_long_line_preserves_heading_and_line_comment_highlighting() {
        let m = Metrics { tab: 8, cjk: false };

        let heading_def = SyntaxDef::new(
            "notes",
            vec!["md".to_owned()],
            vec![],
            None,
            vec!["let".to_owned()],
            vec![],
            false,
        )
        .with_heading_prefix(Some("#".to_owned()));
        let heading = Doc::from_bytes(format!("# {}", "a".repeat(5000)).as_bytes());
        let cells = line_cells(
            Some(&heading_def),
            &heading,
            0..heading.total_bytes(),
            4500,
            10,
            m,
            &[],
        );
        assert!(cells[..9].iter().all(|c| c.style == TokenKind::Keyword));

        let non_heading =
            Doc::from_bytes(format!("{}#{}", "a".repeat(2500), "b".repeat(3000)).as_bytes());
        let cells = line_cells(
            Some(&heading_def),
            &non_heading,
            0..non_heading.total_bytes(),
            4500,
            10,
            m,
            &[],
        );
        assert!(cells.iter().all(|c| c.style == TokenKind::Normal));

        let rust_def = SyntaxDef::new(
            "rust",
            vec!["rs".to_owned()],
            vec![],
            Some("//".to_owned()),
            vec!["let".to_owned()],
            vec![],
            false,
        );
        let comment = Doc::from_bytes(format!("// {} let x = 1;", "a".repeat(4500)).as_bytes());
        let cells = line_cells(
            Some(&rust_def),
            &comment,
            0..comment.total_bytes(),
            4505,
            10,
            m,
            &[],
        );
        assert!(cells[..9].iter().all(|c| c.style == TokenKind::Comment));
    }

    fn huge_doc_with_prefix(prefix: &[u8]) -> Doc {
        // Build a >100 MiB Doc via O(log N) structural sharing, then prepend `prefix`.
        let mut huge = Doc::from_bytes(&vec![b'x'; 16 * 1024]);
        for _ in 0..13 {
            let dup = huge.clone();
            huge = huge
                .apply(crate::Replace {
                    offset: 0,
                    delete_len: 0,
                    insert: dup,
                })
                .unwrap()
                .0;
        }
        assert!(huge.total_bytes() >= HUGE_FILE_SYNTAX_BYTES);
        if prefix.is_empty() {
            huge
        } else {
            huge.apply(crate::Replace {
                offset: 0,
                delete_len: 0,
                insert: Doc::from_bytes(prefix),
            })
            .unwrap()
            .0
        }
    }

    #[test]
    fn viewport_draws_lines_after_a_large_terminated_line() {
        let doc = huge_doc_with_prefix(b"");
        let doc = doc
            .clone()
            .replace(doc.total_bytes(), 0, b"\n\nlast")
            .unwrap()
            .0;
        let ed = Editor::new(crate::Config::default()).with_current_buffer(|b| b.with_text(doc));
        let rows = frame(&ed, 8, 5).row_strings();
        assert_eq!(&rows[..3], ["xxxxxxx$", "        ", "last    "]);
    }

    #[test]
    fn huge_file_preserves_comment_context_or_omits_highlighting() {
        let rust_def = SyntaxDef::new(
            "rust",
            vec!["rs".to_owned()],
            vec![],
            Some("//".to_owned()),
            vec!["let".to_owned()],
            vec![],
            false,
        );
        let line = b"// let x = 1;\n";
        let huge = huge_doc_with_prefix(line);
        let m = Metrics { tab: 8, cjk: false };
        let cells = line_cells(Some(&rust_def), &huge, 0..line.len() - 1, 3, 10, m, &[]);
        assert_eq!(cells[0].style, TokenKind::Comment);

        let padding = " ".repeat(SYNTAX_PREFIX_BUDGET + 1);
        let line = format!("// {padding}let x = 1;\n");
        let huge = huge_doc_with_prefix(line.as_bytes());
        let cells = line_cells(
            Some(&rust_def),
            &huge,
            0..line.len() - 1,
            3 + padding.len(),
            10,
            m,
            &[],
        );
        assert_eq!(cells[0].glyph, "l");
        assert_eq!(cells[0].style, TokenKind::Normal);
    }

    #[test]
    fn huge_file_heading_prefix_checks_line_start_when_scrolled() {
        let m = Metrics { tab: 8, cjk: false };
        let heading_def = SyntaxDef::new(
            "notes",
            vec!["md".to_owned()],
            vec![],
            None,
            vec![],
            vec![],
            false,
        )
        .with_heading_prefix(Some("#".to_owned()));

        // False-positive guard: non-heading line with '#' at column left - 1 stays Normal (I-49).
        let non_heading_line = b"abc#def\n";
        let non_heading = huge_doc_with_prefix(non_heading_line);
        let cells = line_cells(
            Some(&heading_def),
            &non_heading,
            0..non_heading_line.len() - 1,
            4,
            10,
            m,
            &[],
        );
        assert!(cells.iter().all(|c| c.style == TokenKind::Normal));

        // False-negative guard: scrolled heading keeps Keyword highlighting (I-49).
        let heading_line = b"# heading\n";
        let heading = huge_doc_with_prefix(heading_line);
        let cells = line_cells(
            Some(&heading_def),
            &heading,
            0..heading_line.len() - 1,
            2,
            10,
            m,
            &[],
        );
        assert!(cells[..7].iter().all(|c| c.style == TokenKind::Keyword));
    }

    #[test]
    fn huge_file_single_line_identifier_stays_viewport_bounded() {
        let m = Metrics { tab: 8, cjk: false };
        let rust_def = SyntaxDef::new(
            "rust",
            vec!["rs".to_owned()],
            vec![],
            Some("//".to_owned()),
            vec!["let".to_owned()],
            vec!["usize".to_owned()],
            false,
        );
        // 128 MiB single-line identifier of 'x' bytes; must stay strictly viewport-bounded.
        let huge = huge_doc_with_prefix(b"");
        let cells = line_cells(Some(&rust_def), &huge, 0..huge.total_bytes(), 0, 80, m, &[]);
        assert_eq!(cells.len(), 80);
        assert!(
            cells[..79]
                .iter()
                .all(|c| c.glyph == "x" && c.style == TokenKind::Normal)
        );
        assert_eq!(cells[79].glyph, "$");
    }

    #[test]
    fn cell_glyph_from_ascii_accepts_ascii() {
        assert_eq!(CellGlyph::from_ascii(b'A').as_str(), "A");
    }

    #[test]
    #[should_panic(expected = "CellGlyph::from_ascii requires ASCII byte")]
    fn cell_glyph_from_ascii_rejects_non_ascii() {
        let _ = CellGlyph::from_ascii(0x80);
    }
}
