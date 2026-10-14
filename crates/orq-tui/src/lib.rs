use std::error::Error;
use std::io;

use ratatui::backend::Backend;
use ratatui::crossterm::event::{Event, KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use unicode_segmentation::UnicodeSegmentation;

/// Everything the popup header needs, as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupModel {
    pub project_name: String,
    pub repo_path: String,
}

/// A clickable part of the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    ProjectName,
    RepoPath,
}

/// Interactive state of the popup.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PopupState {
    pub selected: Option<Region>,
}

impl PopupState {
    /// Select the region under a left-button press.
    pub fn handle_mouse(&mut self, area: Rect, model: &PopupModel, mouse: MouseEvent) {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }
        if let Some(region) = hit_test(area, model, mouse.column, mouse.row) {
            self.selected = Some(region);
        }
    }
}

/// The rectangle each clickable region occupies, as rendered in `area`.
fn layout(area: Rect, model: &PopupModel) -> [(Region, Rect); 2] {
    let max = usize::from(area.width);
    let row = |index: u16, text: &str| {
        let width = u16::try_from(display_width(&truncate_left(text, max))).unwrap_or(u16::MAX);
        // Clip to the popup: rows the area is too short to render are not clickable.
        Rect::new(area.x, area.y.saturating_add(index), width, 1).intersection(area)
    };
    [
        (Region::ProjectName, row(0, &model.project_name)),
        (Region::RepoPath, row(1, &model.repo_path)),
    ]
}

/// The region under the cell at `column`, `row`, if any.
pub fn hit_test(area: Rect, model: &PopupModel, column: u16, row: u16) -> Option<Region> {
    layout(area, model)
        .into_iter()
        .find(|(_, rect)| rect.contains((column, row).into()))
        .map(|(region, _)| region)
}

/// Where the popup's input events come from (the real terminal, or a script).
pub trait EventSource {
    fn next_event(&mut self) -> io::Result<Event>;
}

/// Puts the terminal into popup mode and back (raw mode, alternate screen...).
pub trait TerminalMode {
    fn enter(&mut self) -> io::Result<()>;
    fn leave(&mut self) -> io::Result<()>;
}

/// Run the popup between `enter` and `leave`. The terminal is always left
/// again once entered, whether the popup closed normally or failed.
pub fn run_popup_session<B: Backend>(
    mode: &mut dyn TerminalMode,
    terminal: &mut Terminal<B>,
    model: &PopupModel,
    events: &mut dyn EventSource,
) -> Result<(), Box<dyn Error>>
where
    B::Error: 'static,
{
    mode.enter()?;
    let result = run_popup(terminal, model, events);
    let restored = mode.leave();
    result?;
    restored?;
    Ok(())
}

/// Draw the popup and process events until Esc is pressed.
pub fn run_popup<B: Backend>(
    terminal: &mut Terminal<B>,
    model: &PopupModel,
    events: &mut dyn EventSource,
) -> Result<(), Box<dyn Error>>
where
    B::Error: 'static,
{
    loop {
        terminal.draw(|frame| render_popup(frame, model))?;
        if let Event::Key(key) = events.next_event()? {
            if key.code == KeyCode::Esc {
                return Ok(());
            }
        }
    }
}

pub fn render_popup(frame: &mut Frame, model: &PopupModel) {
    let area = frame.area();
    let width = usize::from(area.width);
    let header = Paragraph::new(vec![
        Line::from(truncate_left(&model.project_name, width)),
        Line::from(truncate_left(&model.repo_path, width)),
    ]);
    frame.render_widget(header, area);
}

fn display_width(text: &str) -> usize {
    Span::raw(text).width()
}

/// Keep the tail of `text` within `max` display columns, marking the cut with
/// an ellipsis. The tail of a path is the part that tells repos apart. Whole
/// grapheme clusters are kept or dropped, never split.
fn truncate_left(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut budget = max - 1; // room for the ellipsis
    let mut tail: Vec<&str> = Vec::new();
    for cluster in text.graphemes(true).rev() {
        let w = display_width(cluster);
        if w > budget {
            break;
        }
        budget -= w;
        tail.push(cluster);
    }
    tail.reverse();
    format!("…{}", tail.concat())
}
