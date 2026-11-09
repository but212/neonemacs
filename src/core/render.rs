//! Screen model: window rectangles, per-cell grid, and line diffs (§5, §11, A5).
//! `scroll` adjusts the view so point stays visible; `frame` is pure.

use crate::core::editor::{Buffer, Dir, Editor, Layout, Mode, Prefix, Window};
use crate::core::syntax::{SyntaxDef, TokenKind, highlight_line};
use crate::core::text::{Text, Unit};
use crate::core::width::{Metrics, column, unit_width};

/// A single terminal cell containing display glyph and syntax style (I-22).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub glyph: String,
    pub style: TokenKind,
    /// Drawn in reverse video: the active region or the current search match (I-47).
    pub reverse: bool,
}

impl Cell {
    #[must_use]
    pub const fn plain(glyph: String) -> Self {
        Self {
            glyph,
            style: TokenKind::Normal,
            reverse: false,
        }
    }

    #[must_use]
    pub fn space() -> Self {
        Self::plain(" ".to_owned())
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

/// A screen as cell rows of exactly the terminal width, and the cursor as `(x, y)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub rows: Vec<Vec<Cell>>,
    pub cursor: (usize, usize),
}

impl Frame {
    #[must_use]
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
#[must_use]
pub fn scroll(ed: Editor, cols: usize, rows: usize) -> Editor {
    let m = ed.config().metrics();
    let scrolled: Vec<Window> = ed
        .windows()
        .windows()
        .into_iter()
        .copied()
        .zip(areas(&ed, cols, rows))
        .map(|(w, area)| scroll_window(ed.buffer_by_id(w.buffer).text(), w, area, m))
        .collect();
    ed.with_windows(|mut win| {
        for (i, w) in scrolled.into_iter().enumerate() {
            win = win.set_leaf(i, w);
        }
        win
    })
}

fn scroll_window(text: &Text, mut w: Window, area: Rect, m: Metrics) -> Window {
    let height = area.h.saturating_sub(1).max(1);
    w.height = height;
    let width = area.w.max(1);
    let p = w.point.min(text.len());
    if p < w.top {
        w.top = text.line_start(p);
    }
    // Count whole lines from `top` to the line holding point.
    let index = text.count_lines(w.top, p);
    if index >= height {
        w.top = text.line_start(p);
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
    let mut grid = vec![vec![Cell::space(); cols]; rows];
    let leaves = ed.windows().windows();
    let mut cursor = (0, 0);
    let reversed = reversed_ranges(ed);

    for (i, (w, area)) in leaves.into_iter().zip(areas(ed, cols, rows)).enumerate() {
        let buf = ed.buffer_by_id(w.buffer);
        let syntax = ed
            .config()
            .syntax_highlight
            .then(|| ed.syntax_registry().find_by_filename(buf.name()))
            .flatten();
        let marks = if i == ed.windows().selected() {
            reversed.as_slice()
        } else {
            &[]
        };
        let (body, (row, col)) = window_cells(syntax, buf.text(), w, area, m, marks);
        for (r, cells) in body.into_iter().enumerate() {
            grid = put(grid, area.y + r, area.x, cells);
        }
        let status = fit(&status_text(buf, w.point, area.w, m), area.w, '-', m);
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
        // While a prompt is shown, the cursor sits in the echo area and the tail of a long line stays visible.
        let message = match ed.mode() {
            Mode::Prompt(p) if ed.message() == p.echo() => {
                let want = (p.cursor_width(m) + 1).saturating_sub(cols);
                let (rest, skipped) = skip_cells(ed.message(), want, m);
                let x = p.cursor_width(m).saturating_sub(skipped);
                cursor = (x.min(cols.saturating_sub(1)), rows - 1);
                rest
            }
            // A pending prefix key shows until the sequence ends.
            Mode::Normal {
                prefix: Prefix::CtlX,
            } if ed.config().echo_prefix => "C-x-",
            Mode::Normal {
                prefix: Prefix::Meta,
            } if ed.config().echo_prefix => "ESC-",
            _ => ed.message(),
        };
        let echo = fit(message, cols, ' ', m);
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
    syntax: Option<&SyntaxDef>,
    text: &Text,
    w: &Window,
    area: Rect,
    m: Metrics,
    marks: &[(usize, usize)],
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
            let col = column(text, p, m).saturating_sub(w.left_col);
            cursor = (r, col);
        }
        body.push(line_cells(syntax, text, s..e, w.left_col, area.w, m, marks));
        s = e + 1;
    }
    (body, cursor)
}

/// Cells of one line from `left` across `width` columns. A line cut at the
/// right edge shows `$` in the last column (A5).
fn line_cells(
    syntax: Option<&SyntaxDef>,
    text: &Text,
    line: std::ops::Range<usize>,
    left: usize,
    width: usize,
    m: Metrics,
    marks: &[(usize, usize)],
) -> Vec<Cell> {
    let (start, end) = (line.start, line.end);
    let mut cells = vec![Cell::space(); width];
    let right = left + width;
    let mut col = 0;
    let mut p = start;
    let mut cut = false;

    let line_bytes = text.bytes(start..end);
    let token_kinds = highlight_line(syntax, &line_bytes);

    while p < end {
        let Some((unit, n)) = text.unit_at(p) else {
            break;
        };
        let style = token_kinds
            .get(p - start)
            .copied()
            .unwrap_or(TokenKind::Normal);
        let reverse = marks.iter().any(|&(a, b)| a <= p && p < b);

        if col >= right {
            cut = true;
            break;
        }
        let w = unit_width(unit, col, m);
        if w == 0 {
            // Combining mark: attach to the cell before it.
            if col > left && col - left - 1 < width {
                cells[col - left - 1].glyph.push(unit_char(unit));
            }
        } else {
            for (k, g) in glyph(unit_char(unit), w).into_iter().enumerate() {
                let c = col + k;
                if c >= left && c < right {
                    cells[c - left] = Cell {
                        glyph: g,
                        style,
                        reverse,
                    };
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
    if c == '\t' {
        return vec![" ".to_owned(); w];
    }
    if let Ok(b) = u8::try_from(c)
        && b < 0x20
    {
        return vec!["^".to_owned(), char::from(b + 64).to_string()];
    }
    let code = c as u32;
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

/// The status line for a window `width` cells wide. Line and column are 1-based; the column is the display column.
/// What does not fit is dropped in this order: the EOL mode, then the percentage.
fn status_text(buf: &Buffer, point: usize, width: usize, m: Metrics) -> String {
    let mark = if buf.modified() { "**" } else { "--" };
    let view = if buf.view_only() { " (view)" } else { "" };
    let percent = (point as u64 * 100)
        .checked_div(buf.text().len() as u64)
        .unwrap_or(100);
    let text = buf.text();
    let p = point.min(text.len());
    let line = text.count_lines(0, p) + 1;
    let col = column(text, p, m) + 1;
    let head = format!("-{mark}-  {}{view}", buf.name());
    let eol = buf.eol().label();
    let pos = format!("{line}:{col}");
    [
        format!("{head}  {eol}  {pos}  {percent}%"),
        format!("{head}  {pos}  {percent}%"),
    ]
    .into_iter()
    .find(|s| text_width(s, m) <= width)
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

fn text_width(s: &str, m: Metrics) -> usize {
    s.chars().map(|c| m.char(c)).sum()
}

/// Fits `s` into `width` cells, padding with `fill`. Wide characters are not split.
fn fit(s: &str, width: usize, fill: char, m: Metrics) -> Vec<String> {
    let mut cells: Vec<String> = Vec::with_capacity(width);
    for c in s.chars() {
        let w = m.char(c);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;
    use crate::core::eol::Eol;

    fn screen(text: &str, point: usize, cols: usize, rows: usize) -> Frame {
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(Text::from_bytes(text.as_bytes())))
            .with_window(|w| Window { point, ..w });
        let ed = scroll(ed, cols, rows);
        frame(&ed, cols, rows)
    }

    fn screen_rows(text: &str, point: usize, cols: usize, rows: usize) -> Vec<String> {
        screen(text, point, cols, rows).row_strings()
    }

    use crate::test_support::{keys, set_mark, typ};

    fn reversed(frame: &Frame, row: usize) -> Vec<bool> {
        frame.rows[row].iter().map(|c| c.reverse).collect()
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
        assert!(f.rows.iter().flatten().all(|c| !c.reverse));
    }

    fn cells_of(s: &str, left: usize, width: usize) -> Vec<String> {
        let text = Text::from_bytes(s.as_bytes());
        line_cells(None, &text, 0..text.len(), left, width, Metrics { tab: 8, cjk: false }, &[])
            .into_iter()
            .map(|c| c.glyph)
            .collect()
    }

    #[test]
    fn a_combining_mark_joins_the_cell_before_it() {
        assert_eq!(cells_of("e\u{301}x", 0, 4), ["e\u{301}", "x", " ", " "]);
    }

    #[test]
    fn a_combining_mark_at_the_left_edge_is_dropped_not_misplaced() {
        // The base "e" is scrolled off; its mark sits at column 1 == left and has no cell to join.
        assert_eq!(cells_of("e\u{301}x", 1, 3), ["x", " ", " "]);
    }

    #[test]
    fn a_region_over_wide_hangul_reverses_both_cells_of_each_glyph() {
        // "가나다": 나 spans bytes 3..6 and cells 2..4.
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| {
                b.with_text(Text::from_bytes("가나다".as_bytes()))
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
            .filter(|&r| f.rows[r].iter().any(|c| c.reverse))
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
        use crate::core::commands::start_query_replace;
        use crate::core::editor::MatchKind;
        let ed = keys(typ(Editor::new(Config::default()), "abc def"), "C-a");
        let (ed, res) = start_query_replace(ed, "def", "x", MatchKind::Literal);
        res.unwrap();
        let want: Vec<bool> = (0..10).map(|i| (4..7).contains(&i)).collect();
        assert_eq!(reversed(&drawn(ed, 10, 4), 0), want);
    }

    #[test]
    fn echo_prefix_off_keeps_the_message_visible() {
        let ed = Editor::new(Config {
            echo_prefix: false,
            ..Config::default()
        })
        .set_message("Wrote f");
        let echo = |ed: Editor| drawn(ed, 12, 4).row_strings()[3].trim_end().to_owned();
        assert_eq!(echo(keys(ed.clone(), "C-x")), "Wrote f");
        assert_eq!(echo(keys(ed, "ESC")), "Wrote f");
    }

    #[test]
    fn syntax_highlight_off_draws_every_cell_plain() {
        let styled = |highlight: bool| {
            let ed = Editor::new(Config {
                syntax_highlight: highlight,
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
            let (ed, id) = ed.add_buffer(
                "a.t",
                Text::from_bytes(b"let x"),
                crate::core::file::Backing::None,
            );
            let ed = ed.with_window(|w| Window {
                buffer: id,
                point: 0,
                ..w
            });
            drawn(ed, 10, 4).rows[0]
                .iter()
                .any(|c| c.style != TokenKind::Normal)
        };
        assert!(styled(true));
        assert!(!styled(false));
    }

    #[test]
    fn a_pending_prefix_key_shows_in_the_echo_area() {
        let ed = Editor::new(Config::default()).set_message("Wrote f");
        let echo = |ed: Editor| drawn(ed, 12, 4).row_strings()[3].trim_end().to_owned();
        assert_eq!(echo(ed.clone()), "Wrote f");
        assert_eq!(echo(keys(ed.clone(), "C-x")), "C-x-");
        assert_eq!(echo(keys(ed.clone(), "ESC")), "ESC-");
        // The message returns once the sequence ends.
        assert_eq!(echo(keys(ed, "C-x C-g")), "Quit");
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
    fn frame_long_line_is_cut_with_dollar() {
        let r = screen_rows("abcdefghijklmnop", 0, 10, 3);
        assert_eq!(r[0], "abcdefghi$");
    }

    #[test]
    fn frame_control_and_c1_glyphs() {
        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(Text::from_bytes(&[0x01, b'x', 0x85])));
        let ed = scroll(ed, 10, 3);
        let f = frame(&ed, 10, 3);
        assert_eq!(f.row_strings()[0], "^Ax\\85    ");
    }

    #[test]
    fn frame_status_line_shows_name_state_and_percent() {
        let ed = Editor::new(Config::default()).with_current_buffer(|b| {
            b.with_text(Text::from_bytes(b"abc"))
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
            b.with_text(Text::from_bytes(b"abc"))
                .set_eol(eol)
                .set_name("f.txt")
        })
    }

    #[test]
    fn status_line_shows_eol_mode() {
        let ed = scroll(status_editor(Eol::Crlf), 26, 3);
        assert_eq!(
            frame(&ed, 26, 3).row_strings()[1],
            "----  f.txt  CRLF  1:1  0%"
        );

        let ed = scroll(status_editor(Eol::Lf), 24, 3);
        assert_eq!(
            frame(&ed, 24, 3).row_strings()[1],
            "----  f.txt  LF  1:1  0%"
        );
    }

    #[test]
    fn status_line_drops_eol_before_percent_when_narrow() {
        // Full text is 26 cells. At 25 the EOL goes and the position and percent stay.
        let ed = scroll(status_editor(Eol::Crlf), 25, 3);
        let r = frame(&ed, 25, 3).row_strings();
        assert_eq!(r[1], format!("{:-<25}", "----  f.txt  1:1  0%"));
    }

    #[test]
    fn status_line_drops_percent_before_position_when_narrower() {
        // Without EOL and percent the text is 16 cells.
        let ed = scroll(status_editor(Eol::Crlf), 19, 3);
        let r = frame(&ed, 19, 3).row_strings();
        assert_eq!(r[1], format!("{:-<19}", "----  f.txt  1:1"));
    }

    #[test]
    fn status_line_position_counts_lines_and_display_columns() {
        // Line 2 is "가", a tab to column 8, and "b": point at the end sits in display column 9.
        let text = Text::from_bytes(
            "a
가	b"
                .as_bytes(),
        );
        let buf = Buffer::new(
            crate::core::buffer::BufferId(1),
            "t",
            text,
            crate::core::file::Backing::None,
        );
        let end = buf.text().len();
        assert!(status_text(&buf, end, 60, Metrics { tab: 8, cjk: false }).contains("  2:10  "));
        assert!(status_text(&buf, 0, 60, Metrics { tab: 8, cjk: false }).contains("  1:1  "));
        // Point past the end is clamped.
        assert!(status_text(&buf, end + 99, 60, Metrics { tab: 8, cjk: false }).contains("  2:10  "));
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
        let text = Text::from_bytes(b"hello world");
        let buf = Buffer::new(
            crate::core::buffer::BufferId(1),
            "test.txt",
            text,
            crate::core::file::Backing::None,
        );
        let s = status_text(&buf, 5, 40, Metrics { tab: 8, cjk: false });
        // 5 * 100 / 11 = 45%
        assert!(s.contains("45%"));
    }

    fn prompt_editor(before: &str, after: &str) -> Editor {
        use crate::core::editor::{Prompt, Purpose};
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
    fn prompt_cursor_follows_input_cursor_in_display_columns() {
        let ed = prompt_editor("가a", "bc");
        // "Find file: " is 11 columns; "가" is 2 and "a" is 1.
        assert_eq!(frame(&ed, 40, 4).cursor, (14, 3));
    }

    #[test]
    fn prompt_long_input_scrolls_to_keep_cursor_visible() {
        let ed = prompt_editor(&"x".repeat(30), "");
        let f = frame(&ed, 20, 4);
        assert_eq!(f.cursor, (19, 3));
        let echo = f.row_strings()[3].clone();
        assert!(echo.starts_with(&"x".repeat(19)), "{echo}");
    }

    #[test]
    fn prompt_note_message_leaves_cursor_in_text_area() {
        let ed = prompt_editor("is", "").set_message("No match");
        assert_eq!(frame(&ed, 40, 4).cursor, (0, 0));
    }
}
