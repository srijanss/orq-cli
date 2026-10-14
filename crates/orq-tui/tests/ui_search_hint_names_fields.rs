use orq_tui::{render_feature_search, FeatureListState, FeatureRow, PopupModel, SearchState};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::KeyCode;
use ratatui::Terminal;

fn screen(query: &str) -> String {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let rows = vec![
        FeatureRow { name: "login".into(), status: "pending".into() },
        FeatureRow { name: "export".into(), status: "completed".into() },
    ];
    let items = vec![("login".to_string(), None), ("export".to_string(), None)];
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    for c in query.chars() {
        search.handle_key(KeyCode::Char(c));
    }
    let mut terminal = Terminal::new(TestBackend::new(70, 12)).unwrap();
    terminal
        .draw(|f| render_feature_search(f, &model, &rows, &items, &search, &FeatureListState::default()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every field the placeholder names must really be searched, so the hint
/// cannot drift from the matching.
#[test]
fn each_field_the_placeholder_names_is_searchable() {
    let hint = screen("");
    assert!(hint.contains("feature name") && !hint.contains("status"), "{hint}");

    let by_name = screen("login");
    assert!(by_name.contains("login") && !by_name.contains("export"), "{by_name}");
    let by_status = screen("completed");
    assert!(by_status.contains("no matches"), "{by_status}");
}
