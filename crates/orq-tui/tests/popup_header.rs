use orq_tui::{render_popup, PopupModel};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn render(model: &PopupModel, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render_popup(frame, model)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect())
        .collect()
}

fn model() -> PopupModel {
    PopupModel {
        project_name: "orq-cli".into(),
        repo_path: "/home/dev/code/orq-cli".into(),
    }
}

#[test]
fn header_shows_project_name_and_repository_path() {
    let lines = render(&model(), 60, 5).join("\n");

    assert!(lines.contains("orq-cli"), "{lines}");
    assert!(lines.contains("/home/dev/code/orq-cli"), "{lines}");
}

#[test]
fn narrow_terminal_truncates_the_path_but_keeps_the_name_and_its_tail() {
    let lines = render(&model(), 20, 3);

    assert!(lines[0].starts_with("orq-cli"), "{lines:?}");
    assert!(lines[1].contains('…'), "path should be truncated: {lines:?}");
    assert!(lines[1].trim_end().ends_with("orq-cli"), "keeps path tail: {lines:?}");
    assert!(lines[1].trim_end().chars().count() <= 20, "{lines:?}");
}
