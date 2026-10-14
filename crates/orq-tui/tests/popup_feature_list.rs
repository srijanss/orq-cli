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
        .map(|y| {
            let mut line = String::new();
            let mut x = 0;
            while x < width {
                let symbol = buffer[(x, y)].symbol();
                line.push_str(symbol);
                // A wide glyph owns the following continuation cell(s) too.
                x += u16::try_from(columns(symbol).max(1)).unwrap();
            }
            line.trim_end().to_string()
        })
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

fn columns(line: &str) -> usize {
    ratatui::text::Span::raw(line.to_string()).width()
}

#[test]
fn wide_and_combining_names_are_cut_on_whole_graphemes_within_the_width() {
    // Each CJK glyph is two columns; "e" + combining acute is one cluster.
    let lines = render(&[row("日本語の機能e\u{301}x", "draft")], 12, 4);

    assert!(lines[2].ends_with(" draft"), "{lines:?}");
    assert!(lines[2].contains('…'), "{lines:?}");
    assert!(columns(&lines[2]) <= 12, "{lines:?}");
    assert!(!lines[2].contains('\u{301}') || lines[2].contains("e\u{301}"), "{lines:?}");
}

#[test]
fn a_status_as_wide_as_the_viewport_is_cut_and_nothing_overflows() {
    let lines = render(&[row("name", "completed-and-merged")], 8, 4);

    assert!(columns(&lines[2]) <= 8, "{lines:?}");
    assert!(lines[2].contains('…'), "{lines:?}");
}

#[test]
fn a_viewport_with_no_list_space_renders_without_panicking() {
    for (width, height) in [(0, 0), (1, 1), (5, 2), (5, 1), (1, 6)] {
        let _ = render(&[row("alpha", "draft")], width, height);
    }
}

#[test]
fn a_combining_cluster_is_kept_with_its_base_or_dropped_whole_when_cutting() {
    // Two-column marker + name budget + " draft": width 10 leaves two columns
    // for the name, so "é" survives with its accent and the rest becomes "…".
    let fits = render(&[row("e\u{301}xyz", "draft")], 10, 4);
    assert_eq!(fits[2], "  e\u{301}… draft", "{fits:?}");

    // One column of name budget holds only the ellipsis; the accent is never
    // left behind without its base.
    let tight = render(&[row("e\u{301}xyz", "draft")], 9, 4);
    assert_eq!(tight[2], "  … draft", "{tight:?}");
    assert!(!tight[2].contains('\u{301}'), "{tight:?}");
}
