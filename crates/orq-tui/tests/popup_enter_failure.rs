use std::io;

use orq_tui::{run_popup_session, EventSource, PopupModel, TerminalMode};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::Event;
use ratatui::Terminal;

struct FailingEnter {
    log: Vec<&'static str>,
}

impl TerminalMode for FailingEnter {
    fn enter(&mut self) -> io::Result<()> {
        self.log.push("enter");
        Err(io::Error::other("alternate screen unavailable"))
    }

    fn leave(&mut self) -> io::Result<()> {
        self.log.push("leave");
        Ok(())
    }
}

struct NoEvents;

impl EventSource for NoEvents {
    fn next_event(&mut self) -> io::Result<Event> {
        panic!("the popup must not run when entering the terminal failed");
    }
}

#[test]
fn a_failed_terminal_entry_is_undone_and_reported() {
    let model = PopupModel {
        project_name: "orq-cli".into(),
        repo_path: "/home/dev/code/orq-cli".into(),
    };
    let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
    let mut mode = FailingEnter { log: Vec::new() };

    let result = run_popup_session(&mut mode, &mut terminal, &model, &mut NoEvents);

    let err = result.unwrap_err();
    assert!(err.to_string().contains("alternate screen unavailable"), "{err}");
    assert_eq!(mode.log, vec!["enter", "leave"]);
}
