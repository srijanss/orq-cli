use orq_tui::{render_chrome, CHROME_ROWS};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::Terminal;

fn draw(width: u16, height: u16) -> (Terminal<TestBackend>, Rect) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut body = Rect::default();
    terminal
        .draw(|frame| {
            let full = frame.area();
            body = render_chrome(frame, full);
        })
        .unwrap();
    (terminal, body)
}

fn lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

#[test]
fn the_header_names_the_product_with_a_ready_badge_and_the_tab_bar_has_only_features() {
    let (terminal, body) = draw(60, 20);
    let lines = lines(&terminal);

    assert!(lines[0].starts_with("ORQ / Developer Control Plane"), "{lines:?}");
    assert!(lines[0].ends_with("ready"), "{lines:?}");
    assert_eq!(lines[0].chars().count(), 60,"{lines:?}");
    assert!(lines[1].starts_with('╭') && lines[1].ends_with('╮'), "{lines:?}");
    assert!(lines[2].contains("Features"), "{lines:?}");
    assert!(lines[3].starts_with('╰'), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("Orchestrations")), "{lines:?}");
    assert_eq!(body, Rect::new(0, CHROME_ROWS, 60, 20 - CHROME_ROWS));
}

#[test]
fn the_active_tab_border_is_blue() {
    let (terminal, _) = draw(60, 20);
    let buffer = terminal.backend().buffer();

    assert_eq!(buffer[(0, 1)].fg, Color::Blue);
    assert_eq!(buffer[(0, 2)].fg, Color::Blue);
}

#[test]
fn a_viewport_too_small_for_the_chrome_is_left_alone_and_nothing_panics() {
    for (width, height) in [(0, 0), (1, 1), (5, 2), (10, 3), (19, 40), (60, 13), (3, 3), (1, 40)] {
        let (terminal, body) = draw(width, height);
        assert_eq!(body, Rect::new(0, 0, width, height), "{width}x{height}");
        assert!(lines(&terminal).iter().all(|l| l.is_empty()), "{width}x{height}");
    }
    // Just big enough: the chrome is drawn and the body keeps the rest.
    let (_, body) = draw(20, 14);
    assert_eq!(body, Rect::new(0, CHROME_ROWS, 20, 14 - CHROME_ROWS));
}
