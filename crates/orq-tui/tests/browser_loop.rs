use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;

use orq_tui::{
    run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome,
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
