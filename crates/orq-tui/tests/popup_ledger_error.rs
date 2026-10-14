use orq_tui::{render_ledger_error, LedgerErrorView, PopupModel};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn draw(width: u16, height: u16, error: &LedgerErrorView) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/home/dev/code/orq-cli".into() };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render_ledger_error(frame, &model, error)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn error(path: &str, reason: &str) -> LedgerErrorView {
    LedgerErrorView { path: path.into(), reason: reason.into() }
}

#[test]
fn the_header_is_followed_by_a_read_only_panel_with_the_path_and_the_reason() {
    let lines = draw(60, 8, &error("/repo/.tdd-features.json", "malformed: expected value at line 1"));

    assert_eq!(lines[0], "orq-cli");
    assert_eq!(lines[1], "/home/dev/code/orq-cli");
    let panel = lines[2..].join("\n");
    assert!(panel.contains("/repo/.tdd-features.json"), "{lines:?}");
    assert!(panel.contains("malformed: expected value at line 1"), "{lines:?}");
    // No feature list and no selection marker or action hint.
    assert!(!panel.contains("no features"), "{lines:?}");
    assert!(lines.iter().all(|l| !l.starts_with("> ")), "{lines:?}");
}

#[test]
fn a_long_path_keeps_its_tail_and_a_tiny_viewport_never_panics() {
    let long = format!("/{}/.tdd-features.json", "deep/".repeat(20));
    let lines = draw(30, 6, &error(&long, "missing"));
    assert!(lines.iter().any(|l| l.ends_with("/.tdd-features.json")), "{lines:?}");
    assert!(lines.iter().all(|l| l.chars().count() <= 30), "{lines:?}");

    for (w, h) in [(0, 0), (1, 1), (3, 2), (10, 3)] {
        let _ = draw(w, h, &error(&long, "unreadable"));
    }
}

#[test]
fn control_characters_in_the_reason_are_never_drawn_raw() {
    let lines = draw(60, 6, &error("/repo/.tdd-features.json", "bad \u{1b}[31m name\u{7}"));

    assert!(lines.iter().all(|l| !l.contains('\u{1b}') && !l.contains('\u{7}')), "{lines:?}");
    assert!(lines.join("\n").contains("bad"), "{lines:?}");
}
