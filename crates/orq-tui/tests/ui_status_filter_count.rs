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
    run_at(60, entries, events)
}

fn run_at(width: u16, entries: Vec<BrowserEntry>, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let height = 24u16;
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


fn choose(entries: Vec<BrowserEntry>, downs: usize) -> Vec<String> {
    let mut events = vec![key(KeyCode::Char('f'))];
    events.extend(std::iter::repeat_with(|| key(KeyCode::Down)).take(downs));
    events.push(key(KeyCode::Enter));
    run(entries, events)
}

fn sample() -> Vec<BrowserEntry> {
    vec![entry("a", "draft"), entry("b", "draft"), entry("c", "draft"), entry("d", "pending"), entry("e", "zeta")]
}

#[test]
fn all_shows_the_total_count() {
    let lines = run(sample(), vec![]);
    assert!(has(&lines, "Status: All (5) ▾"), "{lines:#?}");
}

#[test]
fn a_selected_status_shows_its_own_count() {
    let lines = choose(sample(), 1);
    assert!(has(&lines, "Status: draft (3) ▾"), "{lines:#?}");
    let lines = choose(sample(), 2);
    assert!(has(&lines, "Status: pending (1) ▾"), "{lines:#?}");
}

#[test]
fn a_status_with_no_features_shows_zero_and_unknown_statuses_count() {
    let lines = choose(sample(), 3);
    assert!(has(&lines, "(0)"), "{lines:#?}");
    let lines = choose(sample(), 5);
    assert!(has(&lines, "Status: zeta (1) ▾"), "{lines:#?}");
}

fn many(n: usize) -> Vec<BrowserEntry> {
    (0..n).map(|i| entry(&format!("f{i}"), "draft")).collect()
}

#[test]
fn the_count_is_dropped_below_the_width_that_keeps_the_control_usable() {
    // 16 reserved + "Status:  ▾" (10) + "All" (3) + " (2)" (4) = 33.
    for width in [29u16, 32] {
        let lines = run_at(width, many(2), vec![]);
        assert!(has(&lines, "Status: All ▾"), "width {width}: {lines:#?}");
        assert!(!has(&lines, "(2)"), "width {width}: {lines:#?}");
    }
    let lines = run_at(33, many(2), vec![]);
    assert!(has(&lines, "Status: All (2) ▾"), "{lines:#?}");
}

#[test]
fn a_multi_digit_count_moves_the_threshold_by_its_extra_width() {
    let lines = run_at(33, many(12), vec![]);
    assert!(has(&lines, "Status: All ▾") && !has(&lines, "(12)"), "{lines:#?}");
    let lines = run_at(34, many(12), vec![]);
    assert!(has(&lines, "Status: All (12) ▾"), "{lines:#?}");
}

#[test]
fn a_long_status_is_clipped_but_keeps_its_count_and_arrow() {
    let long = "a-very-long-status-name-that-would-not-fit-in-the-header";
    let entries = vec![entry("alpha", long), entry("bravo", long), entry("candy", "draft")];
    let lines = run_at(60, entries, vec![key(KeyCode::Char('f')), key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter)]);
    assert!(lines.iter().any(|l| l.contains("Status: a-very-long") && l.contains("(2) ▾")), "{lines:#?}");
}
