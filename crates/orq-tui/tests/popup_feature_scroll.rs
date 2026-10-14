use orq_tui::{render_feature_list, FeatureListState, FeatureRow, PopupModel};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::Terminal;

// 5 rows high: two header rows leave three list rows.
const AREA: Rect = Rect { x: 0, y: 0, width: 40, height: 5 };

fn many_rows(n: usize) -> Vec<FeatureRow> {
    (0..n).map(|i| FeatureRow { name: format!("f{i}"), status: "draft".into() }).collect()
}

fn draw(rows: &[FeatureRow], state: &FeatureListState) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
    terminal.draw(|frame| render_feature_list(frame, &model, rows, state)).unwrap();
    let buffer = terminal.backend().buffer();
    (2..5)
        .map(|y| (0..40).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn wheel(kind: MouseEventKind) -> MouseEvent {
    MouseEvent { kind, column: 5, row: 3, modifiers: KeyModifiers::NONE }
}

fn click(row: u16) -> MouseEvent {
    MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 5, row, modifiers: KeyModifiers::NONE }
}

#[test]
fn the_mouse_wheel_scrolls_one_row_at_a_time_and_stops_at_both_ends() {
    let rows = many_rows(10);
    let mut state = FeatureListState::default();

    state.handle_mouse(AREA, rows.len(), wheel(MouseEventKind::ScrollDown));
    assert_eq!(draw(&rows, &state), ["  f1 draft", "  f2 draft", "  f3 draft"]);

    for _ in 0..20 {
        state.handle_mouse(AREA, rows.len(), wheel(MouseEventKind::ScrollDown));
    }
    assert_eq!(draw(&rows, &state), ["  f7 draft", "  f8 draft", "  f9 draft"]);

    for _ in 0..20 {
        state.handle_mouse(AREA, rows.len(), wheel(MouseEventKind::ScrollUp));
    }
    assert_eq!(draw(&rows, &state), ["  f0 draft", "  f1 draft", "  f2 draft"]);
}

#[test]
fn keyboard_movement_scrolls_so_the_selected_row_stays_in_view() {
    let rows = many_rows(10);
    let mut state = FeatureListState::default();

    for _ in 0..5 {
        state.handle_key(KeyCode::Down, AREA, rows.len());
    }

    assert_eq!(state.selected, Some(4));
    assert_eq!(draw(&rows, &state), ["  f2 draft", "  f3 draft", "> f4 draft"]);

    for _ in 0..4 {
        state.handle_key(KeyCode::Up, AREA, rows.len());
    }
    assert_eq!(draw(&rows, &state), ["> f0 draft", "  f1 draft", "  f2 draft"]);
}

#[test]
fn scrolling_with_the_wheel_never_leaves_the_selection_out_of_view() {
    let rows = many_rows(10);
    let mut state = FeatureListState::default();
    state.handle_key(KeyCode::Down, AREA, rows.len());
    assert_eq!(state.selected, Some(0));

    for _ in 0..3 {
        state.handle_mouse(AREA, rows.len(), wheel(MouseEventKind::ScrollDown));
    }

    assert_eq!(state.selected, Some(3));
    assert_eq!(draw(&rows, &state), ["> f3 draft", "  f4 draft", "  f5 draft"]);
}

#[test]
fn a_click_after_scrolling_selects_the_row_shown_there() {
    let rows = many_rows(10);
    let mut state = FeatureListState::default();
    for _ in 0..4 {
        state.handle_mouse(AREA, rows.len(), wheel(MouseEventKind::ScrollDown));
    }

    state.handle_mouse(AREA, rows.len(), click(3));

    assert_eq!(state.selected, Some(5));
    assert_eq!(draw(&rows, &state), ["  f4 draft", "> f5 draft", "  f6 draft"]);
}

#[test]
fn a_list_that_fits_does_not_scroll() {
    let rows = many_rows(2);
    let mut state = FeatureListState::default();

    state.handle_mouse(AREA, rows.len(), wheel(MouseEventKind::ScrollDown));

    assert_eq!(draw(&rows, &state), ["  f0 draft", "  f1 draft", ""]);
}

fn wheel_at(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
}

#[test]
fn a_wheel_over_the_header_or_outside_the_popup_is_ignored() {
    let rows = many_rows(10);
    let mut state = FeatureListState { selected: Some(1), offset: 1 };
    let before = state.clone();

    for (column, row) in [(5, 0), (5, 1), (45, 3), (5, 9)] {
        for kind in [MouseEventKind::ScrollDown, MouseEventKind::ScrollUp] {
            state.handle_mouse(AREA, rows.len(), wheel_at(kind, column, row));
            assert_eq!(state, before, "({column},{row}) {kind:?}");
        }
    }
}

#[test]
fn a_scrolled_list_that_shrinks_or_empties_stays_valid_and_clickable() {
    let mut state = FeatureListState { selected: Some(8), offset: 7 };

    // Shrinks to 4 rows: the offset and selection are pulled back in range.
    let rows = many_rows(4);
    state.handle_mouse(AREA, rows.len(), wheel(MouseEventKind::ScrollUp));
    assert!(state.offset <= 1, "{state:?}");
    assert!(state.selected.unwrap() < 4, "{state:?}");
    state.handle_mouse(AREA, rows.len(), click(2));
    let shown = draw(&rows, &state);
    assert!(shown[state.selected.unwrap() - state.offset].starts_with("> "), "{shown:?}");

    // Empties: nothing is selected and nothing panics.
    state.handle_mouse(AREA, 0, wheel(MouseEventKind::ScrollDown));
    assert_eq!(state, FeatureListState::default());
}

#[test]
fn tiny_viewports_never_panic_and_keep_the_offset_valid() {
    let rows = many_rows(10);
    for height in 0..=3u16 {
        let area = Rect { x: 0, y: 0, width: 40, height };
        let mut state = FeatureListState { selected: Some(6), offset: 5 };
        for kind in [MouseEventKind::ScrollDown, MouseEventKind::ScrollUp] {
            state.handle_mouse(area, rows.len(), wheel_at(kind, 5, 2));
            state.handle_key(KeyCode::Down, area, rows.len());
            state.handle_key(KeyCode::Up, area, rows.len());
            assert!(state.offset < rows.len(), "{height}: {state:?}");
            assert!(state.selected.map_or(true, |i| i < rows.len()), "{height}: {state:?}");
        }
        state.handle_mouse(area, rows.len(), click(2));
    }
}
