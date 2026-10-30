//! Bounded composer state; canonical messages are owned by core, not this editor.
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
pub const MAX_INPUT: usize = 64 * 1024;
pub fn display(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            ch if ch.is_control() || matches!(ch as u32,0x202a..=0x202e|0x2066..=0x2069) => {
                out.push_str(&format!("\\u{{{:x}}}", ch as u32))
            }
            _ => out.push(ch),
        }
    }
    out
}
#[derive(Default)]
pub struct InputBuffer {
    value: String,
    cursor: usize,
    preferred: Option<usize>,
}
pub struct View {
    pub lines: Vec<String>,
    pub x: usize,
    pub y: usize,
}
impl InputBuffer {
    pub fn value(&self) -> &str {
        &self.value
    }
    pub fn insert(&mut self, ch: char) {
        let mut text = [0; 4];
        self.insert_text(ch.encode_utf8(&mut text));
    }
    pub fn insert_text(&mut self, text: &str) -> bool {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        if normalized.len() > MAX_INPUT.saturating_sub(self.value.len()) {
            return false;
        }
        self.value.insert_str(self.cursor, &normalized);
        let target = self.cursor + normalized.len();
        self.cursor = self
            .value
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|i| *i >= target)
            .unwrap_or(self.value.len());
        self.preferred = None;
        true
    }
    pub fn replace(&mut self, value: String) {
        if value.len() <= MAX_INPUT {
            self.cursor = value.len();
            self.value = value;
            self.preferred = None;
        }
    }
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        self.preferred = None;
        std::mem::take(&mut self.value)
    }
    fn previous(&self) -> usize {
        self.value[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }
    fn next(&self) -> usize {
        self.value[self.cursor..]
            .grapheme_indices(true)
            .nth(1)
            .map_or(self.value.len(), |(i, _)| self.cursor + i)
    }
    pub fn backspace(&mut self) {
        let previous = self.previous();
        self.value.drain(previous..self.cursor);
        self.cursor = previous;
        self.preferred = None;
    }
    pub fn delete(&mut self) {
        let next = self.next();
        self.value.drain(self.cursor..next);
        self.preferred = None;
    }
    pub fn move_left(&mut self) {
        self.cursor = self.previous();
        self.preferred = None;
    }
    pub fn move_right(&mut self) {
        self.cursor = self.next();
        self.preferred = None;
    }
    pub fn move_home(&mut self) {
        self.cursor = self.value[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
        self.preferred = None;
    }
    pub fn move_end(&mut self) {
        self.cursor = self.value[self.cursor..]
            .find('\n')
            .map_or(self.value.len(), |i| self.cursor + i);
        self.preferred = None;
    }
    pub fn document_home(&mut self) {
        self.cursor = 0;
        self.preferred = None;
    }
    pub fn document_end(&mut self) {
        self.cursor = self.value.len();
        self.preferred = None;
    }
    pub fn vertical(&mut self, width: usize, down: bool) -> bool {
        let (view, positions) = self.layout(width, true);
        let desired = self.preferred.unwrap_or(view.x);
        let target = if down {
            view.y + 1
        } else if view.y > 0 {
            view.y - 1
        } else {
            return false;
        };
        let mut selected = None;
        for (byte, x, y) in positions {
            if y == target && x <= desired {
                selected = Some(byte);
            }
        }
        if let Some(byte) = selected {
            self.cursor = byte;
            self.preferred = Some(desired);
            true
        } else {
            false
        }
    }
    pub fn view(&self, width: usize) -> View {
        self.layout(width, false).0
    }
    fn layout(&self, width: usize, remember: bool) -> (View, Vec<(usize, usize, usize)>) {
        let width = width.max(1);
        let mut lines = vec![String::new()];
        let mut x = 0;
        let mut cursor = (0, 0);
        let mut positions = Vec::new();
        let mut soft = false;
        for (i, g) in self.value.grapheme_indices(true) {
            if g == "\n" {
                if i == self.cursor {
                    cursor = (x, lines.len() - 1)
                }
                if remember {
                    positions.push((i, x, lines.len() - 1));
                }
                if !soft {
                    lines.push(String::new());
                }
                soft = false;
                x = 0;
                continue;
            }
            let shown = display(g);
            let actual = UnicodeWidthStr::width(shown.as_str());
            let cells = actual.min(width);
            if cells > 0 && x + cells > width {
                lines.push(String::new());
                x = 0;
            }
            if i == self.cursor {
                cursor = (x, lines.len() - 1)
            }
            if remember {
                positions.push((i, x, lines.len() - 1));
            }
            if actual > width {
                lines.last_mut().unwrap().push('?');
                x += 1;
            } else {
                lines.last_mut().unwrap().push_str(&shown);
                x += cells;
            }
            soft = false;
            if x >= width {
                lines.push(String::new());
                x = 0;
                soft = true;
            }
        }
        if self.cursor == self.value.len() {
            cursor = (x, lines.len() - 1)
        }
        if remember {
            positions.push((self.value.len(), x, lines.len() - 1));
        }
        (
            View {
                lines,
                x: cursor.0,
                y: cursor.1,
            },
            positions,
        )
    }
}
#[derive(Default)]
pub struct History {
    entries: VecDeque<String>,
    bytes: usize,
    position: Option<usize>,
    draft: String,
}
impl History {
    pub fn record(&mut self, text: &str) {
        self.position = None;
        self.draft.clear();
        if text.trim().is_empty() || text.len() > MAX_INPUT {
            return;
        }
        if self.entries.back().is_some_and(|s| s == text) {
            return;
        }
        while self.entries.len() >= 100 || self.bytes + text.len() > 256 * 1024 {
            if let Some(old) = self.entries.pop_front() {
                self.bytes -= old.len();
            } else {
                break;
            }
        }
        self.bytes += text.len();
        self.entries.push_back(text.into());
    }
    pub fn previous(&mut self, current: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let index = match self.position {
            None => {
                self.draft = current.into();
                self.entries.len() - 1
            }
            Some(i) => i.saturating_sub(1),
        };
        self.position = Some(index);
        self.entries.get(index).cloned()
    }
    pub fn next(&mut self) -> Option<String> {
        let index = self.position? + 1;
        if index >= self.entries.len() {
            self.position = None;
            Some(std::mem::take(&mut self.draft))
        } else {
            self.position = Some(index);
            self.entries.get(index).cloned()
        }
    }
    pub fn edited(&mut self) {
        self.position = None;
        self.draft.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grapheme_edits_and_wrapping_preserve_unicode_and_line_boundaries() {
        let mut input = InputBuffer::default();
        assert!(input.insert_text("e\u{301}👩‍💻界\nnext"));
        input.document_home();
        input.move_right();
        assert_eq!(input.cursor, 3);
        input.delete();
        assert_eq!(input.value(), "e\u{301}界\nnext");
        input.backspace();
        assert_eq!(input.value(), "界\nnext");
        let mut wrapped = InputBuffer::default();
        wrapped.insert_text("ab\n界x");
        let view = wrapped.view(2);
        assert_eq!(view.lines, vec!["ab", "界", "x"]);
        assert_eq!((view.x, view.y), (1, 2));
        assert!(wrapped.vertical(2, false));
        assert_eq!(wrapped.cursor, 3);
        assert!(wrapped.vertical(2, true));
        assert_eq!(wrapped.cursor, 7);
        let mut joined = InputBuffer::default();
        joined.insert_text("👩💻");
        joined.document_home();
        joined.move_right();
        joined.insert('\u{200d}');
        joined.backspace();
        assert!(joined.value().is_empty());
    }
    #[test]
    fn oversized_paste_is_atomic_and_controls_are_only_display_escaped() {
        let mut input = InputBuffer::default();
        input.insert_text("draft");
        assert!(!input.insert_text(&"x".repeat(MAX_INPUT)));
        assert_eq!(input.value(), "draft");
        input.replace(String::new());
        input.insert_text("one\r\ntwo\rthree\x1b[31m");
        assert_eq!(input.value(), "one\ntwo\nthree\x1b[31m");
        let view = input.view(40);
        assert!(view.lines.last().unwrap().contains("\\u{1b}[31m"));
        assert!(!view.lines.join("\n").contains('\x1b'));
    }
    #[test]
    fn bounded_history_restores_the_draft_and_never_replays_implicitly() {
        let mut history = History::default();
        history.record("one");
        history.record("two");
        assert_eq!(history.previous("draft").unwrap(), "two");
        assert_eq!(history.previous("ignored").unwrap(), "one");
        assert_eq!(history.next().unwrap(), "two");
        assert_eq!(history.next().unwrap(), "draft");
        assert!(history.next().is_none());
        for i in 0..200 {
            history.record(&format!("{i} {}", "x".repeat(4000)));
        }
        assert!(history.entries.len() <= 100);
        assert!(history.bytes <= 256 * 1024);
    }
}
