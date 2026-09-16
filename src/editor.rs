//! Small Unicode-safe editor shared by comment/review text boxes.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, Default)]
pub struct Editor {
    pub chars: Vec<char>,
    pub cursor: usize,
}
impl Editor {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }
    pub fn insert(&mut self, text: &str) {
        let chars = text
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect::<Vec<_>>();
        let position = self.cursor.min(self.chars.len());
        self.cursor = position + chars.len();
        self.chars.splice(position..position, chars);
    }
    pub fn key(&mut self, key: KeyEvent) {
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
        match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
                self.insert(&c.to_string())
            }
            KeyCode::Enter => self.insert("\n"),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            KeyCode::Home => self.cursor = start,
            KeyCode::End => self.cursor = end,
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.chars.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.chars.len() => {
                self.chars.remove(self.cursor);
            }
            KeyCode::Up if start > 0 => {
                let previous = self
                    .chars
                    .iter()
                    .take(start.saturating_sub(1))
                    .rposition(|c| *c == '\n')
                    .map_or(0, |n| n + 1);
                self.cursor = (previous + self.cursor - start).min(start - 1);
            }
            KeyCode::Down if end < self.chars.len() => {
                let next_end = self
                    .chars
                    .iter()
                    .enumerate()
                    .skip(end + 1)
                    .find_map(|(n, c)| (*c == '\n').then_some(n))
                    .unwrap_or(self.chars.len());
                self.cursor = (end + 1 + self.cursor - start).min(next_end);
            }
            _ => {}
        }
    }
    pub fn mention(&self) -> Option<(usize, String)> {
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
    pub fn complete(&mut self, login: &str) {
        if let Some((start, _)) = self.mention() {
            self.chars.drain(start..self.cursor);
            self.cursor = start;
            self.insert(&format!("@{login} "));
        }
    }
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(1);
        let mut rows = vec![String::new()];
        let mut x = 0;
        let mut y = 0;
        let mut cursor = (0, 0);
        for (i, c) in self.chars.iter().chain(std::iter::once(&'\0')).enumerate() {
            let w = if *c == '\t' {
                4
            } else {
                c.width().unwrap_or(0)
            };
            if *c != '\n' && (x + w > width || x >= width) {
                rows.push(String::new());
                y += 1;
                x = 0;
            }
            if i == self.cursor {
                cursor = (x, y);
            }
            if i == self.chars.len() {
                break;
            }
            if *c == '\n' {
                rows.push(String::new());
                y += 1;
                x = 0;
            } else if let Some(row) = rows.last_mut() {
                if *c == '\t' {
                    row.push_str("    ");
                } else {
                    row.push(*c);
                }
                x += w;
            }
        }
        (rows, cursor)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
