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

fn choose(entries: Vec<BrowserEntry>, downs: usize) -> Vec<String> {
    let mut events = vec![key(KeyCode::Char('f'))];
    events.extend(std::iter::repeat_with(|| key(KeyCode::Down)).take(downs));
    events.push(key(KeyCode::Enter));
    run(entries, events)
}

#[test]
fn a_status_literally_named_all_filters_by_that_status_not_the_sentinel() {
    let entries = vec![entry("alpha", "All"), entry("bravo", "draft"), entry("candy", "zeta")];
    // All, draft, pending, in_progress, completed, then the literal "All".
    let lines = choose(entries, 5);

    assert!(has(&lines, "alpha"), "{lines:#?}");
    assert!(!has(&lines, "bravo") && !has(&lines, "candy"), "{lines:#?}");
}

#[test]
fn a_very_long_status_name_keeps_the_control_and_can_be_left_again() {
    let long = "a-very-long-status-name-that-would-not-fit-in-the-header";
    let entries = vec![entry("alpha", long), entry("bravo", "draft")];
    let lines = choose(entries.clone(), 5);

    assert!(has(&lines, "Status: a-very-long"), "control must stay: {lines:#?}");
    assert!(has(&lines, "a-very-long-status-name") && !has(&lines, "bravo"), "{lines:#?}");

    // Reopen from the same screen and go back up to All.
    let mut events = vec![key(KeyCode::Char('f'))];
    events.extend(std::iter::repeat_with(|| key(KeyCode::Down)).take(5));
    events.push(key(KeyCode::Enter));
    events.push(key(KeyCode::Char('f')));
    events.extend(std::iter::repeat_with(|| key(KeyCode::Up)).take(5));
    events.push(key(KeyCode::Enter));
    let lines = run(entries, events);
    assert!(has(&lines, "Status: All"), "{lines:#?}");
    assert!(has(&lines, "a-very-long-status-name") && has(&lines, "bravo"), "{lines:#?}");
}

#[test]
fn a_wide_character_status_is_filterable_and_reachable() {
    let wide = "完了完了完了完了完了完了完了完了完了完了完了完了";
    let entries = vec![entry("alpha", wide), entry("bravo", "draft")];
    let lines = choose(entries, 5);

    assert!(has(&lines, "Status: "), "{lines:#?}");
    assert!(has(&lines, "alpha") && !has(&lines, "bravo"), "{lines:#?}");
}

fn long_status_events(downs: usize) -> Vec<Event> {
    let mut events = vec![key(KeyCode::Char('f'))];
    events.extend(std::iter::repeat_with(|| key(KeyCode::Down)).take(downs));
    events.push(key(KeyCode::Enter));
    events
}

#[test]
fn a_long_status_keeps_the_control_at_narrow_widths_and_can_be_left() {
    let long = "a-very-long-status-name-that-would-not-fit-in-the-header";
    let entries = vec![entry("alpha", long), entry("bravo", "draft")];

    for width in [29u16, 32, 40, 45] {
        let mut events = long_status_events(5);
        events.push(key(KeyCode::Char('f')));
        events.extend(std::iter::repeat_with(|| key(KeyCode::Up)).take(5));
        events.push(key(KeyCode::Enter));
        let lines = run_at(width, entries.clone(), events);

        assert!(has(&lines, "Status: All"), "width {width}: reopening and leaving must work: {lines:#?}");
        assert!(has(&lines, "bravo"), "width {width}: {lines:#?}");
    }
}

#[test]
fn a_wide_character_status_control_keeps_its_arrow() {
    let wide = "完了完了完了完了完了完了完了完了完了完了完了完了";
    let entries = vec![entry("alpha", wide), entry("bravo", "draft")];

    let lines = run_at(60, entries, long_status_events(5));

    assert!(lines.iter().any(|l| l.contains("Status: ") && l.contains('…') && l.contains('▾')), "{lines:#?}");
}
