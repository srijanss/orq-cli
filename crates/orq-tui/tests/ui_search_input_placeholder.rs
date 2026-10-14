use orq_tui::{render_feature_search, FeatureListState, FeatureRow, PopupModel, SearchState};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::KeyCode;
use ratatui::style::Modifier;
use ratatui::Terminal;

const PLACEHOLDER: &str = "Search by feature name";

fn frame(search: &SearchState, width: u16) -> Terminal<TestBackend> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let rows = vec![FeatureRow { name: "alpha".into(), status: "draft".into() }];
    let items = vec![("alpha".to_string(), None)];
    let mut terminal = Terminal::new(TestBackend::new(width, 10)).unwrap();
    terminal
        .draw(|f| render_feature_search(f, &model, &rows, &items, search, &FeatureListState::default()))
        .unwrap();
    terminal
}

fn text(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
        .map(|(x, y)| buffer[(x, y)].symbol().to_string())
        .collect()
}

#[test]
fn an_open_empty_field_shows_a_dim_placeholder_that_vanishes_on_typing() {
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    let terminal = frame(&search, 60);
    let shown = text(&terminal);
    assert!(shown.contains(PLACEHOLDER), "{shown}");

    // The placeholder is dim so it reads as a hint, not as a query.
    let buffer = terminal.backend().buffer();
    let dim = (0..buffer.area.height).any(|y| {
        (0..buffer.area.width).any(|x| {
            buffer[(x, y)].symbol() == "S" && buffer[(x, y)].modifier.contains(Modifier::DIM)
        })
    });
    assert!(dim);

    search.handle_key(KeyCode::Char('a'));
    assert!(!text(&frame(&search, 60)).contains("Search by"));
}

#[test]
fn a_narrow_field_truncates_the_placeholder_to_one_row() {
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    let terminal = frame(&search, 20);
    let buffer = terminal.backend().buffer();
    let rows_with_hint = (0..buffer.area.height)
        .filter(|&y| {
            (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().contains("Search by")
        })
        .count();
    assert_eq!(rows_with_hint, 1);
}
