use std::cell::Cell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;

use orq_tui::{run_popup, EventSource, PopupModel};
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell as BufferCell;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Size};
use ratatui::Terminal;

/// A `TestBackend` that counts frames flushed and size queries.
struct Counting {
    inner: TestBackend,
    flushes: Rc<Cell<usize>>,
    sizes: Rc<Cell<usize>>,
}

impl Backend for Counting {
    type Error = <TestBackend as Backend>::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a BufferCell)>,
    {
        self.inner.draw(content)
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> Result<Size, Self::Error> {
        self.sizes.set(self.sizes.get() + 1);
        self.inner.size()
    }
    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.flushes.set(self.flushes.get() + 1);
        self.inner.flush()
    }
}

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0
            .pop_front()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
}

fn esc() -> Event {
    Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
}

/// Runs the popup over `events`; returns (frames drawn, size queries).
fn run(events: Vec<Event>) -> (usize, usize) {
    let model = PopupModel {
        project_name: "orq-cli".into(),
        repo_path: "/home/dev/code/orq-cli".into(),
    };
    let flushes = Rc::new(Cell::new(0));
    let sizes = Rc::new(Cell::new(0));
    let backend = Counting {
        inner: TestBackend::new(40, 5),
        flushes: flushes.clone(),
        sizes: sizes.clone(),
    };
    let mut terminal = Terminal::new(backend).unwrap();
    // Terminal::new may query the size once; count from here.
    flushes.set(0);
    sizes.set(0);
    let mut script = Script(events.into());

    run_popup(&mut terminal, &model, &mut script).unwrap();
    (flushes.get(), sizes.get())
}

#[test]
fn ignored_events_neither_redraw_nor_query_the_size_beyond_the_draw() {
    let (frames, sizes) = run(vec![
        mouse(MouseEventKind::Moved, 3, 1),
        mouse(MouseEventKind::Drag(MouseButton::Left), 4, 1),
        Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
        esc(),
    ]);

    // Each draw itself queries the size once (autoresize); ignored events add none.
    assert_eq!((frames, sizes), (1, 1), "only the initial draw is expected");
}

#[test]
fn a_resize_redraws() {
    let (frames, _) = run(vec![Event::Resize(30, 4), esc()]);

    assert_eq!(frames, 2);
}

#[test]
fn a_click_that_changes_the_selection_redraws_but_a_repeat_does_not() {
    let click = || mouse(MouseEventKind::Down(MouseButton::Left), 2, 1);

    let (frames, sizes) = run(vec![click(), click(), esc()]);

    // 2 draws (initial + selection change) and 2 click size queries.
    assert_eq!((frames, sizes), (2, 4));
}
