use orq_tui::{render_feature_search, FeatureListState, FeatureRow, PopupModel, SearchState};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::KeyCode;
use ratatui::Terminal;

fn lines(query: &str) -> String {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let rows = vec![
        FeatureRow { name: "alpha".into(), status: "pending".into() },
        FeatureRow { name: "beta".into(), status: "completed".into() },
        FeatureRow { name: "gamma".into(), status: "Pending".into() },
    ];
    let items = vec![("alpha".to_string(), None), ("beta".to_string(), None), ("gamma".to_string(), None)];
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    for c in query.chars() {
        search.handle_key(KeyCode::Char(c));
    }
    let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
    terminal
        .draw(|f| render_feature_search(f, &model, &rows, &items, &search, &FeatureListState::default()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_query_no_longer_matches_the_feature_status() {
    let shown = lines("PENDING");
    assert!(shown.contains("no matches"), "{shown}");
}

#[test]
fn a_query_still_matches_the_feature_name_ignoring_case() {
    let shown = lines("ALPH");
    assert!(shown.contains("alpha"), "{shown}");
    assert!(!shown.contains("beta") && !shown.contains("gamma"), "{shown}");
}
