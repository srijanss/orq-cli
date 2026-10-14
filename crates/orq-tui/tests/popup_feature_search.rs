use orq_tui::{
    matching_features, render_feature_search, search_area, FeatureListState, FeatureRow, PopupModel, SearchKey,
    SearchState,
};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::Terminal;

fn items() -> Vec<(String, Option<String>)> {
    vec![
        ("alpha-list".into(), Some("Shows the Feature list".into())),
        ("beta-detail".into(), None),
        ("gamma".into(), Some("detail pane for ÉCLAIR".into())),
    ]
}

fn rows() -> Vec<FeatureRow> {
    ["alpha-list", "beta-detail", "gamma"]
        .iter()
        .map(|n| FeatureRow { name: (*n).into(), status: "draft".into() })
        .collect()
}

fn type_str(search: &mut SearchState, text: &str) {
    for c in text.chars() {
        search.handle_key(KeyCode::Char(c));
    }
}

fn draw(search: &SearchState, state: &FeatureListState, height: u16) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(40, height)).unwrap();
    terminal
        .draw(|frame| render_feature_search(frame, &model, &rows(), &items(), search, state))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..40).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

#[test]
fn an_empty_query_matches_every_feature_in_order() {
    assert_eq!(matching_features(&items(), ""), vec![0, 1, 2]);
}

#[test]
fn matching_is_case_insensitive_over_name_and_description() {
    assert_eq!(matching_features(&items(), "DETAIL"), vec![1, 2]); // name, then description
    assert_eq!(matching_features(&items(), "feature list"), vec![0]);
    assert_eq!(matching_features(&items(), "éclair"), vec![2]); // non-ASCII folds too
    assert!(matching_features(&items(), "zzz").is_empty());
}

#[test]
fn slash_opens_the_field_and_typing_and_backspace_edit_the_query() {
    let mut search = SearchState::default();
    assert!(!search.active);

    assert_eq!(search.handle_key(KeyCode::Char('/')), SearchKey::Handled);
    assert!(search.active);
    type_str(&mut search, "ab");
    assert_eq!(search.query, "ab");
    search.handle_key(KeyCode::Backspace);
    assert_eq!(search.query, "a");
}

#[test]
fn slash_is_not_typed_into_the_query_and_keys_when_idle_fall_through() {
    let mut search = SearchState::default();
    assert_eq!(search.handle_key(KeyCode::Down), SearchKey::Ignored);
    assert_eq!(search.handle_key(KeyCode::Char('x')), SearchKey::Ignored);
    assert!(search.query.is_empty());

    search.handle_key(KeyCode::Char('/'));
    assert_eq!(search.query, "");
    // While typing, navigation keys still reach the list.
    assert_eq!(search.handle_key(KeyCode::Down), SearchKey::Ignored);
}

#[test]
fn the_first_escape_clears_the_search_and_only_the_second_closes() {
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    type_str(&mut search, "det");

    assert_eq!(search.handle_key(KeyCode::Esc), SearchKey::Handled);
    assert!(!search.active);
    assert!(search.query.is_empty());
    assert_eq!(search.handle_key(KeyCode::Esc), SearchKey::Close);
}

#[test]
fn escape_clears_an_applied_filter_after_enter_before_closing() {
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    type_str(&mut search, "det");
    assert_eq!(search.handle_key(KeyCode::Enter), SearchKey::Handled);
    assert!(!search.active);
    assert_eq!(search.query, "det");

    assert_eq!(search.handle_key(KeyCode::Esc), SearchKey::Handled);
    assert!(search.query.is_empty());
    assert_eq!(search.handle_key(KeyCode::Esc), SearchKey::Close);
}

#[test]
fn an_opened_but_empty_field_is_closed_by_one_escape_then_the_popup_by_the_next() {
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));

    assert_eq!(search.handle_key(KeyCode::Esc), SearchKey::Handled);
    assert!(!search.active);
    assert_eq!(search.handle_key(KeyCode::Esc), SearchKey::Close);
}

#[test]
fn the_list_shows_only_matching_rows_and_the_field_with_its_query() {
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    type_str(&mut search, "detail");
    let state = FeatureListState { selected: Some(0), ..Default::default() };

    let lines = draw(&search, &state, 8);
    let text = lines.join("\n");

    assert!(text.contains("beta-detail") && text.contains("gamma"), "{lines:?}");
    assert!(!text.contains("alpha-list"), "{lines:?}");
    // The field is the bordered box below the header: query on its middle row.
    assert!(lines[3].starts_with("│/detail") && lines[3].ends_with('│'), "{lines:?}");
    // Selection indexes the filtered rows: row 0 is beta-detail.
    let marked: Vec<_> = lines.iter().filter(|l| l.starts_with("> ")).collect();
    assert_eq!(marked.len(), 1, "{lines:?}");
    assert!(marked[0].contains("beta-detail"), "{lines:?}");
}

#[test]
fn no_matches_is_stated_and_no_row_is_marked() {
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    type_str(&mut search, "zzz");
    let state = FeatureListState { selected: Some(1), ..Default::default() };

    let lines = draw(&search, &state, 8);

    assert!(lines.iter().any(|l| l.contains("no matches")), "{lines:?}");
    assert!(lines.iter().all(|l| !l.starts_with("> ")), "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains("alpha-list")), "{lines:?}");
}

#[test]
fn without_a_search_the_full_list_is_shown_and_no_field_row_is_drawn() {
    let search = SearchState::default();
    let lines = draw(&search, &FeatureListState::default(), 6);
    let text = lines.join("\n");

    assert!(text.contains("alpha-list") && text.contains("beta-detail") && text.contains("gamma"), "{lines:?}");
    assert!(lines[2..].iter().all(|l| !l.starts_with('/')), "{lines:?}");
}

#[test]
fn control_characters_in_the_query_are_never_drawn_raw_and_tiny_viewports_do_not_panic() {
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    type_str(&mut search, "a\u{1b}[31m\u{7}");
    let lines = draw(&search, &FeatureListState::default(), 6);
    assert!(lines.iter().all(|l| !l.contains('\u{1b}') && !l.contains('\u{7}')), "{lines:?}");

    for height in [0, 1, 2, 3] {
        let _ = draw(&search, &FeatureListState::default(), height);
    }
}

fn click(column: u16, row: u16) -> MouseEvent {
    MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE }
}

fn many(n: usize) -> (Vec<FeatureRow>, Vec<(String, Option<String>)>) {
    let rows = (0..n).map(|i| FeatureRow { name: format!("f{i}"), status: "draft".into() }).collect();
    let items = (0..n).map(|i| (format!("f{i}"), None)).collect();
    (rows, items)
}

fn draw_many(
    rows: &[FeatureRow],
    items: &[(String, Option<String>)],
    search: &SearchState,
    state: &FeatureListState,
    height: u16,
) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(40, height)).unwrap();
    terminal.draw(|frame| render_feature_search(frame, &model, rows, items, search, state)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..40).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

#[test]
fn clicks_hit_the_rows_drawn_below_the_field_and_the_field_itself_selects_nothing() {
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    type_str(&mut search, "detail"); // beta-detail, gamma
    let full = Rect::new(0, 0, 40, 8);
    let area = search_area(full, &search);
    let mut state = FeatureListState::default();

    state.handle_mouse(area, 2, click(5, 3)); // inside the field box
    assert_eq!(state.selected, None);
    state.handle_mouse(area, 2, click(5, 6)); // second listed row
    assert_eq!(state.selected, Some(1));

    let lines = draw(&search, &state, 8);
    assert!(lines[6].starts_with("> ") && lines[6].contains("gamma"), "{lines:?}");
}

#[test]
fn without_a_field_the_search_area_is_the_full_area() {
    let full = Rect::new(0, 0, 40, 8);
    assert_eq!(search_area(full, &SearchState::default()), full);
}

#[test]
fn keyboard_navigation_scrolls_the_filtered_list_in_step_with_what_is_drawn() {
    let (rows, items) = many(12);
    let mut search = SearchState::default();
    search.handle_key(KeyCode::Char('/'));
    type_str(&mut search, "f"); // every row matches
    let height = 7; // header 2 + field 1 => 4 list rows
    let area = search_area(Rect::new(0, 0, 40, height), &search);
    let mut state = FeatureListState::default();

    for _ in 0..9 {
        state.handle_key(KeyCode::Down, area, 12);
    }
    assert_eq!(state.selected, Some(8));

    let lines = draw_many(&rows, &items, &search, &state, height);
    let marked: Vec<_> = lines.iter().filter(|l| l.starts_with("> ")).collect();
    assert_eq!(marked.len(), 1, "{lines:?}");
    assert!(marked[0].contains("f8"), "{lines:?}");

    // Clicking the marked row, wherever it is drawn, keeps that feature selected.
    let row = lines.iter().position(|l| l.starts_with("> ")).unwrap() as u16;
    state.handle_mouse(area, 12, click(3, row));
    assert_eq!(state.selected, Some(8));
    // The last drawn row is the one beneath the cursor.
    state.handle_mouse(area, 12, click(3, height - 1));
    let last = lines.last().unwrap();
    assert!(last.contains(&format!("f{}", state.selected.unwrap())), "{lines:?}");
}
