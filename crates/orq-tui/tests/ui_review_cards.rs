use std::collections::VecDeque;
use std::io;

use orq_tui::{run_browser, BrowserBackend, BrowserEntry, EventSource, LedgerErrorView, PopupModel, PromoteOutcome, ReviewRow};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;

struct Script(VecDeque<Event>);

impl EventSource for Script {
    fn next_event(&mut self) -> io::Result<Event> {
        self.0.pop_front().ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "script exhausted"))
    }
}

struct Fake(Vec<ReviewRow>);

impl BrowserBackend for Fake {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView> {
        Ok(vec![])
    }
    fn promote(&mut self, _feature: &str) -> PromoteOutcome {
        PromoteOutcome::Saved
    }
    fn load_reviews(&mut self) -> Result<Vec<ReviewRow>, String> {
        Ok(self.0.clone())
    }
}

fn reviews(count: usize) -> Vec<ReviewRow> {
    (1..=count)
        .map(|n| ReviewRow {
            run_id: format!("run-{n}"),
            feature: format!("COM-{n:02}"),
            project: "orq-cli".into(),
            status: "ready".into(),
            risk: "low".into(),
            current: true,
            stale: false,
            has_link: false,
        })
        .collect()
}

fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// The Reviews tab (opened with a click on its tab) after `events`, as one string per screen row.
fn screen(height: u16, count: usize, events: Vec<Event>) -> Vec<String> {
    screen_of(height, reviews(count), events)
}

fn screen_of(height: u16, rows: Vec<ReviewRow>, events: Vec<Event>) -> Vec<String> {
    screen_sized(80, height, rows, events)
}

fn screen_sized(width: u16, height: u16, rows: Vec<ReviewRow>, events: Vec<Event>) -> Vec<String> {
    let model = PopupModel { project_name: "orq-cli".into(), repo_path: "/repo".into() };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut script = vec![key(KeyCode::Char('V'))];
    script.extend(events);
    let _ = run_browser(&mut terminal, &model, &mut Fake(rows), &mut Script(script.into()));
    let buffer = terminal.backend().buffer();
    (0..height).map(|y| (0..width).map(|x| buffer[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect()
}

fn selected(lines: &[String]) -> Vec<String> {
    lines.iter().filter_map(|l| l.find("> COM-").map(|at| l[at + 2..at + 8].to_string())).collect()
}

fn row_of(lines: &[String], needle: &str) -> u16 {
    u16::try_from(lines.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle} is shown: {lines:?}"))).unwrap()
}

#[test]
fn a_press_on_any_line_of_a_card_selects_that_review() {
    let before = screen(24, 6, vec![]);
    let top = row_of(&before, "COM-03") - 1;
    for line in 0..4 {
        let lines = screen(24, 6, vec![click(10, top + line)]);
        assert_eq!(selected(&lines), ["COM-03"], "line {line} of the card: {lines:?}");
    }
}

#[test]
fn moving_past_the_last_visible_card_scrolls_so_the_selection_stays_on_screen() {
    let lines = screen(24, 6, vec![key(KeyCode::Down); 5]);
    assert_eq!(selected(&lines), ["COM-06"], "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("COM-01")), "{lines:?}");
}

#[test]
fn a_press_after_scrolling_selects_the_card_under_it() {
    let scrolled = screen(24, 6, vec![key(KeyCode::Down); 5]);
    let top = row_of(&scrolled, "COM-05") - 1;
    let lines = screen(24, 6, [vec![key(KeyCode::Down); 5], vec![click(10, top + 1)]].concat());
    assert_eq!(selected(&lines), ["COM-05"], "{lines:?}");
}

#[test]
fn a_press_below_the_last_card_or_on_the_footer_changes_nothing() {
    let before = screen(24, 6, vec![]);
    let last_border = before.iter().rposition(|l| l.contains('╰')).unwrap();
    let footer = before.len() - 1;
    for y in last_border + 1..=footer {
        let lines = screen(24, 6, vec![click(10, u16::try_from(y).unwrap())]);
        assert_eq!(selected(&lines), ["COM-01"], "row {y}: {lines:?}");
    }
}

// Below 14 rows the popup drops its header and tab bar, so the whole screen is the Reviews body:
// the filter line, the list in `height - 2` rows, then the hint.

#[test]
fn a_window_too_short_for_a_card_lists_one_line_per_review_and_keeps_the_hint() {
    for height in [2u16, 3, 4, 5] {
        let lines = screen(height, 3, vec![]);
        assert!(!lines.iter().any(|l| l.contains('╭') || l.contains('╰')), "height {height}: {lines:?}");
        assert!(lines.last().unwrap().contains("move"), "height {height}: {lines:?}");
        let listed = lines.iter().filter(|l| l.contains("COM-")).count();
        assert_eq!(listed, usize::from(height - 2), "height {height}: {lines:?}");
    }
}

#[test]
fn a_window_with_room_for_exactly_one_card_draws_it_whole() {
    let lines = screen(6, 3, vec![]);
    assert_eq!(lines.iter().filter(|l| l.contains('╭')).count(), 1, "{lines:?}");
    assert_eq!(lines.iter().filter(|l| l.contains('╰')).count(), 1, "{lines:?}");
    assert_eq!(selected(&lines), ["COM-01"], "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("COM-02")), "{lines:?}");
}

#[test]
fn a_spare_row_below_the_cards_is_not_filled_with_part_of_the_next_one() {
    let lines = screen(7, 3, vec![]);
    assert_eq!(lines.iter().filter(|l| l.contains('╭')).count(), 1, "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("COM-02")), "{lines:?}");
    assert!(lines.last().unwrap().contains("move"), "{lines:?}");
}

#[test]
fn the_selection_stays_on_screen_in_every_short_window() {
    for height in [3u16, 4, 5, 6, 7] {
        let lines = screen(height, 3, vec![key(KeyCode::Down), key(KeyCode::Down)]);
        assert_eq!(selected(&lines), ["COM-03"], "height {height}: {lines:?}");
    }
}

#[test]
fn presses_in_a_window_with_no_room_for_any_review_change_nothing_and_do_not_panic() {
    let lines = screen(2, 3, vec![click(10, 0), click(10, 1), key(KeyCode::Down)]);
    assert!(lines[0].contains("Project"), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("COM-")), "{lines:?}");
}

#[test]
fn a_card_with_a_link_offers_open_copy_and_details_right_aligned_on_its_first_line() {
    let rows = vec![ReviewRow { has_link: true, ..reviews(1).remove(0) }];
    let lines = screen_of(24, rows, vec![click(44, 2)]);
    let title = lines.iter().find(|l| l.contains("COM-01")).unwrap();
    assert!(title.contains("[Open MR] [Copy link] [Details]"), "{lines:?}");
    assert!(title.trim_end().ends_with("[Details]│"), "{lines:?}");
}

#[test]
fn a_card_without_a_link_offers_only_details() {
    let lines = screen(24, 1, vec![click(44, 2)]);
    let title = lines.iter().find(|l| l.contains("COM-01")).unwrap();
    assert!(title.contains("[Details]"), "{lines:?}");
    assert!(!title.contains("[Open MR]") && !title.contains("[Copy link]"), "{lines:?}");
}

#[test]
fn every_card_shows_its_own_buttons_not_only_the_selected_one() {
    let rows = vec![ReviewRow { has_link: true, ..reviews(2).remove(0) }, reviews(2).remove(1)];
    let lines = screen_of(24, rows, vec![click(44, 2)]);
    assert_eq!(lines.iter().filter(|l| l.contains("[Details]")).count(), 2, "{lines:?}");
    assert_eq!(lines.iter().filter(|l| l.contains("[Open MR]")).count(), 1, "{lines:?}");
}

fn linked() -> Vec<ReviewRow> {
    vec![ReviewRow { has_link: true, ..reviews(1).remove(0) }]
}

fn title_at(width: u16) -> String {
    screen_sized(width, 24, linked(), vec![]).into_iter().find(|l| l.contains("COM-0") || l.contains("[Details]")).unwrap_or_default()
}

#[test]
fn the_link_buttons_join_details_only_when_the_title_keeps_its_room() {
    // 31 columns of buttons plus 24 of title, inside a border of one column each side.
    assert!(title_at(58).contains("[Open MR] [Copy link] [Details]"), "{}", title_at(58));
    let squeezed = title_at(57);
    assert!(squeezed.contains("[Details]") && !squeezed.contains("[Open MR]") && !squeezed.contains("[Copy link]"), "{squeezed}");
}

#[test]
fn a_narrow_card_keeps_its_title_and_marker_beside_the_details_button() {
    let title = title_at(40);
    assert!(title.contains("> COM-01") && title.contains("[Details]") && title.contains("ready"), "{title}");
}

#[test]
fn a_card_too_narrow_for_any_button_shows_none_and_stays_within_its_border() {
    let lines = screen_sized(12, 24, linked(), vec![]);
    assert!(!lines.iter().any(|l| l.contains("[Details]") || l.contains("[Open MR]")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("COM-0") || l.contains('╭')), "{lines:?}");
    let title = lines.iter().find(|l| l.contains("COM-0")).cloned().unwrap_or_default();
    assert!(title.chars().count() <= 12, "{title}");
}
