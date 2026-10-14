use orq_tui::{render_popup_with_features, status_style, FeatureRow, PopupModel};
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;

fn row(name: &str, status: &str) -> FeatureRow {
    FeatureRow { name: name.into(), status: status.into() }
}

#[test]
fn each_known_status_has_its_own_colour_and_unknown_is_red() {
    let fg = |status: &str| status_style(status).fg;

    assert_eq!(fg("completed"), Some(Color::Green));
    assert_eq!(fg("pending"), Some(Color::Blue));
    assert_eq!(fg("in_progress"), Some(Color::Yellow));
    assert_eq!(fg("draft"), Some(Color::DarkGray));
    for unknown in ["", "weird", "Completed"] {
        assert_eq!(fg(unknown), Some(Color::Red), "{unknown:?}");
    }
}

#[test]
fn the_status_text_is_drawn_in_its_colour_and_the_name_is_left_unstyled() {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let rows = [row("alpha", "completed"), row("beta", "draft")];
    let mut terminal = Terminal::new(TestBackend::new(30, 6)).unwrap();
    terminal.draw(|frame| render_popup_with_features(frame, &model, &rows)).unwrap();
    let buffer = terminal.backend().buffer();

    // Row 2 is "  alpha completed": the name stays default, the status is green.
    let line: String = (0..30).map(|x| buffer[(x, 2)].symbol().to_string()).collect();
    let status_at = u16::try_from(line.find("completed").unwrap()).unwrap();
    let name_at = u16::try_from(line.find("alpha").unwrap()).unwrap();
    assert_eq!(buffer[(status_at, 2)].fg, Color::Green, "{line:?}");
    assert_eq!(buffer[(status_at + 8, 2)].fg, Color::Green, "{line:?}");
    assert_eq!(buffer[(name_at, 2)].fg, Color::Reset, "{line:?}");

    let line: String = (0..30).map(|x| buffer[(x, 3)].symbol().to_string()).collect();
    let status_at = u16::try_from(line.find("draft").unwrap()).unwrap();
    assert_eq!(buffer[(status_at, 3)].fg, Color::DarkGray, "{line:?}");
}
