use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;

use orq_tui::{
    detail_layouts, feature_detail_lines, run_browser, FeatureDetail, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome,
};
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

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn wheel(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn entry(name: &str, status: &str) -> BrowserEntry {
    BrowserEntry {
        name: name.into(),
        description: None,
        status: status.into(),
        depends_on: vec![],
        test_file: None,
    }
}

#[derive(Default)]
struct Shared {
    entries: Vec<BrowserEntry>,
    load_error: Option<LedgerErrorView>,
    promotes: Vec<String>,
    fail_with: Option<String>,
    /// Applied to `entries` when a promote fails (what changed behind our back).
    on_failure: Option<Vec<BrowserEntry>>,
    loads: usize,
    /// Replaces `entries` when the second load happens (changed on disk).
    on_second_load: Option<Vec<BrowserEntry>>,
    /// Applied to `entries` whenever a promote succeeds (a concurrent writer).
    alongside_promote: Option<(String, String)>,
}

struct Fake(Rc<RefCell<Shared>>);

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        let mut shared = self.0.borrow_mut();
        shared.loads += 1;
        if shared.loads == 2 {
            if let Some(changed) = shared.on_second_load.take() {
                shared.entries = changed;
            }
        }
        match &shared.load_error {
            Some(error) => Err(error.clone()),
            None => Ok(shared.entries.clone()),
        }
    }

    fn promote(&mut self, feature: &str) -> PromoteOutcome {
        let mut shared = self.0.borrow_mut();
        shared.promotes.push(feature.to_string());
        if let Some(reason) = shared.fail_with.clone() {
            if let Some(fresh) = shared.on_failure.clone() {
                shared.entries = fresh;
            }
            return PromoteOutcome::Failed { reason };
        }
        for e in shared.entries.iter_mut().filter(|e| e.name == feature) {
            e.status = "pending".into();
        }
        if let Some((other, status)) = shared.alongside_promote.clone() {
            for e in shared.entries.iter_mut().filter(|e| e.name == other) {
                e.status = status.clone();
            }
        }
        PromoteOutcome::Saved
    }
}

fn run(shared: &Rc<RefCell<Shared>>, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
    let mut events = Script(events.into());
    run_browser(&mut terminal, &model, &mut Fake(shared.clone()), &mut events).unwrap();
    let buffer = terminal.backend().buffer();
    (0..10)
        .map(|y| (0..40).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn shared(entries: Vec<BrowserEntry>) -> Rc<RefCell<Shared>> {
    Rc::new(RefCell::new(Shared { entries, ..Default::default() }))
}

#[test]
fn the_key_promotes_the_selected_draft_and_the_row_and_button_update() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "draft")]);

    let lines = run(&state, vec![key(KeyCode::Down), key(KeyCode::Char('p')), key(KeyCode::Esc)]);

    assert_eq!(state.borrow().promotes, vec!["alpha".to_string()]);
    let text = lines.join("\n");
    assert!(text.contains("alpha") && text.contains("pending"), "{lines:?}");
    assert!(!text.contains("Mark pending"), "{lines:?}");
}

#[test]
fn clicking_the_button_promotes_but_selecting_a_row_or_clicking_elsewhere_does_not() {
    let state = shared(vec![entry("alpha", "draft")]);

    // Row 2 is the first feature; the button is on the last of the 10 rows.
    run(&state, vec![click(3, 2), click(30, 5), click(0, 0), key(KeyCode::Esc)]);
    assert!(state.borrow().promotes.is_empty());

    run(&state, vec![click(3, 2), click(3, 9), key(KeyCode::Esc)]);
    assert_eq!(state.borrow().promotes, vec!["alpha".to_string()]);
}

#[test]
fn a_failed_promote_says_nothing_was_saved_and_shows_the_fresh_data() {
    let state = shared(vec![entry("alpha", "draft")]);
    {
        let mut s = state.borrow_mut();
        s.fail_with = Some("conflict: ledger changed".into());
        s.on_failure = Some(vec![entry("alpha", "draft"), entry("gamma", "draft")]);
    }

    let lines = run(&state, vec![key(KeyCode::Down), key(KeyCode::Char('p')), key(KeyCode::Esc)]);

    let text = lines.join("\n");
    assert!(text.contains("nothing was saved"), "{lines:?}");
    assert!(text.contains("conflict"), "{lines:?}");
    assert!(text.contains("gamma"), "{lines:?}");
}

#[test]
fn the_reload_key_replaces_the_shown_data_with_what_the_ledger_holds_now() {
    let state = shared(vec![entry("alpha", "draft"), entry("gone", "draft")]);
    // Between the initial load and the reload the ledger changes on disk.
    state.borrow_mut().on_second_load = Some(vec![entry("alpha", "pending"), entry("late", "draft")]);

    let lines = run(&state, vec![key(KeyCode::Char('r')), key(KeyCode::Esc)]);

    assert_eq!(state.borrow().loads, 2);
    let text = lines.join("\n");
    assert!(text.contains("late"), "{lines:?}");
    assert!(!text.contains("gone"), "{lines:?}");
    let alpha = lines.iter().find(|l| l.contains("alpha")).expect("alpha row");
    assert!(alpha.contains("pending"), "{lines:?}");
}

#[test]
fn a_ledger_that_cannot_be_loaded_is_shown_read_only_with_its_error() {
    let state = shared(vec![]);
    state.borrow_mut().load_error = Some(LedgerErrorView {
        path: "/repo/.tdd-features.json".into(),
        reason: "malformed: expected value".into(),
    });

    let lines = run(&state, vec![key(KeyCode::Down), key(KeyCode::Char('p')), key(KeyCode::Esc)]);

    let text = lines.join("\n");
    assert!(text.contains("/repo/.tdd-features.json") && text.contains("malformed"), "{lines:?}");
    assert!(state.borrow().promotes.is_empty());
    assert!(!text.contains("Mark pending"), "{lines:?}");
}

#[test]
fn escape_clears_an_active_search_before_it_closes_the_popup() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "draft")]);

    let lines = run(
        &state,
        vec![key(KeyCode::Char('/')), key(KeyCode::Char('b')), key(KeyCode::Esc), key(KeyCode::Esc)],
    );

    let text = lines.join("\n");
    assert!(text.contains("alpha") && text.contains("beta"), "{lines:?}");
}

#[test]
fn a_search_filters_the_list_and_promotion_targets_the_filtered_selection() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "draft")]);

    run(
        &state,
        vec![
            key(KeyCode::Char('/')),
            key(KeyCode::Char('b')),
            key(KeyCode::Enter),
            key(KeyCode::Down),
            key(KeyCode::Char('p')),
            key(KeyCode::Esc),
            key(KeyCode::Esc),
        ],
    );

    assert_eq!(state.borrow().promotes, vec!["beta".to_string()]);
}

#[test]
fn the_mouse_wheel_scrolls_the_list() {
    let entries = (0..12).map(|i| entry(&format!("f{i:02}"), "pending")).collect();
    let state = shared(entries);

    // 10 rows: 2 header rows leave 8 for the list; two wheel steps move it by two.
    let lines = run(
        &state,
        vec![
            wheel(MouseEventKind::ScrollDown, 3, 3),
            wheel(MouseEventKind::ScrollDown, 3, 3),
            key(KeyCode::Esc),
        ],
    );

    assert!(lines[2].contains("f02"), "{lines:?}");
    assert!(!lines.join("\n").contains("f00"), "{lines:?}");
}

#[test]
fn a_successful_promote_shows_the_reloaded_statuses_including_concurrent_changes() {
    let state = shared(vec![entry("alpha", "draft"), entry("beta", "draft")]);
    state.borrow_mut().alongside_promote = Some(("beta".into(), "pending".into()));

    let lines = run(&state, vec![key(KeyCode::Down), key(KeyCode::Char('p')), key(KeyCode::Esc)]);

    let beta = lines.iter().find(|l| l.contains("beta")).expect("beta row");
    assert!(beta.contains("pending") && !beta.contains("draft"), "{lines:?}");
}

#[test]
fn pointer_motion_and_releases_change_nothing() {
    let state = shared(vec![entry("alpha", "draft")]);

    let lines = run(
        &state,
        vec![
            key(KeyCode::Down),
            wheel(MouseEventKind::Moved, 3, 9),
            wheel(MouseEventKind::Up(MouseButton::Left), 3, 9),
            wheel(MouseEventKind::Drag(MouseButton::Left), 3, 9),
            key(KeyCode::Esc),
        ],
    );

    assert!(state.borrow().promotes.is_empty());
    assert!(lines.join("\n").contains("Mark pending"), "{lines:?}");
}

/// A backend that counts size queries and flushes (one per redraw).
struct Counting {
    inner: TestBackend,
    sizes: Rc<std::cell::Cell<usize>>,
    flushes: Rc<std::cell::Cell<usize>>,
}

impl ratatui::backend::Backend for Counting {
    type Error = <TestBackend as ratatui::backend::Backend>::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
        self.sizes.set(self.sizes.get() + 1);
        self.inner.size()
    }
    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.flushes.set(self.flushes.get() + 1);
        self.inner.flush()
    }
}

fn work_done(events: Vec<Event>) -> (usize, usize) {
    let state = shared(vec![entry("alpha", "draft")]);
    let sizes = Rc::new(std::cell::Cell::new(0));
    let flushes = Rc::new(std::cell::Cell::new(0));
    let backend = Counting { inner: TestBackend::new(40, 10), sizes: sizes.clone(), flushes: flushes.clone() };
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(backend).unwrap();
    run_browser(&mut terminal, &model, &mut Fake(state), &mut Script(events.into())).unwrap();
    (sizes.get(), flushes.get())
}

#[test]
fn ignored_mouse_events_cause_no_size_queries_or_redraws() {
    let baseline = work_done(vec![key(KeyCode::Down), key(KeyCode::Esc)]);

    let with_noise = work_done(vec![
        key(KeyCode::Down),
        wheel(MouseEventKind::Moved, 3, 9),
        wheel(MouseEventKind::Up(MouseButton::Left), 3, 9),
        wheel(MouseEventKind::Drag(MouseButton::Left), 3, 9),
        wheel(MouseEventKind::Down(MouseButton::Right), 3, 9),
        wheel(MouseEventKind::ScrollLeft, 3, 9),
        key(KeyCode::Esc),
    ]);

    assert_eq!(with_noise, baseline);
}

/// Runs `events` and returns the last frame drawn, whether or not the script
/// ends by closing the popup (a script that runs dry still leaves its frame).
fn last_frame(shared: &Rc<RefCell<Shared>>, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
    let mut events = Script(events.into());
    let _ = run_browser(&mut terminal, &model, &mut Fake(shared.clone()), &mut events);
    let buffer = terminal.backend().buffer();
    (0..10)
        .map(|y| (0..40).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

fn detailed(name: &str, status: &str) -> BrowserEntry {
    BrowserEntry {
        name: name.into(),
        description: Some("does the thing".into()),
        status: status.into(),
        depends_on: vec!["base".into(), "ghost".into()],
        test_file: Some("tests/alpha.rs".into()),
    }
}

#[test]
fn enter_opens_the_detail_of_the_selected_feature_and_escape_returns_with_the_selection_kept() {
    let state = shared(vec![entry("base", "completed"), detailed("alpha", "pending"), entry("zeta", "pending")]);

    let in_detail = last_frame(&state, vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter)]);
    let text = in_detail.join("\n");
    assert!(text.contains("alpha") && text.contains("[pending]"), "{in_detail:?}");
    assert!(text.contains("does the thing"), "{in_detail:?}");
    assert!(text.contains("test file: tests/alpha.rs"), "{in_detail:?}");
    assert!(text.contains("base") && text.contains("(completed)"), "{in_detail:?}");
    assert!(text.contains("ghost") && text.contains("not in ledger"), "{in_detail:?}");
    assert!(!text.contains("zeta"), "{in_detail:?}");

    // Esc goes back to the list (not out), and the same row is still selected:
    // pressing Enter again shows the same feature.
    let again = last_frame(
        &state,
        vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Esc), key(KeyCode::Enter)],
    );
    assert!(again.join("\n").contains("test file: tests/alpha.rs"), "{again:?}");

    let back = run(
        &state,
        vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Esc), key(KeyCode::Esc)],
    );
    let text = back.join("\n");
    assert!(text.contains("zeta") && text.contains("base"), "{back:?}");
}

#[test]
fn a_draft_can_be_promoted_from_the_detail_view_and_a_pending_one_cannot() {
    let state = shared(vec![detailed("alpha", "draft")]);

    let lines = last_frame(
        &state,
        vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Char('p'))],
    );

    assert_eq!(state.borrow().promotes, vec!["alpha".to_string()]);
    let text = lines.join("\n");
    assert!(text.contains("[pending]") && text.contains("does the thing"), "{lines:?}");
    assert!(!text.contains("Mark pending"), "{lines:?}");

    last_frame(&state, vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Char('p'))]);
    assert_eq!(state.borrow().promotes.len(), 1);
}

#[test]
fn enter_with_nothing_selected_or_during_search_entry_opens_no_detail() {
    let state = shared(vec![detailed("alpha", "pending")]);

    let nothing = last_frame(&state, vec![key(KeyCode::Enter)]);
    assert!(!nothing.join("\n").contains("test file:"), "{nothing:?}");

    // Enter in the search field only leaves the field.
    let searching = last_frame(
        &state,
        vec![key(KeyCode::Down), key(KeyCode::Char('/')), key(KeyCode::Char('a')), key(KeyCode::Enter)],
    );
    assert!(!searching.join("\n").contains("test file:"), "{searching:?}");
}

fn long_detail() -> BrowserEntry {
    BrowserEntry {
        name: "alpha".into(),
        description: Some(format!("{}TAILWORD", "word ".repeat(30))),
        status: "pending".into(),
        depends_on: (0..8).map(|i| format!("dep{i}")).collect(),
        test_file: None,
    }
}

#[test]
fn a_long_detail_wraps_its_description_and_scrolls_to_its_last_dependency() {
    let state = shared(vec![long_detail()]);

    let top = last_frame(&state, vec![key(KeyCode::Down), key(KeyCode::Enter)]);
    let text = top.join("\n");
    assert!(text.contains("word word"), "{top:?}");
    assert!(!text.contains("dep7"), "{top:?}");
    assert!(top.iter().all(|l| l.chars().count() <= 40), "{top:?}");

    let mut keys = vec![key(KeyCode::Down), key(KeyCode::Enter)];
    keys.extend((0..30).map(|_| key(KeyCode::Down)));
    let bottom = last_frame(&state, keys);
    let text = bottom.join("\n");
    assert!(text.contains("dep7"), "{bottom:?}");

    // The wheel scrolls too, and Up comes back.
    let mut events = vec![key(KeyCode::Down), key(KeyCode::Enter)];
    events.extend((0..30).map(|_| wheel(MouseEventKind::ScrollDown, 3, 3)));
    events.extend((0..30).map(|_| key(KeyCode::Up)));
    let back = last_frame(&state, events);
    assert!(back.join("\n").contains("word word"), "{back:?}");
}

#[test]
fn detail_keys_that_change_nothing_cause_no_redraws() {
    let baseline = work_done(vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Esc), key(KeyCode::Esc)]);

    let with_noise = work_done(vec![
        key(KeyCode::Down),
        key(KeyCode::Enter),
        key(KeyCode::Char('x')),
        key(KeyCode::Up),
        key(KeyCode::Left),
        key(KeyCode::Esc),
        key(KeyCode::Esc),
    ]);

    // Only the size queries for keys that are handled count; ignored keys
    // add none and no flushes.
    assert_eq!(with_noise.1, baseline.1);
}

#[test]
fn ignored_keys_in_the_detail_view_cause_no_size_queries() {
    let baseline = work_done(vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Esc), key(KeyCode::Esc)]);

    let with_noise = work_done(vec![
        key(KeyCode::Down),
        key(KeyCode::Enter),
        key(KeyCode::Char('x')),
        key(KeyCode::Left),
        key(KeyCode::Esc),
        key(KeyCode::Esc),
    ]);

    assert_eq!(with_noise, baseline);
}

#[test]
fn a_reload_in_detail_follows_the_data_and_closes_it_when_the_feature_is_gone() {
    let state = shared(vec![detailed("alpha", "pending")]);
    state.borrow_mut().on_second_load = Some(vec![BrowserEntry {
        description: Some("changed text".into()),
        ..detailed("alpha", "pending")
    }]);
    let changed = last_frame(&state, vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Char('r'))]);
    assert!(changed.join("\n").contains("changed text"), "{changed:?}");

    let state = shared(vec![detailed("alpha", "pending")]);
    state.borrow_mut().on_second_load = Some(vec![entry("zeta", "pending")]);
    let gone = last_frame(&state, vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Char('r'))]);
    let text = gone.join("\n");
    assert!(text.contains("zeta") && !text.contains("test file:"), "{gone:?}");
}

#[test]
fn a_failed_promote_from_detail_says_nothing_was_saved_and_shows_fresh_data() {
    let state = shared(vec![detailed("alpha", "draft")]);
    {
        let mut s = state.borrow_mut();
        s.fail_with = Some("conflict: file changed".into());
        s.on_failure = Some(vec![BrowserEntry { description: Some("fresh text".into()), ..detailed("alpha", "draft") }]);
    }

    let lines = last_frame(&state, vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::Char('p'))]);

    let text = lines.join("\n");
    assert_eq!(state.borrow().promotes, vec!["alpha".to_string()]);
    assert!(text.contains("nothing was saved"), "{lines:?}");
    assert!(text.contains("fresh text"), "{lines:?}");
}

fn described(text: &str) -> FeatureDetail {
    FeatureDetail {
        name: "a".into(),
        description: Some(text.into()),
        status: "s".into(),
        test_file: None,
        dependencies: vec![],
    }
}

#[test]
fn wrapping_keeps_joined_emoji_and_combining_marks_whole_and_never_overflows() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let body = |lines: Vec<String>| lines[1..].to_vec();

    let joined = body(feature_detail_lines(&described(&format!("ab{family}cd")), 4));
    assert_eq!(joined, vec![format!("ab{family}"), "cd".to_string()]);

    let combining = body(feature_detail_lines(&described("abe\u{301}cd"), 3));
    assert_eq!(combining, vec!["abe\u{301}".to_string(), "cd".to_string()]);

    // A cluster wider than the viewport is replaced by an ellipsis, not emitted oversized.
    let cjk = body(feature_detail_lines(&described("\u{4F60}a"), 1));
    assert_eq!(cjk, vec!["\u{2026}".to_string(), "a".to_string()]);
}

#[test]
fn scrolling_the_detail_lays_it_out_once_per_width_not_once_per_key() {
    let state = shared(vec![long_detail()]);
    let mut events = vec![key(KeyCode::Down), key(KeyCode::Enter)];
    events.extend((0..10).map(|_| key(KeyCode::Down)));
    events.extend((0..10).map(|_| wheel(MouseEventKind::ScrollUp, 3, 3)));
    events.push(key(KeyCode::PageDown));
    events.push(key(KeyCode::Esc));
    events.push(key(KeyCode::Esc));

    let before = detail_layouts();
    run(&state, events);

    assert_eq!(detail_layouts() - before, 1);
}

/// A backend whose size follows `target`, so a script can resize the terminal.
struct Resizable {
    inner: std::cell::RefCell<TestBackend>,
    target: Rc<std::cell::Cell<(u16, u16)>>,
}

impl ratatui::backend::Backend for Resizable {
    type Error = <TestBackend as ratatui::backend::Backend>::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.borrow_mut().draw(content)
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.borrow_mut().hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.borrow_mut().show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        self.inner.borrow_mut().get_cursor_position()
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.borrow_mut().set_cursor_position(position)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.borrow_mut().clear()
    }
    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        self.inner.borrow_mut().clear_region(clear_type)
    }
    fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
        let (width, height) = self.target.get();
        self.inner.borrow_mut().resize(width, height);
        Ok(ratatui::layout::Size::new(width, height))
    }
    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
        self.inner.borrow_mut().window_size()
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.borrow_mut().flush()
    }
}

/// Feeds `events`, resizing the terminal whenever a `Resize` is delivered.
struct ResizingScript {
    events: VecDeque<Event>,
    target: Rc<std::cell::Cell<(u16, u16)>>,
}

impl EventSource for ResizingScript {
    fn next_event(&mut self) -> io::Result<Event> {
        let event = self
            .events
            .pop_front()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))?;
        if let Event::Resize(width, height) = event {
            self.target.set((width, height));
        }
        Ok(event)
    }
}

/// The last frame after `events` on a 40x10 terminal that `Resize` events resize.
fn resized_frame(shared: &Rc<RefCell<Shared>>, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let target = Rc::new(std::cell::Cell::new((40, 10)));
    let backend = Resizable { inner: std::cell::RefCell::new(TestBackend::new(40, 10)), target: target.clone() };
    let mut terminal = Terminal::new(backend).unwrap();
    let mut script = ResizingScript { events: events.into(), target: target.clone() };
    let _ = run_browser(&mut terminal, &model, &mut Fake(shared.clone()), &mut script);
    let (width, height) = target.get();
    let inner = terminal.backend().inner.borrow();
    let buffer = inner.buffer();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect()
}

#[test]
fn page_keys_move_a_screenful_in_both_directions() {
    let state = shared(vec![long_detail()]);
    let open = vec![key(KeyCode::Down), key(KeyCode::Enter)];

    let mut down = open.clone();
    down.push(key(KeyCode::PageDown));
    let bottom = last_frame(&state, down);
    let text = bottom.join("\n");
    assert!(text.contains("dep7") && !text.contains("[pending]"), "{bottom:?}");

    let mut back = open;
    back.extend([key(KeyCode::PageDown), key(KeyCode::PageDown), key(KeyCode::PageUp)]);
    let top = last_frame(&state, back);
    let text = top.join("\n");
    assert!(text.contains("[pending]") && !text.contains("dep7"), "{top:?}");
}

#[test]
fn a_reload_to_shorter_content_while_scrolled_to_the_bottom_shows_it_from_the_top() {
    let state = shared(vec![long_detail()]);
    state.borrow_mut().on_second_load = Some(vec![BrowserEntry {
        description: Some("short text".into()),
        depends_on: vec![],
        ..long_detail()
    }]);

    let lines = last_frame(
        &state,
        vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::PageDown), key(KeyCode::Char('r'))],
    );
    let text = lines.join("\n");
    assert!(text.contains("[pending]") && text.contains("short text"), "{lines:?}");

    // Navigation continues from the new bounds: Down has nowhere to go, Up stays put.
    let again = last_frame(
        &state,
        vec![
            key(KeyCode::Down),
            key(KeyCode::Enter),
            key(KeyCode::PageDown),
            key(KeyCode::Char('r')),
            key(KeyCode::Down),
            key(KeyCode::Up),
        ],
    );
    assert!(again.join("\n").contains("short text"), "{again:?}");
}

#[test]
fn a_resize_while_scrolled_rewraps_and_navigation_follows_the_new_bounds() {
    let state = shared(vec![long_detail()]);
    let open = vec![key(KeyCode::Down), key(KeyCode::Enter), key(KeyCode::PageDown)];

    // Wider: the content gets shorter, so the old offset is clamped, not blank.
    let mut wider = open.clone();
    wider.push(Event::Resize(80, 10));
    let lines = resized_frame(&state, wider.clone());
    let text = lines.join("\n");
    assert!(text.contains("dep7") && text.contains("TAILWORD"), "{lines:?}");
    assert!(lines.iter().all(|l| l.chars().count() <= 80), "{lines:?}");

    // From there, Up uses the new bounds and reaches the header again.
    wider.extend((0..5).map(|_| key(KeyCode::Up)));
    let top = resized_frame(&state, wider);
    assert!(top.join("\n").contains("[pending]"), "{top:?}");

    // Narrower: the content gets longer and Down still reaches its end.
    let mut narrower = vec![key(KeyCode::Down), key(KeyCode::Enter), Event::Resize(20, 10)];
    narrower.extend((0..40).map(|_| key(KeyCode::Down)));
    let end = resized_frame(&state, narrower);
    // The names are cut to fit 20 columns, so count the dependency rows instead:
    // all eight are on screen, and the header has scrolled away.
    assert_eq!(end.iter().filter(|l| l.starts_with("  d")).count(), 8, "{end:?}");
    assert!(!end.join("\n").contains("[pending]"), "{end:?}");
    assert!(end.iter().all(|l| l.chars().count() <= 20), "{end:?}");
}
