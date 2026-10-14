use orq_tui::{render_feature_search, FeatureListState, FeatureRow, PopupModel, SearchState};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::KeyCode;
use ratatui::Terminal;

fn lines(search: &SearchState, width: u16, height: u16) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let rows = vec![FeatureRow { name: "alpha".into(), status: "draft".into() }];
    let items = vec![("alpha".to_string(), None)];
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| render_feature_search(f, &model, &rows, &items, search, &FeatureListState::default()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>())
        .collect()
}

fn opened() -> SearchState {
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    search
}

#[test]
fn an_open_field_is_a_three_row_rounded_box_titled_search_holding_the_placeholder() {
    let out = lines(&opened(), 50, 14);
    let top = out.iter().position(|l| l.contains('╭') && l.contains("Search")).unwrap_or_else(|| panic!("{out:#?}"));

    assert!(out[top + 1].starts_with('│') && out[top + 1].trim_end().ends_with('│'), "{out:#?}");
    assert!(out[top + 1].contains("Search by feature name"), "{out:#?}");
    assert!(out[top + 2].starts_with('╰') && out[top + 2].trim_end().ends_with('╯'), "{out:#?}");
}

#[test]
fn a_typed_query_replaces_the_placeholder_inside_the_box() {
    let mut search = opened();
    search.handle_key(KeyCode::Char('a'));
    search.handle_key(KeyCode::Char('l'));
    let out = lines(&search, 50, 14);
    let top = out.iter().position(|l| l.contains('╭') && l.contains("Search")).unwrap_or_else(|| panic!("{out:#?}"));

    assert!(out[top + 1].contains("/al") && !out[top + 1].contains("Search by"), "{out:#?}");
}

#[test]
fn a_narrow_box_keeps_every_line_inside_the_terminal() {
    let out = lines(&opened(), 12, 14);

    assert!(out.iter().all(|l| l.chars().count() <= 12), "{out:#?}");
    assert!(out.iter().any(|l| l.contains('╭')), "{out:#?}");
}

#[test]
fn a_closed_search_draws_no_box() {
    let out = lines(&SearchState::default(), 50, 14);

    assert!(!out.iter().any(|l| l.contains("Search")), "{out:#?}");
}
