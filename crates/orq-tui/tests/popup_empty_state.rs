use orq_tui::{render_view, PopupModel, PopupView};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn screen(terminal: &Terminal<TestBackend>, width: u16, height: u16) -> String {
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn non_repository_shows_an_empty_state_and_never_a_previous_project() {
    let mut terminal = Terminal::new(TestBackend::new(50, 5)).unwrap();
    let project = PopupView::Project(PopupModel {
        project_name: "orq-cli".into(),
        repo_path: "/home/dev/code/orq-cli".into(),
    });
    let empty = PopupView::NotARepository {
        path: "/tmp/scratch".into(),
    };

    terminal.draw(|frame| render_view(frame, &project)).unwrap();
    assert!(screen(&terminal, 50, 5).contains("orq-cli"));

    terminal.draw(|frame| render_view(frame, &empty)).unwrap();
    let after = screen(&terminal, 50, 5);
    assert!(after.contains("not a Git repository"), "{after}");
    assert!(after.contains("/tmp/scratch"), "{after}");
    assert!(!after.contains("orq-cli"), "stale project shown: {after}");
}
