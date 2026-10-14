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
