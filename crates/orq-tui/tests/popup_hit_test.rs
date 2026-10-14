use orq_tui::{PopupModel, PopupState, Region};
use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

fn model() -> PopupModel {
    PopupModel {
        project_name: "orq-cli".into(),
        repo_path: "/home/dev/code/orq-cli".into(),
    }
}

fn click(button: MouseButton, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(button),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

const AREA: Rect = Rect { x: 0, y: 0, width: 40, height: 5 };

#[test]
fn left_click_inside_a_region_selects_it() {
    let mut state = PopupState::default();

    state.handle_mouse(AREA, &model(), click(MouseButton::Left, 2, 0));
    assert_eq!(state.selected, Some(Region::ProjectName));

    state.handle_mouse(AREA, &model(), click(MouseButton::Left, 5, 1));
    assert_eq!(state.selected, Some(Region::RepoPath));
}

#[test]
fn clicks_outside_every_region_or_with_other_buttons_do_nothing() {
    let mut state = PopupState::default();
    state.handle_mouse(AREA, &model(), click(MouseButton::Left, 2, 0));

    // Right of the 7-column project name, below the header, and outside the area.
    state.handle_mouse(AREA, &model(), click(MouseButton::Left, 30, 0));
    state.handle_mouse(AREA, &model(), click(MouseButton::Left, 2, 3));
    state.handle_mouse(AREA, &model(), click(MouseButton::Left, 60, 0));
    // Non-left button inside a region.
    state.handle_mouse(AREA, &model(), click(MouseButton::Right, 2, 1));

    assert_eq!(state.selected, Some(Region::ProjectName));
}
