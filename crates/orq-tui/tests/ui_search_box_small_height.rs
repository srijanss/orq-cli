use orq_tui::{render_feature_search, search_area, FeatureListState, FeatureRow, PopupModel, SearchState};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::Terminal;

fn opened() -> SearchState {
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    search
}

fn lines(height: u16) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let rows = vec![FeatureRow { name: "alpha-feature".into(), status: "pending".into() }];
    let items = vec![("alpha-feature".to_string(), None)];
    let mut terminal = Terminal::new(TestBackend::new(60, height)).unwrap();
    terminal
        .draw(|f| render_feature_search(f, &model, &rows, &items, &opened(), &FeatureListState::default()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>())
        .collect()
}

#[test]
fn a_short_popup_falls_back_to_the_single_row_field() {
    let out = lines(6);

    assert!(!out.iter().any(|l| l.contains('╭') && l.contains("Search")), "{out:#?}");
    assert!(out.iter().any(|l| l.contains("Search by")), "{out:#?}");
    assert_eq!(search_area(Rect::new(0, 0, 60, 6), &opened()).height, 5);
}

#[test]
fn a_short_popup_still_shows_a_list_row() {
    let out = lines(6);

    assert!(out.iter().any(|l| l.contains("alpha-feature")), "{out:#?}");
}

#[test]
fn a_tall_enough_popup_uses_the_box() {
    let out = lines(8);

    assert!(out.iter().any(|l| l.contains('╭') && l.contains("Search")), "{out:#?}");
    assert_eq!(search_area(Rect::new(0, 0, 60, 8), &opened()).height, 5);
}
