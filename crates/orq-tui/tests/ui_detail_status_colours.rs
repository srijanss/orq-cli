use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, status_style, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
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

/// The foreground colour of the first cell of the last row containing `needle`.
fn colour_of(terminal: &Terminal<TestBackend>, needle: &str) -> Option<Color> {
    let buffer = terminal.backend().buffer();
    for y in (0..buffer.area.height).rev() {
        let row: Vec<String> = (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect();
        let joined = row.concat();
        if let Some(at) = joined.find(needle) {
            let column = joined[..at].chars().count();
            return buffer[(column as u16, y)].fg.into();
        }
    }
    None
}

fn opened() -> Terminal<TestBackend> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let entries = vec![
        BrowserEntry {
            name: "aaa".into(),
            description: Some("see (completed) note".into()),
            status: "pending".into(),
            depends_on: vec!["bbb".into(), "ghost".into()],
            test_file: None,
        },
        BrowserEntry {
            name: "bbb".into(),
            description: None,
            status: "completed".into(),
            depends_on: vec![],
            test_file: None,
        },
    ];
    let mut terminal = Terminal::new(TestBackend::new(60, 30)).unwrap();
    let events = vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Char('x'))];
    let _ = run_browser(&mut terminal, &model, &mut Fixed(entries), &mut Script(events.into()));
    terminal
}

#[test]
fn the_feature_status_and_each_dependency_status_use_the_list_badge_colours() {
    let terminal = opened();

    assert_eq!(colour_of(&terminal, "[pending]"), status_style("pending").fg);
    assert_eq!(colour_of(&terminal, "(completed)"), status_style("completed").fg);
    // A dependency the ledger does not know is flagged in the unknown colour.
    assert_eq!(colour_of(&terminal, "(not in ledger)"), status_style("nonsense").fg);
    assert_ne!(status_style("pending").fg, status_style("completed").fg);
}

/// The foreground colour at the start of `tag` inside the row that contains `row_text`.
fn colour_in_row(terminal: &Terminal<TestBackend>, row_text: &str, tag: &str) -> Option<Color> {
    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        let joined: String = (0..buffer.area.width).map(|x| buffer[(x, y)].symbol().to_string()).collect();
        if joined.contains(row_text) {
            let at = joined.rfind(tag)?;
            return Some(buffer[(joined[..at].chars().count() as u16, y)].fg);
        }
    }
    None
}

fn opened_with(width: u16, entries: Vec<BrowserEntry>) -> Terminal<TestBackend> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
    let events = vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Char('x'))];
    let _ = run_browser(&mut terminal, &model, &mut Fixed(entries), &mut Script(events.into()));
    terminal
}

fn entry(name: &str, description: Option<&str>, status: &str, depends_on: &[&str]) -> BrowserEntry {
    BrowserEntry {
        name: name.into(),
        description: description.map(Into::into),
        status: status.into(),
        depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
        test_file: None,
    }
}

#[test]
fn status_text_is_always_drawn() {
    let terminal = opened();
    let buffer = terminal.backend().buffer();
    let text: String = (0..buffer.area.height)
        .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
        .map(|(x, y)| buffer[(x, y)].symbol().to_string())
        .collect();

    assert!(text.contains("[pending]") && text.contains("(completed)") && text.contains("(not in ledger)"), "{text}");
}

#[test]
fn a_description_that_ends_in_a_status_like_tag_is_not_coloured() {
    let terminal = opened_with(60, vec![
        entry("aaa", Some("blocked by (completed)"), "pending", &["bbb"]),
        entry("bbb", None, "completed", &[]),
    ]);

    // The tag at the end of the description row is prose.
    let prose = colour_in_row(&terminal, "blocked by (completed)", "(completed)")
        .expect("description tag should be visible");
    assert_ne!(Some(prose), status_style("completed").fg);
    // The dependency's identical tag still is a badge.
    assert_eq!(colour_in_row(&terminal, "bbb", "(completed)"), status_style("completed").fg);
}

#[test]
fn colours_survive_a_width_that_truncates_the_section_heading() {
    // At 14 columns the "Dependencies" heading no longer fits, but "b (ok)" does.
    let terminal = opened_with(14, vec![entry("aaa", None, "pending", &["b"]), entry("b", None, "ok", &[])]);

    assert_eq!(colour_in_row(&terminal, "b (ok)", "(ok)"), status_style("ok").fg);
    assert_ne!(status_style("ok").fg, Some(Color::Reset));
}
