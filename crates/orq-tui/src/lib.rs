use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Everything the popup header needs, as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupModel {
    pub project_name: String,
    pub repo_path: String,
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
/// an ellipsis. The tail of a path is the part that tells repos apart.
fn truncate_left(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut budget = max - 1; // room for the ellipsis
    let mut tail: Vec<char> = Vec::new();
    for ch in text.chars().rev() {
        let w = display_width(ch.encode_utf8(&mut [0; 4]));
        if w > budget {
            break;
        }
        budget -= w;
        tail.push(ch);
    }
    tail.reverse();
    format!("…{}", tail.into_iter().collect::<String>())
}
