//! Search/filter editing and browser state; playback stays in App.
use crate::api::{Playlist, Track};
use crossterm::event::{KeyCode, KeyModifiers};

#[derive(Default)]
pub struct Input {
    pub text: String,
    pub editing: bool,
}

impl Input {
    /// Returns true when the editor consumed the key. Navigation waits for Enter.
    pub fn key(&mut self, key: KeyCode, mods: KeyModifiers) -> bool {
        if !self.editing {
            return false;
        }
        match key {
            KeyCode::Esc => {
                self.text.clear();
                self.editing = false;
            }
            KeyCode::Enter => self.editing = false,
            KeyCode::Backspace => {
                self.text.pop();
            }
            KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => self.text.clear(),
            KeyCode::Char(c) if !c.is_control() && !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.text.push(c)
            }
            _ => {}
        }
        true
    }
}

pub fn matches(title: &str, query: &str) -> bool {
    let title = title.to_lowercase();
    query
        .split_whitespace()
        .all(|word| title.contains(&word.to_lowercase()))
}

#[derive(Default)]
pub struct Search {
    pub input: Input,
    pub tracks: Vec<Track>,
    pub query: String,
    pub selected: usize,
    pub top: usize,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Playlists {
    pub opening: bool,
    pub items: Vec<Playlist>,
    pub selected: usize,
    pub top: usize,
    pub loading: bool,
    pub loaded: bool,
    pub error: Option<String>,
}

/// Scroll an index list without changing its source indices.
pub fn scroll(top: &mut usize, selected: usize, len: usize, rows: usize) {
    if selected < *top {
        *top = selected;
    } else if selected >= *top + rows {
        *top = selected.saturating_add(1).saturating_sub(rows);
    }
    *top = (*top).min(len.saturating_sub(rows));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filter_matches_unicode_words_in_any_order() {
        assert!(matches("Björk — Jóga 日本", "日本 BJÖRK"));
        assert!(!matches("Björk — Jóga", "björk missing"));
        assert!(matches("title", "  "));
    }
    #[test]
    fn text_input_consumes_playback_keys_and_clears_without_quitting() {
        let mut input = Input {
            editing: true,
            ..Default::default()
        };
        for c in ['n', 'q', ' ', '日', '本'] {
            assert!(input.key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(input.text, "nq 日本");
        input.key(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(input.text, "nq 日");
        input.key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(input.text.is_empty());
        assert!(!input.editing);
    }
    #[test]
    fn scrolling_keeps_filtered_position_in_bounds_after_resizing() {
        let mut top = 0;
        scroll(&mut top, 9, 10, 2);
        assert_eq!(top, 8);
        scroll(&mut top, 9, 10, 20);
        assert_eq!(top, 0);
        scroll(&mut top, 0, 0, 0);
        assert_eq!(top, 0);
    }
}
