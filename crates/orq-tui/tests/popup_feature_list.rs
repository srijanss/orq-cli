use orq_tui::{render_popup_with_features, FeatureRow, PopupModel};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn render(rows: &[FeatureRow], width: u16, height: u16) -> Vec<String> {
    let model = PopupModel {
        project_name: "orq-cli".into(),
        repo_path: "/repo".into(),
    };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| render_popup_with_features(frame, &model, rows))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn row(name: &str, status: &str) -> FeatureRow {
    FeatureRow { name: name.into(), status: status.into() }
}

#[test]
fn each_feature_is_one_row_with_name_and_status_text_below_the_header_in_order() {
    let lines = render(&[row("zeta", "draft"), row("alpha", "pending")], 40, 6);

    assert!(lines[0].starts_with("orq-cli"), "{lines:?}");
    assert!(lines[1].starts_with("/repo"), "{lines:?}");
    assert!(lines[2].contains("zeta") && lines[2].contains("draft"), "{lines:?}");
    assert!(lines[3].contains("alpha") && lines[3].contains("pending"), "{lines:?}");
}

#[test]
fn an_empty_ledger_shows_an_explicit_no_features_state() {
    let lines = render(&[], 40, 5);

    assert!(lines[2].contains("no features"), "{lines:?}");
}

#[test]
fn a_narrow_terminal_keeps_the_status_visible_and_every_row_within_the_width() {
    let lines = render(&[row("a-very-long-feature-name-indeed", "completed")], 16, 4);

    assert!(lines[2].contains("completed"), "{lines:?}");
    assert!(lines[2].contains('…'), "{lines:?}");
    assert!(lines[2].chars().count() <= 16, "{lines:?}");
}
