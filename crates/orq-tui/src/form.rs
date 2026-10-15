//! The Add Feature form: a full-body overlay on the Features tab. It only
//! collects values; saving is the backend's job and never starts anything.

use std::collections::HashSet;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{printable, truncate_right};

/// What the form collects. Optional fields that were left alone are `None`
/// (or empty), so nothing is invented for the ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeatureFields {
    pub name: String,
    pub description: Option<String>,
    pub depends_on: Vec<String>,
    pub parallelizable: Option<bool>,
    pub test_file: Option<String>,
    pub target_files: Vec<String>,
}

/// What a backend says about a save the form asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormSaveOutcome {
    Saved,
    /// The values were refused; shown inline and the form stays open.
    Invalid { reason: String },
    /// The file changed since it was loaded; nothing was overwritten.
    Conflict { reason: String },
    /// The write could not be done; shown inline and the form stays open.
    Failed { reason: String },
}

/// What a key or click asked the browser to do with the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormAction {
    None,
    Cancel,
    Save,
}

const NAME: usize = 0;
const DESCRIPTION: usize = 1;
const PARALLELIZABLE: usize = 2;
const TEST_FILE: usize = 3;
const TARGET_FILES: usize = 4;
const FIRST_DEPENDENCY: usize = 5;
/// Lines above the first field: the title and the fixed status.
const FIELD_LINES_FROM: usize = 2;
const DEPENDS_HEADER_LINE: usize = FIELD_LINES_FROM + FIRST_DEPENDENCY;
const HINT: &str = " ↑↓ move · Space toggle · Ctrl+S save · Esc cancel ";

pub(crate) struct FormState {
    name: String,
    description: String,
    parallelizable: Option<bool>,
    test_file: String,
    target_files: String,
    candidates: Vec<String>,
    chosen: HashSet<String>,
    focus: usize,
    pub(crate) error: Option<String>,
}

impl FormState {
    /// An empty form that can depend on any of `candidates`.
    pub(crate) fn new_add(candidates: Vec<String>) -> FormState {
        FormState {
            name: String::new(),
            description: String::new(),
            parallelizable: None,
            test_file: String::new(),
            target_files: String::new(),
            candidates,
            chosen: HashSet::new(),
            focus: NAME,
            error: None,
        }
    }

    fn save_row(&self) -> usize {
        FIRST_DEPENDENCY + self.candidates.len()
    }

    fn cancel_row(&self) -> usize {
        self.save_row() + 1
    }

    /// The values to hand to the backend.
    pub(crate) fn fields(&self) -> FeatureFields {
        let text = |value: &str| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        };
        FeatureFields {
            name: self.name.trim().to_string(),
            description: text(&self.description),
            depends_on: self.candidates.iter().filter(|c| self.chosen.contains(*c)).cloned().collect(),
            parallelizable: self.parallelizable,
            test_file: text(&self.test_file),
            target_files: self
                .target_files
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect(),
        }
    }

    fn text_mut(&mut self) -> Option<&mut String> {
        match self.focus {
            NAME => Some(&mut self.name),
            DESCRIPTION => Some(&mut self.description),
            TEST_FILE => Some(&mut self.test_file),
            TARGET_FILES => Some(&mut self.target_files),
            _ => None,
        }
    }

    /// Default → Yes → No → Default.
    fn cycle(&mut self, forward: bool) {
        self.parallelizable = match (self.parallelizable, forward) {
            (None, true) | (Some(false), false) => Some(true),
            (Some(true), true) | (None, false) => Some(false),
            (Some(false), true) | (Some(true), false) => None,
        };
    }

    fn toggle_dependency(&mut self) {
        let Some(name) = self.focus.checked_sub(FIRST_DEPENDENCY).and_then(|i| self.candidates.get(i)) else {
            return;
        };
        if !self.chosen.remove(name) {
            self.chosen.insert(name.clone());
        }
    }

    /// Acts on the focused row as Enter, Space or a click would.
    fn activate(&mut self) -> FormAction {
        if self.focus == self.save_row() {
            FormAction::Save
        } else if self.focus == self.cancel_row() {
            FormAction::Cancel
        } else {
            if self.focus == PARALLELIZABLE {
                self.cycle(true);
            } else {
                self.toggle_dependency();
            }
            FormAction::None
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> FormAction {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = self.cancel_row();
        match key.code {
            KeyCode::Esc => return FormAction::Cancel,
            KeyCode::Char('s') if control => return FormAction::Save,
            KeyCode::Down | KeyCode::Tab => self.focus = (self.focus + 1).min(last),
            KeyCode::Up | KeyCode::BackTab => self.focus = self.focus.saturating_sub(1),
            KeyCode::Right if self.focus == PARALLELIZABLE => self.cycle(true),
            KeyCode::Left if self.focus == PARALLELIZABLE => self.cycle(false),
            KeyCode::Enter => {
                if self.text_mut().is_some() {
                    self.focus = (self.focus + 1).min(last);
                } else {
                    return self.activate();
                }
            }
            KeyCode::Char(' ') if self.text_mut().is_none() => return self.activate(),
            KeyCode::Char(c) if !control && !c.is_control() => {
                if let Some(text) = self.text_mut() {
                    text.push(c);
                    self.error = None;
                }
            }
            KeyCode::Backspace => {
                if let Some(text) = self.text_mut() {
                    text.pop();
                    self.error = None;
                }
            }
            _ => {}
        }
        FormAction::None
    }

    /// Rows available for fields once the error and hint rows are kept.
    fn viewport_rows(area: Rect) -> usize {
        usize::from(area.height.saturating_sub(2))
    }

    fn focus_line(focus: usize) -> usize {
        if focus < FIRST_DEPENDENCY {
            FIELD_LINES_FROM + focus
        } else {
            FIELD_LINES_FROM + focus + 1
        }
    }

    /// The first line shown: just enough scrolling to keep the focus in view.
    fn top(&self, rows: usize) -> usize {
        (Self::focus_line(self.focus) + 1).saturating_sub(rows)
    }

    /// A press at (`column`, `row`) inside `area`: focuses and acts on a row.
    pub(crate) fn click(&mut self, area: Rect, column: u16, row: u16) -> FormAction {
        let rows = Self::viewport_rows(area);
        if column < area.x || column >= area.right() || row < area.y {
            return FormAction::None;
        }
        let at = usize::from(row - area.y);
        if at >= rows {
            return FormAction::None;
        }
        let line = self.top(rows) + at;
        let focus = match line {
            l if (FIELD_LINES_FROM..DEPENDS_HEADER_LINE).contains(&l) => l - FIELD_LINES_FROM,
            l if l > DEPENDS_HEADER_LINE => l - FIELD_LINES_FROM - 1,
            _ => return FormAction::None,
        };
        if focus > self.cancel_row() {
            return FormAction::None;
        }
        self.focus = focus;
        if self.text_mut().is_some() {
            FormAction::None
        } else {
            self.activate()
        }
    }

    fn lines(&self) -> Vec<(String, bool)> {
        let field = |index: usize, label: &str, value: &str| {
            let focused = self.focus == index;
            let cursor = if focused { "_" } else { "" };
            let marker = if focused { "> " } else { "  " };
            (format!("{marker}{label:<15}{}{cursor}", printable(value)), focused)
        };
        let mut lines = vec![
            ("Add Feature".to_string(), false),
            ("  Status: draft (fixed)".to_string(), false),
            field(NAME, "Name:", &self.name),
            field(DESCRIPTION, "Description:", &self.description),
        ];
        let parallel = match self.parallelizable {
            None => "< Default >",
            Some(true) => "< Yes >",
            Some(false) => "< No >",
        };
        lines.push(field(PARALLELIZABLE, "Parallelizable:", parallel));
        lines.push(field(TEST_FILE, "Test file:", &self.test_file));
        lines.push(field(TARGET_FILES, "Target files:", &self.target_files));
        lines.push(("  Depends on:".to_string(), false));
        for (i, name) in self.candidates.iter().enumerate() {
            let focused = self.focus == FIRST_DEPENDENCY + i;
            let marker = if focused { "> " } else { "  " };
            let tick = if self.chosen.contains(name) { "x" } else { " " };
            lines.push((format!("{marker}  [{tick}] {}", printable(name)), focused));
        }
        for (index, label) in [(self.save_row(), "[ Save ]"), (self.cancel_row(), "[ Cancel ]")] {
            let focused = self.focus == index;
            let marker = if focused { "> " } else { "  " };
            lines.push((format!("{marker}{label}"), focused));
        }
        lines
    }

    pub(crate) fn render(&self, frame: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let width = usize::from(area.width);
        let rows = Self::viewport_rows(area);
        let top = self.top(rows);
        for (at, (text, focused)) in self.lines().into_iter().skip(top).take(rows).enumerate() {
            let style = match (at + top, focused) {
                (_, true) => Style::new().add_modifier(Modifier::BOLD),
                (0, _) => Style::new().add_modifier(Modifier::BOLD),
                _ => Style::new(),
            };
            let rect = Rect::new(area.x, area.y + u16::try_from(at).unwrap_or(0), area.width, 1);
            frame.render_widget(Paragraph::new(Span::styled(truncate_right(&text, width), style)), rect);
        }
        if area.height >= 2 {
            if let Some(error) = &self.error {
                let rect = Rect::new(area.x, area.bottom() - 2, area.width, 1);
                let text = truncate_right(&format!(" {}", printable(error)), width);
                frame.render_widget(Paragraph::new(Span::styled(text, Style::new().fg(Color::Red))), rect);
            }
        }
        let rect = Rect::new(area.x, area.bottom() - 1, area.width, 1);
        let hint = Line::from(truncate_right(HINT, width)).style(Style::new().fg(Color::DarkGray));
        frame.render_widget(Paragraph::new(hint), rect);
    }
}
