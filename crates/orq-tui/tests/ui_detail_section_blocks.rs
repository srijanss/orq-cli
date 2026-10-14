use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

struct Fixed(Vec<BrowserEntry>);

impl BrowserBackend for Fixed {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(self.0.clone())
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn entry(description: Option<&str>, test_file: Option<&str>, deps: &[&str]) -> BrowserEntry {
    BrowserEntry {
        name: "beta".into(),
        description: description.map(Into::into),
        status: "pending".into(),
        depends_on: deps.iter().map(|d| d.to_string()).collect(),
        test_file: test_file.map(Into::into),
    }
}

/// The frame after opening the detail of the only feature, as trimmed lines.
fn detail_frame(entry: BrowserEntry, width: u16, height: u16) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let events = vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Char('x'))];
    // The script ends by exhausting, which run_browser reports as an error;
    // the last frame drawn is what is inspected.
    let _ = run_browser(&mut terminal, &model, &mut Fixed(vec![entry]), &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn section_top(lines: &[String], title: &str) -> Option<usize> {
    lines.iter().position(|l| l.contains("│╭") && l.contains(title))
}

#[test]
fn description_test_file_and_dependencies_each_get_a_bordered_section() {
    let lines = detail_frame(entry(Some("does the thing"), Some("tests/beta.rs"), &["alpha"]), 70, 30);
    let text = lines.join("\n");

    let description = section_top(&lines, "Description").unwrap_or_else(|| panic!("{text}"));
    let test_file = section_top(&lines, "Test file").unwrap_or_else(|| panic!("{text}"));
    let dependencies = section_top(&lines, "Dependencies").unwrap_or_else(|| panic!("{text}"));
    assert!(description < test_file && test_file < dependencies, "{text}");
    assert!(lines[description + 1].contains("does the thing"), "{text}");
    assert!(lines[test_file + 1].contains("tests/beta.rs"), "{text}");
    assert!(lines[dependencies + 1].contains("alpha") && lines[dependencies + 1].contains("not in ledger"), "{text}");
    // Each section closes with its own bottom border.
    assert!(lines[description + 2].contains("│╰") && lines[test_file + 2].contains("│╰"), "{text}");
}

#[test]
fn an_empty_description_shows_a_placeholder_and_a_missing_test_file_has_no_section() {
    let lines = detail_frame(entry(None, None, &[]), 70, 30);
    let text = lines.join("\n");

    let description = section_top(&lines, "Description").unwrap_or_else(|| panic!("{text}"));
    assert!(lines[description + 1].contains("(no description)"), "{text}");
    assert!(section_top(&lines, "Test file").is_none(), "{text}");
    let dependencies = section_top(&lines, "Dependencies").unwrap_or_else(|| panic!("{text}"));
    assert!(lines[dependencies + 1].contains("(none)"), "{text}");
}

#[test]
fn a_narrow_terminal_keeps_every_section_line_inside_the_panel() {
    let lines = detail_frame(entry(Some("a fairly long description for a narrow popup"), Some("tests/beta.rs"), &["alpha"]), 24, 30);
    let text = lines.join("\n");

    assert!(lines.iter().all(|l| l.chars().count() <= 24), "{text}");
    assert!(section_top(&lines, "Descr").is_some(), "{text}");
}
