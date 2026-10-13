use orq_tui::{render_popup, PopupModel};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn path_line(repo_path: &str, width: u16) -> String {
    let model = PopupModel {
        project_name: "p".into(),
        repo_path: repo_path.into(),
    };
    let mut terminal = Terminal::new(TestBackend::new(width, 2)).unwrap();
    terminal.draw(|frame| render_popup(frame, &model)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..width).map(|x| buffer[(x, 1)].symbol().to_string()).collect()
}

#[test]
fn truncation_never_leaves_a_combining_accent_without_its_base_character() {
    let line = path_line("abe\u{301}", 1);

    assert_eq!(line, "…");
}

#[test]
fn truncation_never_splits_a_zero_width_joiner_emoji_sequence() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let line = path_line(&format!("a{family}b"), 5);

    let after_ellipsis = line.trim_start_matches('…');
    assert!(!after_ellipsis.starts_with('\u{200D}'), "orphaned joiner: {line:?}");
    assert!(!after_ellipsis.starts_with('\u{1F469}'), "split sequence: {line:?}");
    assert!(!after_ellipsis.starts_with('\u{1F467}'), "split sequence: {line:?}");
}
