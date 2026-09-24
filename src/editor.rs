//! Small Unicode-safe editor shared by comment/review text boxes.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    style::Style,
    text::{Line, Span},
};
use std::{cell::Cell, ops::Range};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, Default)]
pub struct Editor {
    pub chars: Vec<char>,
    pub cursor: usize,
    pub anchor: Option<usize>,
    preferred_column: Option<usize>,
    width: Cell<usize>,
    folds: Vec<Fold>,
    syntax: bool,
    undo: Vec<EditState>,
}
#[derive(Clone, Debug, PartialEq)]
struct Fold {
    range: Range<usize>,
    text: String,
    attachment: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct EditState {
    chars: Vec<char>,
    cursor: usize,
    anchor: Option<usize>,
    folds: Vec<Fold>,
    syntax: bool,
}

impl From<String> for Editor {
    fn from(text: String) -> Self {
        Self::from(text.as_str())
    }
}
impl From<&str> for Editor {
    fn from(text: &str) -> Self {
        let mut editor = Self::default();
        editor.insert_raw(text);
        editor
    }
}
impl Editor {
    fn state(&self) -> EditState {
        EditState {
            chars: self.chars.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
            folds: self.folds.clone(),
            syntax: self.syntax,
        }
    }
    fn edit<T>(&mut self, action: impl FnOnce(&mut Self) -> T) -> T {
        let before = self.state();
        let result = action(self);
        if before.chars != self.chars || before.folds != self.folds {
            self.undo.push(before);
        }
        result
    }
    pub fn undo(&mut self) {
        if let Some(state) = self.undo.pop() {
            self.chars = state.chars;
            self.cursor = state.cursor;
            self.anchor = state.anchor;
            self.folds = state.folds;
            self.syntax = state.syntax;
            self.preferred_column = None;
        }
    }
    pub fn clear(&mut self) {
        self.edit(|editor| editor.replace(0..editor.chars.len(), ""));
    }
    pub fn insert(&mut self, text: &str) {
        self.edit(|editor| editor.insert_raw(text));
    }
    pub fn paste(&mut self, text: &str) {
        self.edit(|editor| editor.paste_raw(text));
    }
    /// Insert an atomic attachment label without treating typed lookalikes as media.
    pub fn insert_attachment(&mut self, label: &str) {
        self.edit(|editor| {
            editor.insert_raw(label);
            editor.folds.push(Fold {
                range: editor.cursor.saturating_sub(label.chars().count())..editor.cursor,
                text: label.into(),
                attachment: true,
            });
        });
    }
    pub fn attachment_tokens(&self) -> Vec<&str> {
        let mut tokens = self
            .folds
            .iter()
            .filter(|fold| fold.attachment)
            .collect::<Vec<_>>();
        tokens.sort_by_key(|fold| fold.range.start);
        tokens.into_iter().map(|fold| fold.text.as_str()).collect()
    }
    pub fn expand_paste(&mut self) -> bool {
        self.edit(Self::expand_paste_raw)
    }
    pub fn complete(&mut self, login: &str) {
        self.edit(|editor| editor.complete_raw(login));
    }
    pub fn key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('z') && key.modifiers == KeyModifiers::SUPER {
            self.undo();
        } else {
            self.edit(|editor| editor.key_raw(key));
        }
    }
    fn word_left(&self) -> usize {
        let mut cursor = self.cursor;
        while cursor > 0 && self.chars.get(cursor - 1).is_some_and(|c| !is_word(*c)) {
            cursor -= 1;
        }
        while cursor > 0 && self.chars.get(cursor - 1).is_some_and(|c| is_word(*c)) {
            cursor -= 1;
        }
        cursor
    }
    fn word_right(&self) -> usize {
        let mut cursor = self.cursor;
        while self.chars.get(cursor).is_some_and(|c| !is_word(*c)) {
            cursor += 1;
        }
        while self.chars.get(cursor).is_some_and(|c| is_word(*c)) {
            cursor += 1;
        }
        cursor
    }

    pub fn text(&self) -> String {
        self.expanded_text(0..self.chars.len())
    }
    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?.min(self.chars.len());
        let cursor = self.cursor.min(self.chars.len());
        (anchor != cursor).then_some(anchor.min(cursor)..anchor.max(cursor))
    }
    pub fn selected_text(&self) -> Option<String> {
        Some(self.expanded_text(self.selection()?))
    }
    fn expanded_text(&self, range: Range<usize>) -> String {
        let mut output = String::new();
        let mut cursor = range.start;
        while cursor < range.end {
            if let Some(fold) = self.folds.iter().find(|f| f.range.contains(&cursor)) {
                output.push_str(&fold.text);
                cursor = fold.range.end;
            } else {
                if let Some(c) = self.chars.get(cursor) {
                    output.push(*c);
                }
                cursor += 1;
            }
        }
        output
    }
    fn replace(&mut self, mut range: Range<usize>, text: &str) {
        // Folded content is an atomic token: deleting part of its label deletes the token.
        for fold in &self.folds {
            if range.start < fold.range.end && range.end > fold.range.start {
                range.start = range.start.min(fold.range.start);
                range.end = range.end.max(fold.range.end);
            }
        }
        let chars = text
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect::<Vec<_>>();
        let delta = chars.len() as isize - range.len() as isize;
        self.folds.retain_mut(|fold| {
            if range.start < fold.range.end && range.end > fold.range.start {
                return false;
            }
            if fold.range.start >= range.end {
                fold.range = fold.range.start.saturating_add_signed(delta)
                    ..fold.range.end.saturating_add_signed(delta);
            }
            true
        });
        self.cursor = range.start + chars.len();
        self.chars.splice(range, chars);
        self.anchor = None;
        self.preferred_column = None;
    }
    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            self.anchor = None;
            return false;
        };
        self.replace(range, "");
        true
    }
    fn insert_raw(&mut self, text: &str) {
        self.cursor = self.cursor.min(self.chars.len());
        if self.selection().is_none()
            && self
                .folds
                .iter()
                .any(|f| f.range.start < self.cursor && self.cursor < f.range.end)
        {
            if let Some(fold) = self
                .folds
                .iter()
                .find(|f| f.attachment && f.range.contains(&self.cursor))
            {
                self.cursor = fold.range.end;
            } else {
                self.expand_paste_raw();
            }
        }
        let range = self.selection().unwrap_or(self.cursor..self.cursor);
        self.replace(range, text);
    }
    fn paste_raw(&mut self, text: &str) {
        if self
            .folds
            .iter()
            .any(|f| f.range.start <= self.cursor && self.cursor <= f.range.end && f.text == text)
            && self.expand_paste_raw()
        {
            return;
        }

        self.syntax |= text.contains("```")
            || text.lines().any(|line| {
                let line = line.trim_start();
                [
                    "const ",
                    "let ",
                    "fn ",
                    "pub ",
                    "def ",
                    "import ",
                    "function ",
                    "export ",
                    "class ",
                    "async ",
                    "SELECT ",
                    "{",
                ]
                .iter()
                .any(|prefix| line.starts_with(prefix))
            });
        if text.chars().count() < 500 {
            self.insert_raw(text);
            return;
        }
        let label = format!("[Pasted content · {} chars]", text.chars().count());
        self.insert_raw(&label);
        self.folds.push(Fold {
            range: self.cursor.saturating_sub(label.chars().count())..self.cursor,
            text: text.into(),
            attachment: false,
        });
    }
    fn expand_paste_raw(&mut self) -> bool {
        let Some(index) = self.folds.iter().position(|f| {
            !f.attachment && f.range.start <= self.cursor && self.cursor <= f.range.end
        }) else {
            return false;
        };
        let fold = self.folds.remove(index);
        self.replace(fold.range, &fold.text);
        true
    }
    fn key_raw(&mut self, mut key: KeyEvent) {
        // Ghostty's macOS bindings send these control keys for Command gestures.
        // Filter-specific Ctrl+U handling happens before reaching the editor.
        if key.modifiers == KeyModifiers::CONTROL {
            let translated = match key.code {
                KeyCode::Char('a') => Some(KeyCode::Left),
                KeyCode::Char('e') => Some(KeyCode::Right),
                KeyCode::Char('u') => Some(KeyCode::Backspace),
                _ => None,
            };
            if let Some(code) = translated {
                key.code = code;
                key.modifiers = KeyModifiers::SUPER;
            }
        }
        // Ghostty translates Option+Left/Right to ESC b/f (Alt+b/f).
        // Keep this translation inside inputs so other panes retain their shortcuts.
        if key.modifiers == KeyModifiers::ALT {
            key.code = match key.code {
                KeyCode::Char('b') => KeyCode::Left,
                KeyCode::Char('f') => KeyCode::Right,
                code => code,
            };
        }
        self.cursor = self.cursor.min(self.chars.len());
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let command = key.modifiers.contains(KeyModifiers::SUPER);
        if key.code == KeyCode::Char('a') && command {
            self.anchor = Some(0);
            self.cursor = self.chars.len();
            return;
        }
        let start = self
            .chars
            .iter()
            .take(self.cursor)
            .rposition(|c| *c == '\n')
            .map_or(0, |n| n + 1);
        let end = self
            .chars
            .iter()
            .enumerate()
            .skip(self.cursor)
            .find_map(|(n, c)| (*c == '\n').then_some(n))
            .unwrap_or(self.chars.len());
        let movement = matches!(
            key.code,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
        );
        if movement {
            let old = self.cursor;
            if !shift
                && !command
                && let Some(range) = self.selection()
            {
                match key.code {
                    KeyCode::Left => {
                        self.cursor = range.start;
                        self.anchor = None;
                        self.preferred_column = None;
                        return;
                    }
                    KeyCode::Right => {
                        self.cursor = range.end;
                        self.anchor = None;
                        self.preferred_column = None;
                        return;
                    }
                    _ => {}
                }
            }
            if shift {
                self.anchor.get_or_insert(old);
            } else {
                self.anchor = None;
            }
            match key.code {
                KeyCode::Left if command => self.cursor = start,
                KeyCode::Right if command => self.cursor = end,
                KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                    self.cursor = self.word_left()
                }
                KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                    self.cursor = self.word_right()
                }
                KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
                KeyCode::Right => self.cursor = self.cursor.saturating_add(1).min(self.chars.len()),
                KeyCode::Home => self.cursor = if command { 0 } else { start },
                KeyCode::End => self.cursor = if command { self.chars.len() } else { end },
                KeyCode::Up if command => self.cursor = 0,
                KeyCode::Down if command => self.cursor = self.chars.len(),
                KeyCode::Up | KeyCode::Down => {
                    let positions = self.positions(self.width.get().max(1));
                    if let Some(&(x, y)) = positions.get(old) {
                        let column = *self.preferred_column.get_or_insert(x);
                        let target = if key.code == KeyCode::Up {
                            y.saturating_sub(1)
                        } else {
                            y.saturating_add(1)
                        };
                        if target != y
                            && let Some((index, _)) = positions
                                .iter()
                                .enumerate()
                                .filter(|(_, (_, row))| *row == target)
                                .min_by_key(|(_, (col, _))| col.abs_diff(column))
                        {
                            self.cursor = index;
                        }
                    }
                }
                _ => {}
            }
            if let Some(fold) = self
                .folds
                .iter()
                .find(|f| f.attachment && f.range.start < self.cursor && self.cursor < f.range.end)
            {
                self.cursor = if self.cursor < old {
                    fold.range.start
                } else {
                    fold.range.end
                };
            }
            if !matches!(key.code, KeyCode::Up | KeyCode::Down) {
                self.preferred_column = None;
            }
            return;
        }
        match key.code {
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT,
                ) =>
            {
                self.insert_raw(&c.to_string())
            }
            KeyCode::Enter => self.insert_raw("\n"),
            KeyCode::Backspace | KeyCode::Delete => {
                self.preferred_column = None;
                if !self.delete_selection() {
                    if key.code == KeyCode::Backspace && command {
                        // Delete the logical line, including its separator; preserve adjacent lines.
                        let range = if end < self.chars.len() {
                            start..end + 1
                        } else {
                            start.saturating_sub(1)..end
                        };
                        self.replace(range, "");
                    } else if key.code == KeyCode::Backspace
                        && key.modifiers.contains(KeyModifiers::ALT)
                    {
                        self.replace(self.word_left()..self.cursor, "");
                    } else if key.code == KeyCode::Backspace && self.cursor > 0 {
                        self.replace(self.cursor - 1..self.cursor, "");
                    } else if key.code == KeyCode::Delete && self.cursor < self.chars.len() {
                        self.replace(self.cursor..self.cursor + 1, "");
                    }
                }
            }
            _ => {}
        }
    }
    fn positions(&self, width: usize) -> Vec<(usize, usize)> {
        let width = if self.width.get() == 0 {
            usize::MAX
        } else {
            width.max(1)
        };
        let mut x: usize = 0;
        let mut y = 0;
        self.chars
            .iter()
            .chain(std::iter::once(&'\0'))
            .enumerate()
            .map(|(index, c)| {
                let w = if *c == '\t' {
                    4
                } else {
                    c.width().unwrap_or(0)
                };
                let word_start = *c != '\0'
                    && !c.is_whitespace()
                    && (index == 0 || self.chars.get(index - 1).is_some_and(|c| c.is_whitespace()));
                if word_start && x > 0 {
                    let word_width = self
                        .chars
                        .iter()
                        .skip(index)
                        .take_while(|c| !c.is_whitespace())
                        .fold(0usize, |total, ch| {
                            total.saturating_add(ch.width().unwrap_or(0))
                        });
                    if word_width <= width && x.saturating_add(word_width) > width {
                        x = 0;
                        y += 1;
                    }
                }
                // Trailing spaces belong to the preceding row. They must not create
                // an empty row before the next word; the source remains unchanged.
                if *c != '\n' && !c.is_whitespace() && (x.saturating_add(w) > width || x >= width) {
                    x = 0;
                    y += 1;
                }
                let position = (x, y);
                if *c == '\n' {
                    x = 0;
                    y += 1;
                } else {
                    x = x.saturating_add(w);
                }
                position
            })
            .collect()
    }
    pub fn mention(&self) -> Option<(usize, String)> {
        if self.selection().is_some() {
            return None;
        }
        let start = self
            .chars
            .iter()
            .take(self.cursor)
            .rposition(|c| c.is_whitespace())
            .map_or(0, |i| i + 1);
        let word = self
            .chars
            .get(start..self.cursor)?
            .iter()
            .collect::<String>();
        let query = word.strip_prefix('@')?;
        query
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
            .then(|| (start, query.to_lowercase()))
    }
    fn complete_raw(&mut self, login: &str) {
        if let Some((start, _)) = self.mention() {
            self.replace(start..self.cursor, &format!("@{login} "));
        }
    }
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let (rows, cursor) = self.styled_layout(width, Style::default());
        (
            rows.into_iter().map(|row| row.to_string()).collect(),
            cursor,
        )
    }
    pub fn styled_layout(
        &self,
        width: usize,
        selected: Style,
    ) -> (Vec<Line<'static>>, (usize, usize)) {
        self.width.set(width.max(1));
        let positions = self.positions(width);
        let mut rows = vec![Line::default()];
        let range = self.selection();
        let colors = if self.syntax {
            crate::ui::syntax_spans(&self.chars.iter().collect::<String>())
                .into_iter()
                .flat_map(|span| {
                    let style = span.style;
                    span.content.chars().map(move |_| style).collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for (index, character) in self.chars.iter().enumerate() {
            let Some(&(column, row)) = positions.get(index) else {
                continue;
            };
            while rows.len() <= row {
                rows.push(Line::default());
            }
            let is_selected = range.as_ref().is_some_and(|r| r.contains(&index));
            if column < width.max(1) && (*character != '\n' || is_selected) {
                let text = match character {
                    '\t' => " ".repeat(width.max(1).saturating_sub(column).min(4)),
                    '\n' => " ".into(),
                    c => c.to_string(),
                };
                if let Some(line) = rows.get_mut(row) {
                    line.spans.push(Span::styled(
                        text,
                        if is_selected {
                            selected
                        } else if self
                            .folds
                            .iter()
                            .any(|fold| fold.attachment && fold.range.contains(&index))
                        {
                            Style::default().fg(crate::ui::ACCENT)
                        } else {
                            colors.get(index).copied().unwrap_or_default()
                        },
                    ));
                }
            }
        }
        let cursor = positions
            .get(self.cursor.min(self.chars.len()))
            .copied()
            .unwrap_or_default();
        let last_row = positions.last().map_or(cursor.1, |(_, row)| *row);
        while rows.len() <= last_row {
            rows.push(Line::default());
        }
        (rows, cursor)
    }
}
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attachments_stay_inline_atomic_and_distinct_from_large_pastes() {
        let mut editor = Editor::from("before  after");
        editor.cursor = 7;
        editor.insert_attachment("[image 1]");
        assert_eq!(editor.text(), "before [image 1] after");
        assert!(!editor.expand_paste());
        editor.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(editor.cursor, 7);
        editor.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(editor.cursor, 16);
        editor.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(editor.text(), "before  after");
        assert!(editor.attachment_tokens().is_empty());
        editor.undo();
        assert_eq!(editor.attachment_tokens(), ["[image 1]"]);
        let content = "hello ".repeat(100);
        editor.paste(&content);
        assert_eq!(editor.text(), format!("before [image 1]{content} after"));
        assert!(editor.expand_paste());
        assert_eq!(editor.attachment_tokens(), ["[image 1]"]);
        editor.cursor = 8; // A mouse selection starting inside the token still deletes it whole.
        editor.anchor = Some(9);
        editor.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert!(editor.attachment_tokens().is_empty());
        editor.undo();
        assert_eq!(editor.attachment_tokens(), ["[image 1]"]);
    }

    #[test]
    fn word_wrapping_keeps_cursor_selection_and_source_aligned() {
        let mut editor = Editor::from("one two three");
        let (rows, cursor) = editor.layout(9);
        assert_eq!(rows, ["one two ", "three"]);
        assert_eq!(cursor, (5, 1));
        editor.cursor = 0;
        editor.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(editor.cursor, 8);
        assert_eq!(editor.selected_text().as_deref(), Some("one two "));
        assert_eq!(editor.layout(9).1, (0, 1));
        let editor = Editor::from("abc def");
        let (rows, cursor) = editor.layout(3);
        assert_eq!(rows, ["abc", "def", ""]);
        assert_eq!(cursor, (0, 2));
        assert_eq!(editor.text(), "abc def");
        let editor = Editor::from("hi 世界 ok");
        assert_eq!(editor.layout(6).0, ["hi ", "世界 ", "ok"]);
        let editor = Editor::from("abcdefghij");
        assert_eq!(editor.layout(4).0, ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn terminal_control_aliases_move_delete_and_undo_logical_lines() {
        let mut e = Editor::from("before\ncurrent 世界 line\nafter");
        e.layout(5);
        e.cursor = 10;
        e.anchor = Some(12);
        e.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(e.cursor, 7);
        assert!(e.selection().is_none());
        e.key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(e.cursor, 22);
        e.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(e.text(), "before\nafter");
        e.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::SUPER));
        assert_eq!(e.text(), "before\ncurrent 世界 line\nafter");
        assert_eq!(e.cursor, 22);
        e.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
        assert_eq!(
            e.selected_text().as_deref(),
            Some("before\ncurrent 世界 line\nafter")
        );
    }
    #[test]
    fn alt_backspace_deletes_words_and_undo_restores_them() {
        let mut e = Editor::from("hello, 世界 foo_bar");
        let delete = KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT);
        e.key(delete);
        assert_eq!(e.text(), "hello, 世界 ");
        e.key(delete);
        assert_eq!(e.text(), "hello, ");
        e.undo();
        assert_eq!(e.text(), "hello, 世界 ");
        e.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        e.key(delete);
        assert_eq!(e.text(), "hello, 世界 ");
        e.cursor = e.chars.len();
        e.paste(&"x".repeat(500));
        e.key(delete);
        assert_eq!(e.text(), "hello, 世界 ");
        e.undo();
        assert_eq!(e.text(), format!("hello, 世界 {}", "x".repeat(500)));
    }
    #[test]
    fn command_backspace_removes_current_line_without_touching_neighbors() {
        let delete = KeyEvent::new(KeyCode::Backspace, KeyModifiers::SUPER);
        let mut e = Editor::from("before\ncurrent 世界 line\nafter");
        e.cursor = 10; // Middle of the current line: remove both sides of the cursor.
        e.key(delete);
        assert_eq!(e.text(), "before\nafter");
        assert_eq!(e.cursor, 7);
        e.undo();
        assert_eq!(e.text(), "before\ncurrent 世界 line\nafter");
        assert_eq!(e.cursor, 10);
        e.cursor = e.chars.len();
        e.key(delete);
        assert_eq!(e.text(), "before\ncurrent 世界 line");
        e.cursor = 0;
        e.key(delete);
        assert_eq!(e.text(), "current 世界 line");
        e.key(delete);
        assert_eq!(e.text(), "");
        e.key(delete);
        assert_eq!(e.cursor, 0);
        let mut e = Editor::from("first\n\nlast");
        e.cursor = 6;
        e.key(delete);
        assert_eq!(e.text(), "first\nlast");
    }
    #[test]
    fn command_arrows_reach_logical_line_edges_even_with_selection_and_wrapping() {
        let mut e = Editor::from("before\nlong current line\nafter");
        e.layout(5);
        e.cursor = 12;
        e.anchor = Some(10);
        e.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SUPER));
        assert_eq!(e.cursor, 7);
        assert!(e.selection().is_none());
        e.key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ));
        assert_eq!(e.selected_text().as_deref(), Some("long current line"));
        e.cursor = 12;
        e.anchor = Some(10);
        e.key(KeyEvent::new(KeyCode::Right, KeyModifiers::SUPER));
        assert_eq!(e.cursor, 24);
        assert!(e.selection().is_none());
    }
    #[test]
    fn native_and_ghostty_word_navigation_match_without_editing_text() {
        for (left, right) in [
            (KeyCode::Left, KeyCode::Right),
            (KeyCode::Char('b'), KeyCode::Char('f')),
        ] {
            let mut editor = Editor::from("hello, 世界 foo_bar");
            for expected in [10, 7, 0, 0] {
                editor.key(KeyEvent::new(left, KeyModifiers::ALT));
                assert_eq!(editor.cursor, expected);
                assert!(editor.selection().is_none());
            }
            for expected in [5, 9, 17, 17] {
                editor.key(KeyEvent::new(right, KeyModifiers::ALT));
                assert_eq!(editor.cursor, expected);
                assert!(editor.selection().is_none());
            }
            assert_eq!(editor.text(), "hello, 世界 foo_bar");
        }
    }
    #[test]
    fn word_selection_crosses_punctuation_and_preserves_unicode_anchor() {
        let mut e = Editor::from("hello, 世界 foo_bar");
        let select = KeyModifiers::SHIFT | KeyModifiers::ALT;
        e.key(KeyEvent::new(KeyCode::Left, select));
        assert_eq!(e.selected_text().as_deref(), Some("foo_bar"));
        e.key(KeyEvent::new(KeyCode::Left, select));
        assert_eq!(e.selected_text().as_deref(), Some("世界 foo_bar"));
        e.key(KeyEvent::new(KeyCode::Right, select));
        assert_eq!(e.selected_text().as_deref(), Some(" foo_bar"));
        e.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        e.key(KeyEvent::new(KeyCode::Right, select));
        assert_eq!(e.selected_text().as_deref(), Some("hello"));
        e.key(KeyEvent::new(KeyCode::Right, select));
        assert_eq!(e.selected_text().as_deref(), Some("hello, 世界"));
    }
    #[test]
    fn undo_restores_selection_paste_tokens_and_completion_as_single_edits() {
        let mut e = Editor::from("hello 世界");
        e.key(KeyEvent::new(
            KeyCode::Left,
            KeyModifiers::SHIFT | KeyModifiers::ALT,
        ));
        e.insert("replacement");
        e.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::SUPER));
        assert_eq!(e.text(), "hello 世界");
        assert_eq!(e.selected_text().as_deref(), Some("世界"));
        let pasted = "λ".repeat(500);
        e.paste(&pasted);
        assert_eq!(e.text(), format!("hello {pasted}"));
        assert!(e.expand_paste());
        e.undo();
        assert!(e.chars.iter().collect::<String>().contains("500 chars"));
        e.undo();
        assert_eq!(e.text(), "hello 世界");
        assert_eq!(e.selected_text().as_deref(), Some("世界"));
        e.clear();
        e.undo();
        assert_eq!(e.text(), "hello 世界");
        let mut e = Editor::from("@al");
        e.complete("alice");
        e.undo();
        assert_eq!(e.text(), "@al");
        e.undo(); // Initial field value is not an edit.
        assert_eq!(e.text(), "@al");
    }
    #[test]
    fn shifted_punctuation_and_letters_are_inserted_verbatim() {
        let mut e = Editor::default();
        for c in "?{}ABC!".chars() {
            e.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT));
        }
        assert_eq!(e.text(), "?{}ABC!");
        e.undo();
        assert_eq!(e.text(), "?{}ABC");
    }
    #[test]
    fn collapsed_pastes_expand_for_sending_copy_and_repeat_paste() {
        let a = "α".repeat(500);
        let b = "b".repeat(510);
        let mut e = Editor::default();
        e.paste(&a);
        assert!(e.chars.iter().collect::<String>().contains("500 chars"));
        assert_eq!(e.text(), a);
        e.insert(" ");
        e.paste(&b);
        assert_eq!(e.text(), format!("{a} {b}"));
        e.paste(&b); // Terminal Cmd+V arrives as a bracketed paste.
        assert!(!e.chars.iter().collect::<String>().contains("510 chars"));
        assert_eq!(e.text(), format!("{a} {b}"));
        e.cursor = 0;
        assert!(e.expand_paste());
        e.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
        assert_eq!(e.selected_text(), Some(format!("{a} {b}")));
        e.insert("replacement");
        assert_eq!(e.text(), "replacement");
        e.paste(&a);
        e.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(e.text(), "replacement");
    }
    #[test]
    fn pasted_code_keeps_syntax_colors_and_selection_overlay() {
        let mut e = Editor::default();
        e.paste("const name = \"value\";\nreturn 42;");
        let (rows, _) = e.styled_layout(80, Style::default().bg(ratatui::style::Color::Blue));
        assert!(
            rows.iter()
                .flat_map(|r| &r.spans)
                .any(|s| s.style.fg == Some(crate::ui::ACCENT))
        );
        e.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
        let (rows, _) = e.styled_layout(80, Style::default().bg(ratatui::style::Color::Blue));
        assert!(
            rows.iter()
                .flat_map(|r| &r.spans)
                .all(|s| s.style.bg == Some(ratatui::style::Color::Blue))
        );
    }
    #[test]
    fn shift_selection_reverses_and_replaces_unicode_text() {
        let mut editor = Editor::default();
        editor.insert("hello 🦀世界");
        editor.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        editor.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        assert_eq!(editor.selected_text().as_deref(), Some("世界"));
        editor.key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(editor.selected_text().as_deref(), Some("界"));
        editor.insert("!");
        assert_eq!(editor.text(), "hello 🦀世!");
        assert!(editor.selection().is_none());
        editor.key(KeyEvent::new(KeyCode::Home, KeyModifiers::SHIFT));
        editor.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(editor.text(), "");
    }
    #[test]
    fn selection_follows_visual_rows_and_preserves_preferred_column() {
        let mut editor = Editor::default();
        editor.insert("abcd\nx\nabcdefgh");
        editor.cursor = 3;
        let _ = editor.layout(4);
        editor.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(editor.cursor, 6);
        editor.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(editor.cursor, 10);
        editor.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(editor.cursor, 14);
        let (lines, _) = editor.styled_layout(4, Style::default().bg(ratatui::style::Color::Green));
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|s| s.style.bg == Some(ratatui::style::Color::Green))
        );
        editor.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(editor.cursor, 3);
        assert!(editor.selection().is_none());
    }
    #[test]
    fn select_all_and_delete_do_not_leak_selection_into_new_input() {
        let mut editor = Editor::default();
        editor.insert("first\nsecond");
        editor.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
        assert_eq!(editor.selected_text().as_deref(), Some("first\nsecond"));
        editor.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        editor.insert("replacement");
        assert_eq!(editor.text(), "replacement");
        assert_eq!(editor.cursor, 11);
        assert!(editor.selection().is_none());
    }
    #[test]
    fn edits_unicode_and_completes_mentions() {
        let mut e = Editor::default();
        e.insert("hi 🦀 @ali");
        e.complete("alice");
        assert_eq!(e.text(), "hi 🦀 @alice ");
        e.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        e.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(e.text(), "i 🦀 @alice ");
        e.cursor = e.chars.len();
        let (rows, (_, y)) = e.layout(5);
        assert!(rows.len() > 1);
        assert!(y < rows.len());
    }
}
