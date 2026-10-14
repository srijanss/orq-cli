use orq_tui::{search_area, FeatureListState, FeatureRow, PopupModel, SearchState};
use orq_tui::render_feature_search;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::Terminal;

fn opened() -> SearchState {
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    search
}

#[test]
fn the_list_area_is_three_rows_shorter_while_the_box_is_shown() {
    let area = Rect::new(0, 0, 60, 20);

    let shown = search_area(area, &opened());

    assert_eq!((shown.y, shown.height), (3, 17));
    assert_eq!(search_area(area, &SearchState::default()), area);
}

#[test]
fn the_first_row_is_drawn_where_the_shifted_area_says_it_starts() {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let rows = vec![FeatureRow { name: "alpha-feature".into(), status: "pending".into() }];
    let items = vec![("alpha-feature".to_string(), None)];
    let search = opened();
    let area = Rect::new(0, 0, 60, 20);
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    terminal
        .draw(|f| render_feature_search(f, &model, &rows, &items, &search, &FeatureListState::default()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let row_of = |needle: &str| {
        (0..buffer.area.height).find(|&y| {
            (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().contains(needle)
        })
    };

    let list_top = search_area(area, &search).y + 2; // below the two header rows
    let search_row = row_of("Search by").expect("search box drawn");
    // The first card's top border is the first rounded corner below the box.
    let card_top = (search_row + 2..buffer.area.height)
        .find(|&y| (0..buffer.area.width).any(|x| buffer[(x, y)].symbol() == "╭"))
        .expect("card border drawn");
    assert_eq!(card_top, list_top, "the first row is drawn exactly where hit-testing starts the list");
    assert!(row_of("alpha-feature").expect("row drawn") > search_row);
}
