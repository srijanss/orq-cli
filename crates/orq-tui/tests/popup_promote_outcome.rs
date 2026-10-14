use orq_tui::{
    apply_promote_outcome, promote_split, reload_requested, render_notice, FeatureRow, PromoteOutcome,
};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::Terminal;

fn row(name: &str, status: &str) -> FeatureRow {
    FeatureRow { name: name.into(), status: status.into() }
}

fn draw(width: u16, height: u16, notice: Option<&str>) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| render_notice(frame, Rect::new(0, 0, width, height), notice))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

#[test]
fn success_shows_the_feature_as_pending_and_the_button_disappears() {
    let rows = vec![row("alpha", "draft"), row("beta", "draft")];

    let view = apply_promote_outcome(&rows, "alpha", &PromoteOutcome::Saved, &[]);

    assert_eq!(view.rows, vec![row("alpha", "pending"), row("beta", "draft")]);
    assert_eq!(view.notice, None);
    let area = Rect::new(0, 0, 40, 8);
    assert!(promote_split(area, Some(&view.rows[0])).1.is_none());
    assert!(promote_split(area, Some(&view.rows[1])).1.is_some());
}

#[test]
fn failure_says_nothing_was_saved_and_reloads_the_list_from_fresh_data() {
    let stale = vec![row("alpha", "draft")];
    let fresh = vec![row("alpha", "pending"), row("gamma", "draft")];

    for reason in ["conflict: file changed", "lock timeout", "io error"] {
        let outcome = PromoteOutcome::Failed { reason: reason.into() };
        let view = apply_promote_outcome(&stale, "alpha", &outcome, &fresh);

        assert_eq!(view.rows, fresh, "{reason}");
        let notice = view.notice.expect("notice");
        assert!(notice.contains("nothing was saved"), "{notice}");
        assert!(notice.contains(reason), "{notice}");
    }
}

#[test]
fn a_failure_reason_never_carries_control_characters() {
    let outcome = PromoteOutcome::Failed { reason: "bad \u{1b}[31m\u{7}reason\nline".into() };

    let view = apply_promote_outcome(&[], "alpha", &outcome, &[]);

    let notice = view.notice.unwrap();
    assert!(notice.chars().all(|c| !c.is_control()), "{notice:?}");
    assert!(notice.contains("reason"), "{notice:?}");
}

#[test]
fn success_for_an_unknown_feature_changes_nothing() {
    let rows = vec![row("alpha", "draft")];

    let view = apply_promote_outcome(&rows, "ghost", &PromoteOutcome::Saved, &[]);

    assert_eq!(view.rows, rows);
}

#[test]
fn only_the_reload_key_asks_for_a_reload() {
    assert!(reload_requested(KeyCode::Char('r')));
    for code in [KeyCode::Char('p'), KeyCode::Enter, KeyCode::Esc, KeyCode::Char('R')] {
        assert!(!reload_requested(code), "{code:?}");
    }
}

#[test]
fn the_notice_is_drawn_on_the_last_row_clipped_and_absent_when_none() {
    let lines = draw(40, 4, Some("not saved: nothing was saved"));
    assert!(lines[3].contains("nothing was saved"), "{lines:?}");
    assert!(lines[..3].iter().all(String::is_empty), "{lines:?}");

    assert!(draw(40, 4, None).iter().all(String::is_empty));

    let narrow = draw(10, 2, Some("nothing was saved at all"));
    assert!(narrow.iter().all(|l| l.chars().count() <= 10), "{narrow:?}");
    for (w, h) in [(0, 0), (1, 1), (5, 0)] {
        let _ = draw(w, h, Some("x"));
    }
}
