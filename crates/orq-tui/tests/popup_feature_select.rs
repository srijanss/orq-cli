use orq_tui::{render_feature_list, FeatureListState, FeatureRow, PopupModel};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::Terminal;

fn rows() -> Vec<FeatureRow> {
    ["alpha", "beta", "gamma"]
        .iter()
        .map(|n| FeatureRow { name: (*n).into(), status: "draft".into() })
        .collect()
}

fn click(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

const AREA: Rect = Rect { x: 0, y: 0, width: 40, height: 8 };

fn render(state: &FeatureListState) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
    terminal
        .draw(|frame| render_feature_list(frame, &model, &rows(), state))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..8)
        .map(|y| (0..40).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

#[test]
fn down_and_up_move_the_selection_and_stop_at_the_ends() {
    let mut state = FeatureListState::default();
    assert_eq!(state.selected, None);

    state.handle_key(KeyCode::Down, 3);
    assert_eq!(state.selected, Some(0));
    state.handle_key(KeyCode::Down, 3);
    state.handle_key(KeyCode::Down, 3);
    state.handle_key(KeyCode::Down, 3);
    assert_eq!(state.selected, Some(2));
    state.handle_key(KeyCode::Up, 3);
    assert_eq!(state.selected, Some(1));
    state.handle_key(KeyCode::Up, 3);
    state.handle_key(KeyCode::Up, 3);
    assert_eq!(state.selected, Some(0));
}

#[test]
fn a_left_click_on_a_row_selects_it_and_it_is_shown_as_selected() {
    let mut state = FeatureListState::default();

    state.handle_mouse(AREA, 3, click(5, 3));

    assert_eq!(state.selected, Some(1));
    let lines = render(&state);
    assert!(lines[3].starts_with("> ") && lines[3].contains("beta"), "{lines:?}");
    assert!(lines[2].starts_with("  ") && lines[4].starts_with("  "), "{lines:?}");
}

#[test]
fn a_click_outside_the_list_changes_nothing() {
    let mut state = FeatureListState { selected: Some(1) };

    state.handle_mouse(AREA, 3, click(5, 0)); // header
    state.handle_mouse(AREA, 3, click(5, 6)); // below the last row
    state.handle_mouse(AREA, 3, click(60, 3)); // right of the popup

    assert_eq!(state.selected, Some(1));
}

#[test]
fn selection_is_never_set_for_an_empty_list() {
    let mut state = FeatureListState::default();

    state.handle_key(KeyCode::Down, 0);
    state.handle_mouse(AREA, 0, click(5, 2));

    assert_eq!(state.selected, None);
}

#[test]
fn a_selection_beyond_a_shrunken_list_is_clamped_or_cleared() {
    let mut state = FeatureListState { selected: Some(9) };
    state.handle_key(KeyCode::Up, 3);
    assert_eq!(state.selected, Some(1));

    let mut state = FeatureListState { selected: Some(9) };
    state.handle_key(KeyCode::Down, 3);
    assert_eq!(state.selected, Some(2));

    let mut state = FeatureListState { selected: Some(2) };
    state.handle_key(KeyCode::Down, 0);
    assert_eq!(state.selected, None);

    let mut state = FeatureListState { selected: Some(2) };
    state.handle_mouse(AREA, 0, click(5, 2));
    assert_eq!(state.selected, None);
}

fn many_rows(n: usize) -> Vec<FeatureRow> {
    (0..n).map(|i| FeatureRow { name: format!("f{i}"), status: "draft".into() }).collect()
}

fn draw(width: u16, height: u16, rows: &[FeatureRow], state: &FeatureListState) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render_feature_list(frame, &model, rows, state)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

#[test]
fn a_selection_below_the_visible_rows_stays_marked_and_clickable() {
    // 5 rows high: 2 header rows leave 3 list rows for 10 features.
    let rows = many_rows(10);
    let state = FeatureListState { selected: Some(7) };

    let lines = draw(40, 5, &rows, &state);

    let marked: Vec<_> = lines.iter().filter(|l| l.starts_with("> ")).collect();
    assert_eq!(marked.len(), 1, "{lines:?}");
    assert!(marked[0].contains("f7"), "{lines:?}");

    // Clicking the first visible list row selects the feature shown there.
    let mut clicked = FeatureListState { selected: Some(7) };
    clicked.handle_mouse(Rect::new(0, 0, 40, 5), 10, click(3, 2));
    assert_eq!(clicked.selected, Some(5));
}

#[test]
fn a_two_column_popup_shows_the_marker_and_one_column_keeps_it_distinguishable() {
    let rows = many_rows(2);
    let selected = FeatureListState { selected: Some(0) };
    let none = FeatureListState::default();

    let wide = draw(2, 4, &rows, &selected);
    assert_eq!(wide[2], ">");
    let one_sel = draw(1, 4, &rows, &selected);
    let one_none = draw(1, 4, &rows, &none);
    assert_eq!(one_sel[2], ">");
    assert_ne!(one_sel[2], one_none[2]);
    // Zero columns must not panic.
    let _ = draw(0, 4, &rows, &selected);
}
