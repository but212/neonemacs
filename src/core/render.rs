//! Screen model: window rectangles, per-cell grid, and line diffs (§5, §11, A5).
//! `scroll` adjusts the view so point stays visible; `frame` is pure.

use crate::core::commands::column;
use crate::core::editor::{Buffer, Dir, Editor, Layout, Window};
use crate::core::syntax::{TokenKind, detect_lang, highlight_line};
use crate::core::text::{Text, Unit};
use crate::core::width::{char_width, unit_width};

/// A single terminal cell containing display glyph and syntax style (I-22).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub glyph: String,
    pub style: TokenKind,
}

impl Cell {
    pub const fn plain(glyph: String) -> Self {
        Self {
            glyph,
            style: TokenKind::Normal,
        }
    }

    pub fn space() -> Self {
        Self::plain(" ".to_owned())
    }
}

/// A screen as cell rows of exactly the terminal width, and the cursor as `(x, y)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub rows: Vec<Vec<Cell>>,
    pub cursor: (usize, usize),
}

impl Frame {
    pub fn row_strings(&self) -> Vec<String> {
        self.rows
            .iter()
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
        Layout::Split {
            dir: Dir::Below,
            first,
            second,
        } => {
            let top = rect.h.div_ceil(2);
            let mut out = rects(first, Rect { h: top, ..rect });
            out.extend(rects(
                second,
                Rect {
                    y: rect.y + top,
                    h: rect.h - top,
                    ..rect
                },
            ));
            out
        }
        Layout::Split {
            dir: Dir::Right,
            first,
            second,
        } => {
            let left = rect.w.div_ceil(2);
            let mut out = rects(first, Rect { w: left, ..rect });
            out.extend(rects(
                second,
                Rect {
                    x: rect.x + left,
                    w: rect.w - left,
                    ..rect
                },
            ));
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

/// Moves each window's `top` and `left_col` so that its point is visible.
pub fn scroll(ed: Editor, cols: usize, rows: usize) -> Editor {
    let tab = usize::from(ed.config().tab_width);
    let windows: Vec<Window> = ed.windows().windows().into_iter().cloned().collect();
    let scrolled: Vec<Window> = windows
        .into_iter()
        .zip(areas(&ed, cols, rows))
        .map(|(w, area)| {
            let text = &ed.buffer_by_id(w.buffer).text;
            scroll_window(text, w, area, tab)
        })
        .collect();
    let mut win = ed.windows().clone();
    for (i, w) in scrolled.into_iter().enumerate() {
        win = win.set_leaf(i, w);
    }
    ed.set_windows(win)
}

fn scroll_window(text: &Text, mut w: Window, area: Rect, tab: usize) -> Window {
    let height = area.h.saturating_sub(1).max(1);
    let width = area.w.max(1);
    let p = w.point.min(text.len());
    if p < w.top {
        w.top = text.line_start(p);
    }
    // Count whole lines from `top` to the line holding point.
    let mut index = 0;
    let mut s = w.top;
    while p > text.line_end(s) {
        index += 1;
        s = text.line_end(s) + 1;
    }
    if index >= height {
        w.top = text.line_start(p);
    }
    let col = column(text, p, tab);
    if col < w.left_col {
        w.left_col = col;
    } else if col >= w.left_col + width {
        w.left_col = col + 1 - width;
    }
    w
}

/// Builds the screen. Pure: call `scroll` first to bring point into view.
pub fn frame(ed: &Editor, cols: usize, rows: usize) -> Frame {
    let tab = usize::from(ed.config().tab_width);
    let mut grid = vec![vec![Cell::space(); cols]; rows];
    let leaves = ed.windows().windows();
    let mut cursor = (0, 0);

    for (i, (w, area)) in leaves.into_iter().zip(areas(ed, cols, rows)).enumerate() {
        let buf = ed.buffer_by_id(w.buffer);
        let lang = detect_lang(&buf.name);
        let (body, (row, col)) = window_cells(&buf.text, w, area, tab, lang);
        for (r, cells) in body.into_iter().enumerate() {
            grid = put(grid, area.y + r, area.x, cells);
        }
        let status = fit(&status_text(buf, w.point), area.w, '-');
        let status_cells: Vec<Cell> = status.into_iter().map(Cell::plain).collect();
        grid = put(
            grid,
            area.y + area.h.saturating_sub(1),
            area.x,
            status_cells,
        );
        if i == ed.windows().selected() {
            cursor = (area.x + col, area.y + row);
        }
    }

    if rows > 0 {
        let echo = fit(ed.message(), cols, ' ');
        let echo_cells: Vec<Cell> = echo.into_iter().map(Cell::plain).collect();
        grid = put(grid, rows - 1, 0, echo_cells);
    }

    Frame { rows: grid, cursor }
}

fn put<T: Clone>(mut grid: Vec<Vec<T>>, row: usize, col: usize, items: Vec<T>) -> Vec<Vec<T>> {
    if let Some(line) = grid.get_mut(row) {
        for (i, item) in items.into_iter().enumerate() {
            if let Some(slot) = line.get_mut(col + i) {
                *slot = item;
            }
        }
    }
    grid
}

/// Cells of the text area of one window, and the cursor as `(row, col)` inside it.
fn window_cells(
    text: &Text,
    w: &Window,
    area: Rect,
    tab: usize,
    lang: Option<&str>,
) -> (Vec<Vec<Cell>>, (usize, usize)) {
    let height = area.h.saturating_sub(1);
    let p = w.point.min(text.len());
    let mut cursor = (0, 0);
    let mut body = Vec::with_capacity(height);
    let mut s = w.top;
    for r in 0..height {
        if s > text.len() {
            body.push(vec![Cell::space(); area.w]);
            continue;
        }
        let e = text.line_end(s);
        if s <= p && p <= e {
            let col = column(text, p, tab).saturating_sub(w.left_col);
            cursor = (r, col);
        }
        body.push(line_cells(text, s, e, w.left_col, area.w, tab, lang));
        s = e + 1;
    }
    (body, cursor)
}

/// Cells of one line from `left` across `width` columns. A line cut at the
/// right edge shows `$` in the last column (A5).
fn line_cells(
    text: &Text,
    start: usize,
    end: usize,
    left: usize,
    width: usize,
    tab: usize,
    lang: Option<&str>,
) -> Vec<Cell> {
    let mut cells = vec![Cell::space(); width];
    let right = left + width;
    let mut col = 0;
    let mut p = start;
    let mut cut = false;

    let line_bytes = text.bytes(start..end);
    let line_str = String::from_utf8_lossy(&line_bytes);
    let token_kinds = highlight_line(lang, &line_str);
    let mut char_idx = 0;

    while p < end {
        let Some((unit, n)) = text.unit_at(p) else {
            break;
        };
        let style = token_kinds
            .get(char_idx)
            .copied()
            .unwrap_or(TokenKind::Normal);
        char_idx += 1;

        if col >= right {
            cut = true;
            break;
        }
        let w = unit_width(unit, col, tab);
        if w == 0 {
            // Combining mark: attach to the cell before it.
            if col > left && col - left - 1 < width {
                cells[col - left - 1].glyph.push(unit_char(unit));
            }
        } else {
            for (k, g) in glyph(unit_char(unit), w).into_iter().enumerate() {
                let c = col + k;
                if c >= left && c < right {
                    cells[c - left] = Cell { glyph: g, style };
                }
            }
        }
        col += w;
        p += n;
    }
    if cut && width > 0 {
        cells[width - 1] = Cell::plain("$".to_owned());
    }
    cells
}

fn unit_char(unit: Unit) -> char {
    match unit {
        Unit::Char(c) => c,
        Unit::Raw(b) => char::from(b),
    }
}

/// `w` cells for one character: control characters as `^X`, C1 as `\HH`,
/// tabs as spaces, and wide characters followed by an empty cell.
fn glyph(c: char, w: usize) -> Vec<String> {
    let code = c as u32;
    if c == '\t' {
        return vec![" ".to_owned(); w];
    }
    if code < 0x20 {
        return vec!["^".to_owned(), char::from(code as u8 + 64).to_string()];
    }
    if code == 0x7F {
        return vec!["^".to_owned(), "?".to_owned()];
    }
    if (0x80..=0x9F).contains(&code) {
        return format!("\\{code:02X}").chars().map(String::from).collect();
    }
    let mut cells = vec![c.to_string()];
    cells.extend(std::iter::repeat_n(String::new(), w.saturating_sub(1)));
    cells
}

fn status_text(buf: &Buffer, point: usize) -> String {
    let mark = if buf.modified { "**" } else { "--" };
    let view = if buf.view_only { " (view)" } else { "" };
    let percent = (point * 100).checked_div(buf.text.len()).unwrap_or(100);
    format!("-{mark}-  {}{view}  {percent}%", buf.name)
}

/// Fits `s` into `width` cells, padding with `fill`. Wide characters are not split.
fn fit(s: &str, width: usize, fill: char) -> Vec<String> {
    let mut cells: Vec<String> = Vec::with_capacity(width);
    for c in s.chars() {
        let w = char_width(c);
        if w == 0 {
            if let Some(last) = cells.last_mut() {
                last.push(c);
            }
            continue;
        }
        if cells.len() + w > width {
            break;
        }
        cells.push(c.to_string());
        cells.extend(std::iter::repeat_n(String::new(), w - 1));
    }
    while cells.len() < width {
        cells.push(fill.to_string());
    }
    cells
}

/// Rows that changed, with their new content. An empty `prev` redraws everything.
pub fn diff(prev: &[Vec<Cell>], next: &[Vec<Cell>]) -> Vec<(usize, Vec<Cell>)> {
    next.iter()
        .enumerate()
        .filter(|(i, row)| prev.get(*i) != Some(row))
        .map(|(i, row)| (i, (*row).clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;

    fn screen(text: &str, point: usize, cols: usize, rows: usize) -> Frame {
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| Buffer {
                text: Text::from_bytes(text.as_bytes()),
                ..b
            })
            .with_window(|w| Window { point, ..w });
        let ed = scroll(ed, cols, rows);
        frame(&ed, cols, rows)
    }

    fn screen_rows(text: &str, point: usize, cols: usize, rows: usize) -> Vec<String> {
        screen(text, point, cols, rows).row_strings()
    }

    #[test]
    fn frame_hangul_and_tab() {
        let r = screen_rows("가나\na\tb", 0, 10, 4);
        assert_eq!(r[0], format!("가나{}", " ".repeat(6)));
        assert_eq!(r[1], "a       b ");
    }

    #[test]
    fn frame_long_line_is_cut_with_dollar() {
        let r = screen_rows("abcdefghijklmnop", 0, 10, 3);
        assert_eq!(r[0], "abcdefghi$");
    }

    #[test]
    fn frame_control_and_c1_glyphs() {
        let ed = Editor::new(Config::default()).with_current_buffer(|b| Buffer {
            text: Text::from_bytes(&[0x01, b'x', 0x85]),
            ..b
        });
        let ed = scroll(ed, 10, 3);
        let f = frame(&ed, 10, 3);
        assert_eq!(f.row_strings()[0], "^Ax\\85    ");
    }

    #[test]
    fn frame_status_line_shows_name_state_and_percent() {
        let ed = Editor::new(Config::default()).with_current_buffer(|b| Buffer {
            text: Text::from_bytes(b"abc"),
            modified: true,
            view_only: true,
            name: "f.txt".to_owned(),
            ..b
        });
        let ed = scroll(ed, 30, 3);
        let f = frame(&ed, 30, 3);
        let r = f.row_strings();
        assert!(r[1].starts_with("-**-  f.txt (view)  0%"));
        assert!(r[1].ends_with('-'));
    }

    #[test]
    fn frame_cursor_and_scroll_follow_point() {
        let f = screen("가b", 3, 10, 4);
        assert_eq!(f.cursor, (2, 0));

        let r = screen_rows("1\n2\n3\n4\n5", 8, 10, 4);
        assert_eq!(r[0], "5         ");
    }

    #[test]
    fn diff_only_changed_rows() {
        let prev = vec![vec![Cell::plain("a".into())], vec![Cell::plain("b".into())]];
        let next = vec![vec![Cell::plain("a".into())], vec![Cell::plain("c".into())]];
        assert_eq!(diff(&prev, &next), vec![(1, vec![Cell::plain("c".into())])]);
    }

    #[test]
    fn diff_full_redraw_when_no_previous_frame() {
        let next = vec![vec![Cell::plain("x".into())], vec![Cell::plain("y".into())]];
        assert_eq!(diff(&[], &next).len(), 2);
    }
}
