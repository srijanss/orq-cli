use std::collections::VecDeque;
use std::io;

use orq_tui::{run_popup, EventSource, PopupModel, Region};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

// The plain popup draws no chrome, so its header rows are the first rows of a
// large terminal and clicks there must be tested against those rows.
#[test]
fn on_a_large_terminal_a_click_on_the_drawn_project_row_selects_it() {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/home/dev/code/orq-cli".into() };
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    let mut script = Script(VecDeque::from([
        click(2, 0),
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
    ]));

    let state = run_popup(&mut terminal, &model, &mut script).unwrap();

    assert_eq!(state.selected, Some(Region::ProjectName));
}

#[test]
fn on_a_large_terminal_a_click_on_the_drawn_repo_path_row_selects_it() {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/home/dev/code/orq-cli".into() };
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    let mut script = Script(VecDeque::from([
        click(2, 1),
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
    ]));

    let state = run_popup(&mut terminal, &model, &mut script).unwrap();

    assert_eq!(state.selected, Some(Region::RepoPath));
}
