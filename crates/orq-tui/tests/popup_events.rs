use std::collections::VecDeque;
use std::io;

use orq_tui::{run_popup, run_popup_session, EventSource, PopupModel, TerminalMode};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

struct Script {
    events: VecDeque<Event>,
}

impl Script {
    fn keys(codes: &[KeyCode]) -> Self {
        let events = codes
            .iter()
            .map(|code| Event::Key(KeyEvent::new(*code, KeyModifiers::NONE)))
            .collect();
        Self { events }
    }
}

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.events
            .pop_front()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

fn model() -> PopupModel {
    PopupModel {
        project_name: "orq-cli".into(),
        repo_path: "/home/dev/code/orq-cli".into(),
    }
}

#[test]
fn esc_closes_the_popup_and_other_keys_leave_it_open() {
    let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
    let mut script = Script::keys(&[
        KeyCode::Char('a'),
        KeyCode::Down,
        KeyCode::Esc,
        KeyCode::Char('x'),
    ]);

    run_popup(&mut terminal, &model(), &mut script).unwrap();

    assert_eq!(script.events.len(), 1, "stopped at Esc, did not drain the rest");
}

#[derive(Default)]
struct RecordingMode {
    log: Vec<&'static str>,
}

impl TerminalMode for RecordingMode {
    fn enter(&mut self) -> io::Result<()> {
        self.log.push("enter");
        Ok(())
    }

    fn leave(&mut self) -> io::Result<()> {
        self.log.push("leave");
        Ok(())
    }
}

#[test]
fn terminal_is_restored_after_esc_and_after_an_event_error() {
    let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();

    let mut mode = RecordingMode::default();
    let mut script = Script::keys(&[KeyCode::Esc]);
    run_popup_session(&mut mode, &mut terminal, &model(), &mut script).unwrap();
    assert_eq!(mode.log, vec!["enter", "leave"]);

    let mut mode = RecordingMode::default();
    let mut script = Script::keys(&[]);
    let result = run_popup_session(&mut mode, &mut terminal, &model(), &mut script);
    assert!(result.is_err());
    assert_eq!(mode.log, vec!["enter", "leave"]);
}
