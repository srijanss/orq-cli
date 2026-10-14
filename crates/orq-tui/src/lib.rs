use std::error::Error;
use std::io;

use ratatui::backend::Backend;
use ratatui::crossterm::event::{Event, KeyCode};
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
