use orq_tui::{PopupModel, PopupState, Region};
use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

fn model() -> PopupModel {
    PopupModel {
        project_name: "orq-cli".into(),
        repo_path: "/home/dev/code/orq-cli".into(),
    }
}

fn left_click(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn selected_after_click(area: Rect, column: u16, row: u16) -> Option<Region> {
    let mut state = PopupState::default();
    state.handle_mouse(area, &model(), left_click(column, row));
    state.selected
}

#[test]
fn regions_not_rendered_in_a_short_area_are_not_clickable() {
    let one_row = Rect::new(0, 0, 40, 1);
    assert_eq!(selected_after_click(one_row, 2, 0), Some(Region::ProjectName));
    assert_eq!(selected_after_click(one_row, 2, 1), None);

    let zero_rows = Rect::new(0, 0, 40, 0);
    assert_eq!(selected_after_click(zero_rows, 2, 0), None);
}

#[test]
fn hit_testing_respects_a_nonzero_area_origin() {
    let area = Rect::new(5, 3, 30, 4);

    assert_eq!(selected_after_click(area, 6, 3), Some(Region::ProjectName));
    assert_eq!(selected_after_click(area, 6, 4), Some(Region::RepoPath));
    assert_eq!(selected_after_click(area, 0, 0), None);
    assert_eq!(selected_after_click(area, 2, 3), None);
}
