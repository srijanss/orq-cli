use std::collections::VecDeque;
use std::io;

use orq_tui::{render_chrome, run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

struct Backend(Result<Vec<BrowserEntry>, LedgerErrorView>);

impl BrowserBackend for Backend {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        self.0.clone()
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
}

fn lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn chrome_lines(width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| { let _ = render_chrome(frame, Rect::new(0, 0, width, height)); }).unwrap();
    lines(&terminal)
}

fn browse(loaded: Result<Vec<BrowserEntry>, LedgerErrorView>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
    let events = vec![Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))];
    run_browser(&mut terminal, &model, &mut Backend(loaded), &mut Script(events.into())).unwrap();
    lines(&terminal)
}

#[test]
fn a_features_title_with_the_ledger_file_name_at_the_right_sits_below_the_tab_bar() {
    let lines = chrome_lines(60, 20);

    assert!(lines[4].starts_with("Features"), "{lines:?}");
    assert!(lines[4].ends_with(".tdd-features.json"), "{lines:?}");
    assert_eq!(lines[4].chars().count(), 60, "{lines:?}");
}

#[test]
fn a_narrow_title_row_never_overflows_and_keeps_the_section_name() {
    for width in [20, 22, 25, 30] {
        let lines = chrome_lines(width, 20);
        assert!(lines[4].chars().count() <= usize::from(width), "{width}: {lines:?}");
        assert!(lines[4].starts_with("Features"), "{width}: {lines:?}");
    }
}

#[test]
fn the_browser_puts_the_project_name_and_path_under_the_title_and_the_list_below_them() {
    let entries = vec![BrowserEntry {
        name: "alpha".into(),
        description: None,
        status: "draft".into(),
        depends_on: vec![],
        test_file: None,
    }];
    let lines = browse(Ok(entries));

    assert!(lines[4].starts_with("Features"), "{lines:?}");
    // The status control shares the project row, right-aligned.
    assert!(lines[5].starts_with("orq-cli"), "{lines:?}");
    assert_eq!(lines[6], "/repo", "{lines:?}");
    assert!(lines[7].starts_with('╭'), "{lines:?}");
}

#[test]
fn an_unreadable_ledger_still_shows_its_read_only_error_under_the_chrome() {
    let error = LedgerErrorView { path: "/repo/.tdd-features.json".into(), reason: "bad json".into() };
    let lines = browse(Err(error));

    assert!(lines[0].starts_with("ORQ / Developer Control Plane"), "{lines:?}");
    assert!(lines.iter().any(|l| l == "ledger error"), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("bad json")), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("Mark pending")), "{lines:?}");
}
