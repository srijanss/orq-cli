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

fn entry(name: &str, status: &str) -> BrowserEntry {
    BrowserEntry {
        name: name.into(),
        description: Some(format!("{name} details")),
        status: status.into(),
        depends_on: vec![],
        test_file: None,
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn run(entries: Vec<BrowserEntry>, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let (width, height) = (60u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let _ = run_browser(&mut terminal, &model, &mut Fixed(entries), &mut Script(events.into()));
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

#[test]
fn unknown_statuses_are_listed_under_their_own_name_once_and_sorted_last() {
    let entries = vec![
        entry("alpha", "zeta"),
        entry("bravo", "abandoned"),
        entry("candy", "abandoned"),
        entry("delta", "draft"),
    ];
    let lines = run(entries, vec![key(KeyCode::Char('f'))]);

    let row_of = |needle: &str| lines.iter().position(|l| l.contains(&format!("  {needle}")) || l.contains(&format!("> {needle}")));
    let (completed, abandoned, zeta) = (row_of("completed"), row_of("abandoned"), row_of("zeta"));
    assert!(completed.is_some() && abandoned.is_some() && zeta.is_some(), "{lines:#?}");
    assert!(completed < abandoned && abandoned < zeta, "{lines:#?}");
    assert!(!has(&lines, "other"), "{lines:#?}");
    assert_eq!(lines.iter().filter(|l| l.contains("abandoned") && !l.contains("bravo") && !l.contains("candy")).count(), 1, "{lines:#?}");
}

#[test]
fn choosing_an_unknown_status_shows_only_its_features() {
    let entries = vec![entry("alpha", "zeta"), entry("bravo", "abandoned"), entry("candy", "abandoned"), entry("delta", "draft")];
    // All, draft, pending, in_progress, completed, abandoned
    let mut events = vec![key(KeyCode::Char('f'))];
    events.extend(std::iter::repeat_with(|| key(KeyCode::Down)).take(5));
    events.push(key(KeyCode::Enter));
    let lines = run(entries, events);

    assert!(has(&lines, "Status: abandoned"), "{lines:#?}");
    assert!(has(&lines, "bravo") && has(&lines, "candy"), "{lines:#?}");
    assert!(!has(&lines, "alpha") && !has(&lines, "delta"), "{lines:#?}");
}
