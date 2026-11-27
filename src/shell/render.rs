//! Terminal screen cell-grid assembly (§5, §11, A5).
//! `scroll` lives in `neonemacs_core::render`; this module assembles styled cells into a
//! reusable flat grid (`render_into`) or a standalone `Frame` (`frame`).

use crate::core::render::{HUGE_FILE_SYNTAX_BYTES, Rect, areas};
use crate::core::width::{Metrics, column, pos_and_column, str_width, unit_width};
use crate::core::{
    Buffer, Doc, Editor, LexState, Mode, Prefix, SyntaxDef, Toggles, TokenKind, Window,
    highlight_line, highlight_line_with_state, is_ident_byte, line_at, line_to_offset, total_lines,
    unit_char,
};

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
            CellGlyphInner::Inline { len, buf } => {
                std::str::from_utf8(&buf[..usize::from(*len)]).expect("inline glyph is valid UTF-8")
            }
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
                        std::str::from_utf8(&buf[..cur]).expect("inline glyph is valid UTF-8"),
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

impl std::ops::Deref for CellGlyph {
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

/// Reusable scratch storage for `render_into` to avoid per-frame allocations.
#[derive(Debug, Default)]
pub struct RenderScratch {
    reversed: Vec<(usize, usize)>,
}

/// Byte ranges drawn in reverse video in the selected window: the active region, and the current
/// search or query-replace match.
fn fill_reversed_ranges(ed: &Editor, out: &mut Vec<(usize, usize)>) {
    out.clear();
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
    pub fn row_strings(&self) -> Vec<String> {
        self.rows()
            .map(|row| row.iter().map(|c| c.glyph.as_str()).collect::<String>())
            .collect()
    }
}

/// Builds the screen into a new `Frame`. Pure: call `core::render::scroll` first to bring point into view.
#[must_use]
pub fn frame(ed: &Editor, cols: usize, rows: usize) -> Frame {
    let mut cells = Vec::new();
    let mut scratch = RenderScratch::default();
    let cursor = render_into(ed, cols, rows, &mut cells, &mut scratch);
    Frame {
        cells,
        cols,
        cursor,
    }
}

/// Renders the screen into `grid` in-place (`grid.len() == cols * rows`), returning the cursor `(x, y)`.
/// Reuses `grid`'s allocation when `cols * rows` is unchanged.
pub fn render_into(
    ed: &Editor,
    cols: usize,
    rows: usize,
    grid: &mut Vec<Cell>,
    scratch: &mut RenderScratch,
) -> (usize, usize) {
    let len = cols * rows;
    if grid.len() == len {
        grid.fill(Cell::space());
    } else {
        grid.clear();
        grid.resize(len, Cell::space());
    }

    let m = ed.config().metrics();
    let leaves = ed.windows().windows();
    let mut cursor = (0, 0);
    fill_reversed_ranges(ed, &mut scratch.reversed);

    for (i, (w, area)) in leaves.into_iter().zip(areas(ed, cols, rows)).enumerate() {
        let buf = ed.buffer_by_id(w.buffer);
        let syntax = ed
            .config()
            .toggles
            .contains(Toggles::SYNTAX_HIGHLIGHT)
            .then(|| ed.syntax_registry().find_by_filename(buf.name()))
            .flatten();
        let marks = if i == ed.windows().selected() {
            scratch.reversed.as_slice()
        } else {
            &[]
        };
        let point = w.point.min(buf.text().total_bytes());
        let point_col = column(buf.text(), point, m);
        let (row, col) =
            window_cells_into(grid, cols, rows, syntax, buf, w, area, m, marks, point_col);
        let status_row = area.y + area.h.saturating_sub(1);
        if status_row < rows && area.x < cols {
            let max_w = area.w.min(cols - area.x);
            let start = status_row * cols + area.x;
            fit_into(
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
        let message = match ed.mode() {
            Mode::Prompt(p) if p.matches_echo(ed.message()) => {
                let want = (p.cursor_width(m) + 1).saturating_sub(cols);
                let (rest, skipped) = skip_cells(ed.message(), want, m);
                let x = p.cursor_width(m).saturating_sub(skipped);
                cursor = (x.min(cols.saturating_sub(1)), rows - 1);
                rest
            }
            Mode::Normal {
                prefix: Prefix::CtlX,
            } if ed.config().toggles.contains(Toggles::ECHO_PREFIX) => "C-x-",
            Mode::Normal {
                prefix: Prefix::Meta,
            } if ed.config().toggles.contains(Toggles::ECHO_PREFIX) => "ESC-",
            _ => ed.message(),
        };
        let start = (rows - 1) * cols;
        fit_into(grid, start, cols, message, ' ', m);
    }

    cursor
}

/// Writes the text area of one window into `grid` and returns `(row, col)`.
#[allow(clippy::too_many_arguments)]
fn window_cells_into(
    grid: &mut [Cell],
    grid_cols: usize,
    grid_rows: usize,
    syntax: Option<&SyntaxDef>,
    buf: &Buffer,
    window: &Window,
    area: Rect,
    metrics: Metrics,
    marks: &[(usize, usize)],
    point_col: usize,
) -> (usize, usize) {
    let text = buf.text();
    let height = area.h.saturating_sub(1);
    let total = text.total_bytes();
    let total_lines = total_lines(text);
    let point = window.point.min(total);
    let mut cursor = (0, 0);
    let first_line = line_at(text, window.top);
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
            line_cells_into(
                &mut grid[row_start..row_start + max_w],
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
    cursor
}

const SYNTAX_PREFIX_BUDGET: usize = 4096;
const LINE_SCAN_BYTES: usize = 64 * 1024;
const CELL_GLYPH_BYTES: usize = 8 * 1024;

/// Writes cells of one line from `left` across `width` columns into `cells`.
/// A line cut at the right edge or by the viewport scan limit shows `$` in the last column (A5).
#[allow(clippy::too_many_arguments)]
fn line_cells_into(
    cells: &mut [Cell],
    width: usize,
    syntax: Option<&SyntaxDef>,
    start_state: LexState,
    text: &Doc,
    line: std::ops::Range<usize>,
    left: usize,
    metrics: Metrics,
    marks: &[(usize, usize)],
) {
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
    let token_kinds = if let Some(def) = syntax
        && heading_style.is_none()
        && (!huge_file || pos - start <= SYNTAX_PREFIX_BUDGET)
    {
        let scan_end = if huge_file || long_line {
            let limit = text.floor_boundary(start.saturating_add(LINE_SCAN_BYTES).min(end));
            let (mut cap_end, mut scan_col) = (pos, col);
            while cap_end < limit && scan_col < right {
                let Some((unit, len)) = text.decode_at(cap_end) else {
                    break;
                };
                scan_col += unit_width(unit, scan_col, metrics);
                cap_end += len;
            }
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
                if is_ident_byte(b) {
                    cap_end += 1;
                } else {
                    break;
                }
            }
            let prefix_len = def.line_comment.as_ref().map_or(0, String::len);
            let lookahead = cap_end
                .saturating_add(prefix_len.saturating_sub(1))
                .min(end);
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
            if col > left && col - left - 1 < width {
                let idx = col - left - 1;
                let glyph = std::mem::replace(&mut cells[idx].glyph, CellGlyph::EMPTY);
                let mark = unit_char(unit);
                cells[idx].glyph = if glyph.len() + mark.len_utf8() <= CELL_GLYPH_BYTES {
                    glyph.push(mark)
                } else {
                    glyph
                };
            }
        } else if col + width_of_unit > right {
            break;
        } else {
            let (pieces, piece_count) = glyph(unit_char(unit), width_of_unit);
            for (offset, piece) in pieces.into_iter().take(piece_count).enumerate() {
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
}

#[cfg(test)]
fn line_cells(
    syntax: Option<&SyntaxDef>,
    text: &Doc,
    line: std::ops::Range<usize>,
    left: usize,
    width: usize,
    metrics: Metrics,
    marks: &[(usize, usize)],
) -> Vec<Cell> {
    let mut cells = vec![Cell::space(); width];
    line_cells_into(
        &mut cells,
        width,
        syntax,
        LexState::Normal,
        text,
        line,
        left,
        metrics,
        marks,
    );
    cells
}

/// Up to 3 cells for one character without heap allocation: control characters as `^X`,
/// C1 as `\HH`, tabs as spaces, and wide characters followed by `CellGlyph::EMPTY`.
fn glyph(c: char, w: usize) -> ([CellGlyph; 3], usize) {
    if c == '\t' {
        return (
            [CellGlyph::SPACE, CellGlyph::SPACE, CellGlyph::SPACE],
            w.min(3),
        );
    }
    if let Ok(b) = u8::try_from(c)
        && b < 0x20
    {
        return (
            [
                CellGlyph::from_ascii(b'^'),
                CellGlyph::from_ascii(b + 64),
                CellGlyph::EMPTY,
            ],
            2,
        );
    }
    let code = c as u32;
    if code == 0x7F {
        return (
            [
                CellGlyph::from_ascii(b'^'),
                CellGlyph::from_ascii(b'?'),
                CellGlyph::EMPTY,
            ],
            2,
        );
    }
    if (0x80..=0x9F).contains(&code) {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let b = code as usize;
        return (
            [
                CellGlyph::from_ascii(b'\\'),
                CellGlyph::from_ascii(HEX[b >> 4]),
                CellGlyph::from_ascii(HEX[b & 0xF]),
            ],
            3,
        );
    }
    (
        [CellGlyph::from_char(c), CellGlyph::EMPTY, CellGlyph::EMPTY],
        w.min(3),
    )
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
    let line = line_at(text, point) + 1;
    let col = display_col + 1;
    let head = format!("-{mark}-  {}{view}", buf.name());
    let eol = buf.eol().label();
    let pos = format!("{line}:{col}");
    [
        format!("{head}  {eol}  {pos}  {percent}%"),
        format!("{head}  {pos}  {percent}%"),
    ]
    .into_iter()
    .find(|s| str_width(s, m) <= width)
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
fn fit_into(grid: &mut [Cell], start: usize, width: usize, s: &str, fill: char, m: Metrics) {
    let out = &mut grid[start..start + width];
    let mut col = 0;
    for c in s.chars() {
        let w = m.char(c);
        if w == 0 {
            if col > 0 {
                let prev = std::mem::replace(&mut out[col - 1].glyph, CellGlyph::EMPTY);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::render::scroll;
    use crate::core::{Backing, Config, DocBuilder, Eol, MAX_SLICE_BYTES, Replace};
    use crate::shell::lua;
    use crate::test_support::{keys, typ};

    fn set_mark(ed: Editor) -> Editor {
        let point = ed.window().point;
        ed.with_current_buffer(|b| b.set_mark(Some(point)))
    }

    fn bootstrapped() -> Editor {
        let lua = lua::new_state().unwrap();
        lua::bootstrap(Editor::new(Config::default()), &lua)
    }

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
        assert_eq!(cells_of("e\u{301}x", 1, 3), ["x", " ", " "]);
    }

    #[test]
    fn a_region_over_wide_hangul_reverses_both_cells_of_each_glyph() {
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
        let ed = set_mark(keys(typ(bootstrapped(), "abc"), "C-a"));
        let ed = ed.with_window(|w| Window { point: 2, ..w });
        let ed = keys(ed, "C-x 2");
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
        let ed = keys(typ(bootstrapped(), "abc def"), "C-a M-%");
        let ed = keys(typ(ed, "def"), "RET");
        let ed = keys(typ(ed, "x"), "RET");
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
            let (ed, id) = ed.add_buffer("a.t", Doc::from_bytes(b"let x"), Backing::None);
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
            (Eol::Crlf, 25, format!("{:-<25}", "----  f.txt  1:1  0%")),
            (Eol::Crlf, 19, format!("{:-<19}", "----  f.txt  1:1")),
        ];
        for (eol, width, expected) in cases {
            let ed = scroll(status_editor(eol), width, 3);
            assert_eq!(frame(&ed, width, 3).row_strings()[1], expected);
        }
    }

    #[test]
    fn status_line_position_counts_lines_and_display_columns() {
        let text = Doc::from_bytes("a\n가\tb".as_bytes());
        let ed = Editor::new(Config::default()).with_current_buffer(|b| b.with_text(text));
        let buf = ed.buffer();
        let end = buf.text().total_bytes();
        assert!(status_text(buf, end, 60, Metrics { tab: 8, cjk: false }).contains("  2:10  "));
        assert!(status_text(buf, 0, 60, Metrics { tab: 8, cjk: false }).contains("  1:1  "));
        assert!(
            status_text(buf, end + 99, 60, Metrics { tab: 8, cjk: false }).contains("  2:10  ")
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
        let ed = Editor::new(Config::default()).with_current_buffer(|b| b.with_text(text));
        let s = status_text(ed.buffer(), 5, 40, Metrics { tab: 8, cjk: false });
        assert!(s.contains("45%"));
    }

    #[test]
    fn prompt_cursor_scroll_and_note_message_behavior() {
        let ed = keys(typ(Editor::new(Config::default()), ""), "C-x C-f");
        let ed = typ(ed, "가a");
        assert_eq!(frame(&ed, 40, 4).cursor, (14, 3));

        let ed = keys(Editor::new(Config::default()), "C-x C-f");
        let ed = typ(ed, &"x".repeat(30));
        let f = frame(&ed, 20, 4);
        assert_eq!(f.cursor, (19, 3));
        let echo = f.row_strings()[3].clone();
        assert!(echo.starts_with(&"x".repeat(19)), "{echo}");

        let ed = keys(Editor::new(Config::default()), "C-x C-f").set_message("No match");
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
                .apply(Replace {
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
        let mut huge = Doc::from_bytes(&vec![b'x'; 16 * 1024]);
        for _ in 0..13 {
            let dup = huge.clone();
            huge = huge
                .apply(Replace {
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
            huge.apply(Replace {
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
        let ed = Editor::new(Config::default()).with_current_buffer(|b| b.with_text(doc));
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
