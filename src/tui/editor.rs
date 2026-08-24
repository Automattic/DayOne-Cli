//! In-memory text editor backing the TUI entry edit mode.
//!
//! Text is stored as a `Vec` of logical lines, each a `Vec<char>` so cursor
//! columns are plain char indices (no byte-boundary juggling). Rendering and
//! mouse hit-testing go through [`EntryEditor::layout`], which wraps each
//! logical line to a width and yields visual rows. Wrapping is by char count,
//! so wide (CJK/emoji) glyphs can drift the cursor by a column on lines that
//! contain them — acceptable for plain journaling text.

/// A cursor/anchor position: a logical line index plus a char column within
/// that line. `col` may equal the line length (cursor past the last char).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    fn as_tuple(self) -> (usize, usize) {
        (self.line, self.col)
    }
}

impl PartialOrd for Pos {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Pos {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_tuple().cmp(&other.as_tuple())
    }
}

/// One on-screen row produced by wrapping a logical line. Covers the chars
/// `[start, start + len)` of logical line `line`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualRow {
    pub line: usize,
    pub start: usize,
    pub len: usize,
}

/// Direction for a cursor move. Distinct from the navigation `Delta` so the
/// editor can also move left/right and to document bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    DocStart,
    DocEnd,
}

/// A scratch text buffer with a cursor, optional selection anchor, and a
/// remembered desired screen column for vertical motion.
pub struct EntryEditor {
    pub journal_id: String,
    pub entry_id: String,
    lines: Vec<Vec<char>>,
    cursor: Pos,
    anchor: Option<Pos>,
    /// Desired screen column, preserved across Up/Down so the cursor keeps its
    /// horizontal position over short lines. `None` means "use current col".
    desired_col: Option<usize>,
    /// Wrap width from the last render, used by vertical movement which runs
    /// before the next draw. Updated by the UI each frame.
    pub wrap_width: usize,
    /// Visual-row scroll offset into the wrapped layout.
    pub scroll: u16,
    original: String,
}

impl EntryEditor {
    pub fn new(journal_id: String, entry_id: String, body: &str) -> Self {
        Self {
            journal_id,
            entry_id,
            lines: split_lines(body),
            cursor: Pos { line: 0, col: 0 },
            anchor: None,
            desired_col: None,
            wrap_width: 1,
            scroll: 0,
            original: body.to_string(),
        }
    }

    pub fn to_text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn is_dirty(&self) -> bool {
        self.to_text() != self.original
    }

    /// Mark the current contents as saved so `is_dirty` reads false until the
    /// next change.
    pub fn mark_saved(&mut self) {
        self.original = self.to_text();
    }

    /// The selected range as ordered `(start, end)` positions, or `None` when
    /// there is no selection (no anchor, or anchor == cursor).
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            return None;
        }
        if anchor < self.cursor {
            Some((anchor, self.cursor))
        } else {
            Some((self.cursor, anchor))
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        Some(self.text_between(start, end))
    }

    fn line_len(&self, line: usize) -> usize {
        self.lines.get(line).map(Vec::len).unwrap_or(0)
    }

    fn clamp_pos(&self, pos: Pos) -> Pos {
        let line = pos.line.min(self.lines.len().saturating_sub(1));
        let col = pos.col.min(self.line_len(line));
        Pos { line, col }
    }

    fn text_between(&self, start: Pos, end: Pos) -> String {
        if start.line == end.line {
            return self.lines[start.line][start.col..end.col].iter().collect();
        }
        let mut out = String::new();
        out.extend(self.lines[start.line][start.col..].iter());
        for line in &self.lines[start.line + 1..end.line] {
            out.push('\n');
            out.extend(line.iter());
        }
        out.push('\n');
        out.extend(self.lines[end.line][..end.col].iter());
        out
    }

    // --- Editing ---------------------------------------------------------

    /// Delete the active selection (if any) and place the cursor at its start.
    /// Returns true when something was removed.
    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            return false;
        };
        if start.line == end.line {
            self.lines[start.line].drain(start.col..end.col);
        } else {
            let tail: Vec<char> = self.lines[end.line][end.col..].to_vec();
            self.lines[start.line].truncate(start.col);
            self.lines[start.line].extend(tail);
            self.lines.drain(start.line + 1..=end.line);
        }
        self.cursor = start;
        self.anchor = None;
        true
    }

    pub fn insert_char(&mut self, ch: char) {
        if ch == '\n' {
            self.insert_newline();
            return;
        }
        self.delete_selection();
        let Pos { line, col } = self.cursor;
        self.lines[line].insert(col, ch);
        self.cursor.col = col + 1;
        self.desired_col = None;
    }

    pub fn insert_newline(&mut self) {
        self.delete_selection();
        let Pos { line, col } = self.cursor;
        let tail: Vec<char> = self.lines[line].split_off(col);
        self.lines.insert(line + 1, tail);
        self.cursor = Pos {
            line: line + 1,
            col: 0,
        };
        self.desired_col = None;
    }

    /// Insert arbitrary text (e.g. a paste), splitting on newlines.
    pub fn insert_str(&mut self, text: &str) {
        self.delete_selection();
        // Normalize CRLF/CR so pasted Windows/old-Mac text doesn't leave stray
        // carriage returns in the buffer.
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut parts = normalized.split('\n');
        if let Some(first) = parts.next() {
            let Pos { line, col } = self.cursor;
            let inserted: Vec<char> = first.chars().collect();
            let n = inserted.len();
            self.lines[line].splice(col..col, inserted);
            self.cursor.col = col + n;
        }
        for part in parts {
            self.insert_newline();
            let chars: Vec<char> = part.chars().collect();
            let n = chars.len();
            let Pos { line, col } = self.cursor;
            self.lines[line].splice(col..col, chars);
            self.cursor.col = col + n;
        }
        self.desired_col = None;
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            self.desired_col = None;
            return;
        }
        let Pos { line, col } = self.cursor;
        if col > 0 {
            self.lines[line].remove(col - 1);
            self.cursor.col = col - 1;
        } else if line > 0 {
            let cur = self.lines.remove(line);
            let prev_len = self.lines[line - 1].len();
            self.lines[line - 1].extend(cur);
            self.cursor = Pos {
                line: line - 1,
                col: prev_len,
            };
        }
        self.desired_col = None;
    }

    pub fn delete_forward(&mut self) {
        if self.delete_selection() {
            self.desired_col = None;
            return;
        }
        let Pos { line, col } = self.cursor;
        if col < self.line_len(line) {
            self.lines[line].remove(col);
        } else if line + 1 < self.lines.len() {
            let next = self.lines.remove(line + 1);
            self.lines[line].extend(next);
        }
        self.desired_col = None;
    }

    // --- Movement --------------------------------------------------------

    pub fn move_cursor(&mut self, mv: Move, extend: bool) {
        if extend {
            // Begin a selection at the current cursor if one isn't open.
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        match mv {
            Move::Left => self.move_horizontal(-1),
            Move::Right => self.move_horizontal(1),
            Move::Up => self.move_vertical(-1),
            Move::Down => self.move_vertical(1),
            Move::Home => {
                self.cursor.col = 0;
                self.desired_col = None;
            }
            Move::End => {
                self.cursor.col = self.line_len(self.cursor.line);
                self.desired_col = None;
            }
            Move::DocStart => {
                self.cursor = Pos { line: 0, col: 0 };
                self.desired_col = None;
            }
            Move::DocEnd => {
                let last = self.lines.len() - 1;
                self.cursor = Pos {
                    line: last,
                    col: self.line_len(last),
                };
                self.desired_col = None;
            }
        }
    }

    fn move_horizontal(&mut self, dir: i32) {
        self.desired_col = None;
        if dir < 0 {
            if self.cursor.col > 0 {
                self.cursor.col -= 1;
            } else if self.cursor.line > 0 {
                self.cursor.line -= 1;
                self.cursor.col = self.line_len(self.cursor.line);
            }
        } else if self.cursor.col < self.line_len(self.cursor.line) {
            self.cursor.col += 1;
        } else if self.cursor.line + 1 < self.lines.len() {
            self.cursor.line += 1;
            self.cursor.col = 0;
        }
    }

    /// Visual vertical movement: walk the wrapped layout up/down by one row,
    /// preserving the desired screen column.
    fn move_vertical(&mut self, dir: i32) {
        let width = self.wrap_width.max(1);
        let rows = self.layout(width);
        let cur_row = self.cursor_visual_row(&rows, width);
        let (_, screen_col) = self.cursor_screen(&rows, width);
        let target_col = self.desired_col.get_or_insert(screen_col);
        let target_col = *target_col;
        let next_row = if dir < 0 {
            cur_row.checked_sub(1)
        } else {
            (cur_row + 1 < rows.len()).then_some(cur_row + 1)
        };
        let Some(next_row) = next_row else {
            // At top/bottom edge: jump to line start/end like most editors.
            if dir < 0 {
                self.cursor.col = 0;
            } else {
                self.cursor.col = self.line_len(self.cursor.line);
            }
            return;
        };
        let row = rows[next_row];
        let col = (row.start + target_col.min(row.len)).min(self.line_len(row.line));
        self.cursor = Pos {
            line: row.line,
            col,
        };
    }

    // --- Mouse -----------------------------------------------------------

    /// Move the cursor to the visual row/col clicked (relative to the content
    /// area, before scroll). Clears any selection and drops a fresh anchor so a
    /// following drag can extend from here.
    pub fn click(&mut self, rel_col: usize, rel_row: usize) {
        let pos = self.pos_from_screen(rel_col, rel_row);
        self.cursor = pos;
        self.anchor = Some(pos);
        self.desired_col = None;
    }

    /// Extend the selection to the dragged-to position.
    pub fn drag(&mut self, rel_col: usize, rel_row: usize) {
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
        self.cursor = self.pos_from_screen(rel_col, rel_row);
        self.desired_col = None;
    }

    fn pos_from_screen(&self, rel_col: usize, rel_row: usize) -> Pos {
        let width = self.wrap_width.max(1);
        let rows = self.layout(width);
        if rows.is_empty() {
            return Pos { line: 0, col: 0 };
        }
        let row_idx = (self.scroll as usize + rel_row).min(rows.len() - 1);
        let row = rows[row_idx];
        let col = row.start + rel_col.min(row.len);
        self.clamp_pos(Pos {
            line: row.line,
            col,
        })
    }

    pub fn scroll_lines(&mut self, dir: i32, amount: u16) {
        if dir < 0 {
            self.scroll = self.scroll.saturating_sub(amount);
        } else {
            self.scroll = self.scroll.saturating_add(amount);
        }
    }

    // --- Layout ----------------------------------------------------------

    /// Wrap every logical line to `width` columns, yielding the visual rows in
    /// top-to-bottom order. An empty logical line yields one zero-length row.
    pub fn layout(&self, width: usize) -> Vec<VisualRow> {
        let width = width.max(1);
        let mut rows = Vec::new();
        for (line, chars) in self.lines.iter().enumerate() {
            if chars.is_empty() {
                rows.push(VisualRow {
                    line,
                    start: 0,
                    len: 0,
                });
                continue;
            }
            let mut start = 0;
            while start < chars.len() {
                let len = (chars.len() - start).min(width);
                rows.push(VisualRow { line, start, len });
                start += len;
            }
        }
        if rows.is_empty() {
            rows.push(VisualRow {
                line: 0,
                start: 0,
                len: 0,
            });
        }
        rows
    }

    /// Index of the visual row containing the cursor.
    pub fn cursor_visual_row(&self, rows: &[VisualRow], _width: usize) -> usize {
        let c = self.cursor;
        let mut last_on_line = 0;
        for (i, row) in rows.iter().enumerate() {
            if row.line != c.line {
                continue;
            }
            last_on_line = i;
            if c.col < row.start + row.len {
                return i;
            }
            // Cursor sits exactly at a wrap boundary: prefer the next row's
            // start unless this is the line's final row.
            if c.col == row.start + row.len {
                let is_line_end = c.col == self.line_len(c.line);
                let next_wraps = rows.get(i + 1).map(|r| r.line == c.line).unwrap_or(false);
                if is_line_end || !next_wraps {
                    return i;
                }
            }
        }
        last_on_line
    }

    /// Cursor position as (visual row index, screen column).
    pub fn cursor_screen(&self, rows: &[VisualRow], width: usize) -> (usize, usize) {
        let row_idx = self.cursor_visual_row(rows, width);
        let row = rows[row_idx];
        let col = self.cursor.col.saturating_sub(row.start);
        (row_idx, col)
    }

    /// Ensure the cursor's visual row is within `[scroll, scroll + height)`.
    pub fn clamp_scroll(&mut self, height: u16, width: usize) {
        if height == 0 {
            return;
        }
        let rows = self.layout(width);
        let cur = self.cursor_visual_row(&rows, width) as u16;
        if cur < self.scroll {
            self.scroll = cur;
        } else if cur >= self.scroll + height {
            self.scroll = cur + 1 - height;
        }
        let max_scroll = (rows.len() as u16).saturating_sub(1);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
    }

    /// Char slice of a logical line, for rendering.
    pub fn line_chars(&self, line: usize) -> &[char] {
        self.lines.get(line).map(Vec::as_slice).unwrap_or(&[])
    }
}

fn split_lines(body: &str) -> Vec<Vec<char>> {
    let normalized = body.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<Vec<char>> = normalized
        .split('\n')
        .map(|l| l.chars().collect())
        .collect();
    if lines.is_empty() {
        vec![Vec::new()]
    } else {
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(body: &str) -> EntryEditor {
        let mut e = EntryEditor::new("j".into(), "e".into(), body);
        e.wrap_width = 80;
        e
    }

    #[test]
    fn insert_and_backspace() {
        let mut e = ed("ab");
        e.move_cursor(Move::End, false);
        e.insert_char('c');
        assert_eq!(e.to_text(), "abc");
        e.backspace();
        assert_eq!(e.to_text(), "ab");
        assert_eq!(e.cursor, Pos { line: 0, col: 2 });
    }

    #[test]
    fn newline_splits_line() {
        let mut e = ed("hello");
        e.move_cursor(Move::Right, false);
        e.move_cursor(Move::Right, false);
        e.insert_newline();
        assert_eq!(e.to_text(), "he\nllo");
        assert_eq!(e.cursor, Pos { line: 1, col: 0 });
    }

    #[test]
    fn backspace_joins_lines() {
        let mut e = ed("ab\ncd");
        e.cursor = Pos { line: 1, col: 0 };
        e.backspace();
        assert_eq!(e.to_text(), "abcd");
        assert_eq!(e.cursor, Pos { line: 0, col: 2 });
    }

    #[test]
    fn selection_replace_on_insert() {
        let mut e = ed("hello world");
        e.cursor = Pos { line: 0, col: 0 };
        for _ in 0..5 {
            e.move_cursor(Move::Right, true);
        }
        assert_eq!(e.selected_text().as_deref(), Some("hello"));
        e.insert_str("HI");
        assert_eq!(e.to_text(), "HI world");
        assert!(e.selection().is_none());
    }

    #[test]
    fn selection_across_lines_copy() {
        let mut e = ed("ab\ncd\nef");
        e.cursor = Pos { line: 0, col: 1 };
        e.anchor = Some(Pos { line: 0, col: 1 });
        e.cursor = Pos { line: 2, col: 1 };
        assert_eq!(e.selected_text().as_deref(), Some("b\ncd\ne"));
    }

    #[test]
    fn paste_multiline() {
        let mut e = ed("xy");
        e.move_cursor(Move::Right, false);
        e.insert_str("1\n2\n3");
        assert_eq!(e.to_text(), "x1\n2\n3y");
        assert_eq!(e.cursor, Pos { line: 2, col: 1 });
    }

    #[test]
    fn delete_forward_joins() {
        let mut e = ed("ab\ncd");
        e.move_cursor(Move::End, false);
        e.delete_forward();
        assert_eq!(e.to_text(), "abcd");
    }

    #[test]
    fn wrap_layout_click_maps_to_char() {
        let mut e = EntryEditor::new("j".into(), "e".into(), "abcdef");
        e.wrap_width = 3;
        // Two visual rows: "abc" / "def". Click row 1, col 1 -> char index 4.
        e.click(1, 1);
        assert_eq!(e.cursor, Pos { line: 0, col: 4 });
    }

    #[test]
    fn vertical_move_keeps_column() {
        let mut e = ed("hello\nhi\nworld");
        e.cursor = Pos { line: 0, col: 4 };
        e.move_cursor(Move::Down, false);
        // Line "hi" is shorter; cursor clamps to end.
        assert_eq!(e.cursor, Pos { line: 1, col: 2 });
        e.move_cursor(Move::Down, false);
        // Desired column 4 restored on the longer line.
        assert_eq!(e.cursor, Pos { line: 2, col: 4 });
    }

    #[test]
    fn dirty_tracking() {
        let mut e = ed("orig");
        assert!(!e.is_dirty());
        e.insert_char('!');
        assert!(e.is_dirty());
        e.mark_saved();
        assert!(!e.is_dirty());
    }
}
