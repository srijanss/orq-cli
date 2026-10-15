use ratatui::crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{printable, truncate_right};

/// What the generation agent proposed for one feature, as the preview shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateView {
    pub name: String,
    pub description: Option<String>,
    pub depends_on: Vec<String>,
    pub kind: CandidateKind,
    /// The agent proposed `parallelizable: true`; only a suggestion.
    pub suggests_parallel: bool,
    /// Fields the agent set that Orq ignores (status, recordedAt, ...).
    pub ignored_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateKind {
    /// May be added.
    New,
    /// Already in the ledger or earlier in the same output; never added.
    Duplicate(String),
    /// Cannot be added; the reason is shown.
    Invalid(String),
}

/// A candidate the user chose to add. `parallelizable` is `Some(true)` only
/// when the agent suggested it and the user kept the suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedCandidate {
    pub name: String,
    pub parallelizable: Option<bool>,
}

pub(crate) enum PreviewAction {
    None,
    Cancel,
    Accept(Vec<AcceptedCandidate>),
}

struct Item {
    view: CandidateView,
    selected: bool,
    parallel: bool,
}

pub(crate) struct PreviewState {
    items: Vec<Item>,
    cursor: usize,
    /// Why the last accept did not save; the preview stays open for a retry.
    pub error: Option<String>,
}

const NOTE: &str = " Dependencies and scheduling safety are enforced later; parallel is only a suggestion.";
const HINT: &str = " ↑↓ move · Space select · p parallel · Enter add selected · x reject all · Esc cancel ";

impl PreviewState {
    pub fn new(candidates: Vec<CandidateView>) -> PreviewState {
        let items = candidates
            .into_iter()
            .map(|view| {
                let new = view.kind == CandidateKind::New;
                Item { selected: new, parallel: new && view.suggests_parallel, view }
            })
            .collect();
        PreviewState { items, cursor: 0, error: None }
    }

    fn toggle_selected(&mut self, index: usize) {
        if let Some(item) = self.items.get_mut(index) {
            if item.view.kind == CandidateKind::New {
                item.selected = !item.selected;
            }
        }
    }

    fn accepted(&self) -> Vec<AcceptedCandidate> {
        self.items
            .iter()
            .filter(|item| item.selected && item.view.kind == CandidateKind::New)
            .map(|item| AcceptedCandidate {
                name: item.view.name.clone(),
                parallelizable: item.parallel.then_some(true),
            })
            .collect()
    }

    pub fn handle_key(&mut self, code: KeyCode) -> PreviewAction {
        match code {
            KeyCode::Esc | KeyCode::Char('x') => return PreviewAction::Cancel,
            KeyCode::Down => self.cursor = (self.cursor + 1).min(self.items.len().saturating_sub(1)),
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char(' ') => self.toggle_selected(self.cursor),
            KeyCode::Char('p') => {
                if let Some(item) = self.items.get_mut(self.cursor) {
                    if item.view.suggests_parallel && item.view.kind == CandidateKind::New {
                        item.parallel = !item.parallel;
                    }
                }
            }
            KeyCode::Enter => {
                let accepted = self.accepted();
                if accepted.is_empty() {
                    self.error = Some("Select at least one new candidate".into());
                } else {
                    self.error = None;
                    return PreviewAction::Accept(accepted);
                }
            }
            _ => {}
        }
        PreviewAction::None
    }

    /// The rows of the list region: below the two header lines, above the two
    /// detail lines, the error line and the hint.
    fn list_rows(body: Rect) -> usize {
        usize::from(body.height.saturating_sub(6))
    }

    fn top(&self, rows: usize) -> usize {
        if rows == 0 {
            return 0;
        }
        self.cursor.saturating_sub(rows - 1)
    }

    /// A left click on a candidate row toggles it.
    pub fn handle_mouse(&mut self, body: Rect, mouse: MouseEvent) {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) || mouse.row < body.y + 2 {
            return;
        }
        let rows = Self::list_rows(body);
        let at = usize::from(mouse.row - body.y - 2);
        if at >= rows || !body.contains((mouse.column, mouse.row).into()) {
            return;
        }
        let index = self.top(rows) + at;
        if index < self.items.len() {
            self.cursor = index;
            self.toggle_selected(index);
        }
    }

    pub fn render(&self, frame: &mut Frame, body: Rect) {
        let width = usize::from(body.width);
        let dim = Style::new().fg(Color::DarkGray);
        let mut put = |row: u16, line: Line<'static>| {
            if row < body.height {
                frame.render_widget(Paragraph::new(line), Rect::new(body.x, body.y + row, body.width, 1));
            }
        };
        let count = |f: fn(&CandidateKind) -> bool| self.items.iter().filter(|i| f(&i.view.kind)).count();
        let new = count(|k| matches!(k, CandidateKind::New));
        let duplicate = count(|k| matches!(k, CandidateKind::Duplicate(_)));
        let invalid = count(|k| matches!(k, CandidateKind::Invalid(_)));
        let title = format!(" Generated candidates · {new} new, {duplicate} duplicate, {invalid} invalid");
        put(0, Line::from(Span::styled(truncate_right(&title, width), Style::new().add_modifier(Modifier::BOLD))));
        put(1, Line::from(Span::styled(truncate_right(NOTE, width), dim)));
        let rows = Self::list_rows(body);
        let top = self.top(rows);
        for (offset, item) in self.items.iter().enumerate().skip(top).take(rows) {
            let marker = if offset == self.cursor { ">" } else { " " };
            let name = printable(&item.view.name);
            let (box_, detail, style) = match &item.view.kind {
                CandidateKind::New => {
                    let mut detail = String::new();
                    if item.view.suggests_parallel {
                        detail.push_str(if item.parallel { " · parallel" } else { " · parallel removed" });
                    }
                    if !item.view.ignored_fields.is_empty() {
                        let ignored = printable(&item.view.ignored_fields.join(", "));
                        detail.push_str(&format!(" · ignored: {ignored}"));
                    }
                    (if item.selected { "[x]" } else { "[ ]" }, detail, Style::new())
                }
                CandidateKind::Duplicate(why) => ("[-]", format!(" · duplicate: {}", printable(why)), dim),
                CandidateKind::Invalid(why) => {
                    ("[!]", format!(" · invalid: {}", printable(why)), Style::new().fg(Color::Red))
                }
            };
            let line = format!(" {marker} {box_} {name}{detail}");
            let row = u16::try_from(2 + offset - top).unwrap_or(u16::MAX);
            put(row, Line::from(Span::styled(truncate_right(&line, width), style)));
        }
        if let Some(item) = self.items.get(self.cursor) {
            let description = item.view.description.as_deref().map(printable).unwrap_or_default();
            let depends = if item.view.depends_on.is_empty() {
                String::new()
            } else {
                format!(" depends on: {}", printable(&item.view.depends_on.join(", ")))
            };
            let base = body.height.saturating_sub(4);
            put(base, Line::from(Span::styled(truncate_right(&format!(" {description}"), width), dim)));
            put(base + 1, Line::from(Span::styled(truncate_right(&depends, width), dim)));
        }
        if let Some(error) = &self.error {
            let text = format!(" {}", printable(error));
            put(
                body.height.saturating_sub(2),
                Line::from(Span::styled(truncate_right(&text, width), Style::new().fg(Color::Red))),
            );
        }
        put(body.height.saturating_sub(1), Line::from(Span::styled(truncate_right(HINT, width), dim)));
    }
}
