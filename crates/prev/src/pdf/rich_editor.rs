//! A text box's styled runs shown in iced's text editor, which draws one
//! style for its whole text: a highlighter gives each run its font and
//! color, line by line, and the selection is found in the text's bytes so
//! a style can be applied to it.

use std::ops::Range;

use iced::advanced::text::highlighter::{self, Highlighter};
use iced::widget::text_editor;
use iced::{Color, Font, Theme};
use prev_pdf::annotation::{Rgb, TextRun};

/// How a run looks in the editor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    font: Font,
    color: Option<Color>,
}

/// What the highlighter draws: each line's runs as byte ranges in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Lines {
    lines: Vec<Vec<(Range<usize>, Look)>>,
}

impl Lines {
    /// `runs` split at the line breaks of their text, in `font`.
    pub fn new(runs: &[TextRun], font: Font) -> Self {
        let mut lines = vec![Vec::new()];
        let mut column = 0;
        for run in runs {
            let look = Look {
                font: Font {
                    weight: if run.bold {
                        iced::font::Weight::Bold
                    } else {
                        font.weight
                    },
                    style: if run.italic {
                        iced::font::Style::Italic
                    } else {
                        font.style
                    },
                    ..font
                },
                color: run.color.map(to_color),
            };
            for (index, part) in run.text.split('\n').enumerate() {
                if index > 0 {
                    lines.push(Vec::new());
                    column = 0;
                }
                if !part.is_empty() {
                    let line = lines.last_mut().expect("a line");
                    line.push((column..column + part.len(), look));
                    column += part.len();
                }
            }
        }
        Self { lines }
    }
}

/// Draws [`Lines`].
pub struct RunHighlighter {
    lines: Lines,
    current: usize,
}

impl Highlighter for RunHighlighter {
    type Settings = Lines;
    type Highlight = Look;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Look)>;

    fn new(settings: &Self::Settings) -> Self {
        Self {
            lines: settings.clone(),
            current: 0,
        }
    }

    fn update(&mut self, new_settings: &Self::Settings) {
        self.lines = new_settings.clone();
        self.current = 0;
    }

    fn change_line(&mut self, line: usize) {
        self.current = self.current.min(line);
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        let spans = self
            .lines
            .lines
            .get(self.current)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            // A line typed longer than the runs know of yet keeps in range.
            .filter_map(|(range, look)| {
                let end = range.end.min(line.len());
                (range.start < end).then_some((range.start..end, look))
            })
            .collect::<Vec<_>>();
        self.current += 1;
        spans.into_iter()
    }

    fn current_line(&self) -> usize {
        self.current
    }
}

/// The format iced draws a [`Look`] in.
pub fn format(look: &Look, _theme: &Theme) -> highlighter::Format<Font> {
    highlighter::Format {
        color: look.color,
        font: Some(look.font),
    }
}

/// The selected bytes of the editor's text, if any. A word or line
/// picked with a double or triple click is reported at the cursor alone,
/// so it is found by its text around the cursor.
pub fn selection(content: &text_editor::Content) -> Option<Range<usize>> {
    let selected = content
        .selection()
        .filter(|selected| !selected.is_empty())?;
    let cursor = content.cursor();
    let text = content.text();
    let offset = |line: usize, column: usize| -> usize {
        text.split('\n')
            .take(line)
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + column
    };
    let at = offset(cursor.position.line, cursor.position.column);
    if let Some(other) = cursor.selection {
        let other = offset(other.line, other.column);
        let range = at.min(other)..at.max(other);
        if text.get(range.clone()) == Some(selected.as_str()) {
            return Some(range);
        }
    }
    // The occurrence of the selected text that holds the cursor, or the
    // nearest one.
    text.match_indices(selected.as_str())
        .map(|(start, found)| start..start + found.len())
        .min_by_key(|range| {
            if range.contains(&at) || range.end == at {
                0
            } else {
                range.start.abs_diff(at).min(range.end.abs_diff(at))
            }
        })
}

fn to_color(rgb: Rgb) -> Color {
    Color::from_rgb(rgb.red, rgb.green, rgb.blue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_split_into_lines_by_bytes() {
        let runs = vec![
            TextRun {
                text: "café ".into(),
                ..TextRun::default()
            },
            TextRun {
                text: "bold\nnext".into(),
                bold: true,
                ..TextRun::default()
            },
        ];
        let lines = Lines::new(&runs, Font::DEFAULT);
        assert_eq!(lines.lines.len(), 2);
        // "café " is six bytes.
        assert_eq!(lines.lines[0][0].0, 0..6);
        assert_eq!(lines.lines[0][1].0, 6..10);
        assert_eq!(lines.lines[0][1].1.font.weight, iced::font::Weight::Bold);
        assert_eq!(lines.lines[1][0].0, 0..4);
    }

    #[test]
    fn the_selection_is_found_across_lines() {
        let mut content = text_editor::Content::with_text("one\ntwo three");
        content.perform(text_editor::Action::Move(
            text_editor::Motion::DocumentStart,
        ));
        content.perform(text_editor::Action::Move(text_editor::Motion::Down));
        content.perform(text_editor::Action::Select(text_editor::Motion::WordRight));
        let range = selection(&content).expect("a selection");
        assert_eq!(&content.text()[range], "two");
    }

    #[test]
    fn a_double_clicked_word_is_found() {
        let mut content = text_editor::Content::with_text("one two\ntwo three");
        content.perform(text_editor::Action::Move(
            text_editor::Motion::DocumentStart,
        ));
        content.perform(text_editor::Action::Move(text_editor::Motion::Down));
        content.perform(text_editor::Action::Move(text_editor::Motion::Right));
        content.perform(text_editor::Action::SelectWord);
        let range = selection(&content).expect("a selection");
        // The second "two", where the cursor is, not the first.
        assert_eq!(range, 8..11);
    }
}
