use orq_tui::{
    promote_intent_for_key, promote_intent_for_mouse, promote_split, render_promote_button, FeatureRow, PromoteIntent,
};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::Terminal;

const AREA: Rect = Rect { x: 0, y: 0, width: 40, height: 8 };

fn row(name: &str, status: &str) -> FeatureRow {
    FeatureRow { name: name.into(), status: status.into() }
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
}

fn click(column: u16, row: u16) -> MouseEvent {
    mouse(MouseEventKind::Down(MouseButton::Left), column, row)
}

fn draw(width: u16, height: u16, selected: Option<&FeatureRow>) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| render_promote_button(frame, Rect::new(0, 0, width, height), selected))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

#[test]
fn the_button_appears_only_for_a_selected_draft() {
    let draft = row("alpha", "draft");
    assert!(promote_split(AREA, Some(&draft)).1.is_some());

    for status in ["pending", "completed", "in_progress", "unknown"] {
        assert!(promote_split(AREA, Some(&row("alpha", status))).1.is_none(), "{status}");
    }
    assert!(promote_split(AREA, None).1.is_none());
}

#[test]
fn the_button_takes_the_last_row_and_the_list_keeps_the_rest() {
    let draft = row("alpha", "draft");
    let (list, button) = promote_split(AREA, Some(&draft));
    let button = button.unwrap();

    assert_eq!(button.y, AREA.bottom() - 1);
    assert_eq!(button.height, 1);
    assert_eq!(list, Rect::new(0, 0, 40, 7));
    // Without a button the list gets the whole area.
    assert_eq!(promote_split(AREA, Some(&row("a", "pending"))), (AREA, None));
}

#[test]
fn the_key_raises_an_intent_for_the_selected_draft_only() {
    let draft = row("alpha", "draft");

    assert_eq!(
        promote_intent_for_key(KeyCode::Char('p'), Some(&draft)),
        Some(PromoteIntent { feature: "alpha".into() })
    );
    assert_eq!(promote_intent_for_key(KeyCode::Char('p'), Some(&row("alpha", "pending"))), None);
    assert_eq!(promote_intent_for_key(KeyCode::Char('p'), None), None);
    for code in [KeyCode::Down, KeyCode::Enter, KeyCode::Char('x'), KeyCode::Esc] {
        assert_eq!(promote_intent_for_key(code, Some(&draft)), None, "{code:?}");
    }
}

#[test]
fn clicking_the_button_raises_an_intent_and_nothing_else_does() {
    let draft = row("alpha", "draft");
    let (_, button) = promote_split(AREA, Some(&draft));
    let button = button.unwrap();

    assert_eq!(
        promote_intent_for_mouse(AREA, Some(&draft), click(button.x, button.y)),
        Some(PromoteIntent { feature: "alpha".into() })
    );
    // The row above the button, the header, beyond the label, and the far edge.
    for (c, r) in [(button.x, button.y - 1), (3, 0), (3, 3), (39, button.y), (60, 3)] {
        assert_eq!(promote_intent_for_mouse(AREA, Some(&draft), click(c, r)), None, "{c},{r}");
    }
    // Only a left press counts.
    for kind in [
        MouseEventKind::Down(MouseButton::Right),
        MouseEventKind::Up(MouseButton::Left),
        MouseEventKind::Moved,
        MouseEventKind::ScrollDown,
    ] {
        assert_eq!(promote_intent_for_mouse(AREA, Some(&draft), mouse(kind, button.x, button.y)), None);
    }
}

#[test]
fn a_click_where_the_button_would_be_is_inert_when_nothing_promotable_is_selected() {
    let (list, _) = promote_split(AREA, Some(&row("a", "draft")));
    let last = list.bottom();

    assert_eq!(promote_intent_for_mouse(AREA, Some(&row("a", "pending")), click(0, last)), None);
    assert_eq!(promote_intent_for_mouse(AREA, None, click(0, last)), None);
}

#[test]
fn the_button_is_drawn_on_the_last_row_for_a_draft_and_absent_otherwise() {
    let lines = draw(40, 6, Some(&row("alpha", "draft")));
    assert!(lines[5].contains("Mark pending"), "{lines:?}");
    assert!(lines[..5].iter().all(String::is_empty), "{lines:?}");

    for selected in [Some(row("a", "pending")), None] {
        let lines = draw(40, 6, selected.as_ref());
        assert!(lines.iter().all(String::is_empty), "{lines:?}");
    }
}

#[test]
fn a_narrow_or_tiny_area_clips_the_button_without_panicking() {
    let draft = row("alpha", "draft");
    let narrow = draw(8, 3, Some(&draft));
    assert!(narrow.iter().all(|l| l.chars().count() <= 8), "{narrow:?}");

    for (w, h) in [(0, 0), (1, 1), (5, 1), (20, 0)] {
        let _ = draw(w, h, Some(&draft));
        let area = Rect::new(0, 0, w, h);
        let _ = promote_split(area, Some(&draft));
        let _ = promote_intent_for_mouse(area, Some(&draft), click(0, 0));
    }
}
