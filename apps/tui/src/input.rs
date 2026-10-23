//! Bounded UTF-8 text editing. Cursor offsets always stay on character boundaries.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Default, Debug, Clone)]
pub(crate) struct Input {
    pub text: String,
    pub cursor: usize,
}
impl Input {
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
    pub fn insert(&mut self, value: &str, limit: usize, multiline: bool) {
        for character in value
            .chars()
            .filter(|c| !c.is_control() || (multiline && *c == '\n'))
        {
            if self.text.len() + character.len_utf8() > limit {
                break;
            }
            self.text.insert(self.cursor, character);
            self.cursor += character.len_utf8();
        }
    }
    pub fn row_column(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        (
            before.bytes().filter(|b| *b == b'\n').count(),
            before
                .rsplit('\n')
                .next()
                .unwrap_or_default()
                .chars()
                .count(),
        )
    }
    fn vertical(&mut self, delta: isize) {
        let (row, column) = self.row_column();
        let lines = self.text.split('\n').collect::<Vec<_>>();
        let next = row
            .saturating_add_signed(delta)
            .min(lines.len().saturating_sub(1));
        let start = lines[..next]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>();
        let offset = lines[next]
            .char_indices()
            .nth(column)
            .map(|(index, _)| index)
            .unwrap_or(lines[next].len());
        self.cursor = start + offset;
    }
    pub fn key(&mut self, key: KeyEvent, limit: usize, multiline: bool) {
        match key.code {
            KeyCode::Left => {
                self.cursor = self.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map(|(index, _)| index)
                    .unwrap_or(0)
            }
            KeyCode::Right => {
                self.cursor += self.text[self.cursor..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or(0)
            }
            KeyCode::Up if multiline => self.vertical(-1),
            KeyCode::Down if multiline => self.vertical(1),
            KeyCode::Home => {
                self.cursor = self.text[..self.cursor]
                    .rfind('\n')
                    .map(|index| index + 1)
                    .unwrap_or(0)
            }
            KeyCode::End => {
                self.cursor += self.text[self.cursor..]
                    .find('\n')
                    .unwrap_or(self.text.len() - self.cursor)
            }
            KeyCode::Backspace => {
                if let Some((previous, _)) = self.text[..self.cursor].char_indices().next_back() {
                    self.text.drain(previous..self.cursor);
                    self.cursor = previous;
                }
            }
            KeyCode::Delete => {
                if let Some(character) = self.text[self.cursor..].chars().next() {
                    self.text
                        .drain(self.cursor..self.cursor + character.len_utf8());
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => self.clear(),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&character.to_string(), limit, multiline)
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    #[test]
    fn unicode_cursor_editing_does_not_split_characters() {
        let mut input = Input::default();
        input.insert("aé🙂z", 64, false);
        input.key(key(KeyCode::Left), 64, false);
        input.key(key(KeyCode::Left), 64, false);
        input.key(key(KeyCode::Backspace), 64, false);
        assert_eq!(input.text, "a🙂z");
        assert_eq!(input.cursor, 1);
        input.key(key(KeyCode::Delete), 64, false);
        assert_eq!(input.text, "az");
        input.key(key(KeyCode::Home), 64, false);
        input.insert("é", 64, false);
        assert_eq!(input.text, "éaz");
        assert!(input.text.is_char_boundary(input.cursor));
    }
    #[test]
    fn multiline_arrows_preserve_columns_and_paste_is_bounded() {
        let mut input = Input::default();
        input.insert("abc\né\nxyz", 64, true);
        input.key(key(KeyCode::Up), 64, true);
        assert_eq!(input.row_column(), (1, 1));
        input.key(key(KeyCode::Up), 64, true);
        assert_eq!(input.row_column(), (0, 1));
        input.key(key(KeyCode::Down), 64, true);
        assert_eq!(input.row_column(), (1, 1));
        input.clear();
        input.insert("ééé", 5, false);
        assert_eq!(input.text, "éé");
        input.clear();
        input.insert("y\nn\x1bq", 20, false);
        assert_eq!(input.text, "ynq");
    }
}
