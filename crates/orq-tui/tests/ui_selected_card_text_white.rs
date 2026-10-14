use orq_tui::{render_feature_list, FeatureListState, FeatureRow, PopupModel};
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;

fn row(name: &str, status: &str) -> FeatureRow {
    FeatureRow { name: name.into(), status: status.into() }
}

fn draw(width: u16, height: u16, selected: Option<usize>) -> ratatui::buffer::Buffer {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let rows = [row("alpha", "draft"), row("beta", "pending")];
    let state = FeatureListState { selected, offset: 0 };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render_feature_list(frame, &model, &rows, &state)).unwrap();
    terminal.backend().buffer().clone()
}

/// Foreground of the first cell of `needle` found on any line, if drawn.
fn fg_of(buffer: &ratatui::buffer::Buffer, needle: &str) -> Vec<Color> {
    let (w, h) = (buffer.area.width, buffer.area.height);
    let mut found = Vec::new();
    for y in 0..h {
        let cells: Vec<String> = (0..w).map(|x| buffer[(x, y)].symbol().to_string()).collect();
        let line: String = cells.concat();
        if let Some(byte) = line.find(needle) {
            // Borders are multi-byte, so convert the byte offset to a cell.
            let x = cells.iter().scan(0, |acc, c| { let at = *acc; *acc += c.len(); Some(at) }).position(|at| at == byte).unwrap();
            found.push(buffer[(u16::try_from(x).unwrap(), y)].fg);
        }
    }
    found
}

#[test]
fn the_selected_card_name_and_dependency_line_are_white() {
    let buffer = draw(40, 20, Some(0));

    assert_eq!(fg_of(&buffer, "alpha"), vec![Color::White]);
    assert_eq!(fg_of(&buffer, "#1"), vec![Color::White]);
}

#[test]
fn unselected_cards_keep_the_default_name_and_dim_summary() {
    let buffer = draw(40, 20, Some(0));

    assert_eq!(fg_of(&buffer, "beta"), vec![Color::Reset]);
    assert_eq!(fg_of(&buffer, "#2"), vec![Color::DarkGray]);
}

#[test]
fn the_selected_one_row_name_is_white_too() {
    let buffer = draw(40, 6, Some(1));

    assert_eq!(fg_of(&buffer, "beta"), vec![Color::White]);
    assert_eq!(fg_of(&buffer, "alpha"), vec![Color::Reset]);
}
