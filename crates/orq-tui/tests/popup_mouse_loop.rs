use std::collections::VecDeque;
use std::io;

use orq_tui::{run_popup, EventSource, PopupModel, Region};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0
            .pop_front()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn clicking_a_region_selects_it_and_the_loop_reports_the_selection_on_esc() {
    let model = PopupModel {
        project_name: "orq-cli".into(),
        repo_path: "/home/dev/code/orq-cli".into(),
    };
    let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
    let mut script = Script(VecDeque::from([
        click(2, 1),
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
    ]));

    let state = run_popup(&mut terminal, &model, &mut script).unwrap();

    assert_eq!(state.selected, Some(Region::RepoPath));
}
