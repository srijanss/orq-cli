use std::error::Error;
use std::io;

use ratatui::backend::{Backend, TestBackend};
use ratatui::crossterm::event::{Event, KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use unicode_segmentation::UnicodeSegmentation;

/// Everything the popup header needs, as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupModel {
    pub project_name: String,
    pub repo_path: String,
}

/// A clickable part of the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    ProjectName,
    RepoPath,
}

/// Interactive state of the popup.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PopupState {
    pub selected: Option<Region>,
}

impl PopupState {
    /// Select the region under a left-button press.
    pub fn handle_mouse(&mut self, area: Rect, model: &PopupModel, mouse: MouseEvent) {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }
        if let Some(region) = hit_test(area, model, mouse.column, mouse.row) {
            self.selected = Some(region);
        }
    }
}

/// The rectangle each clickable region occupies, as rendered in `area`.
fn layout(area: Rect, model: &PopupModel) -> [(Region, Rect); 2] {
    let max = usize::from(area.width);
    let row = |index: u16, text: &str| {
        let width = u16::try_from(display_width(&truncate_left(text, max))).unwrap_or(u16::MAX);
        // Clip to the popup: rows the area is too short to render are not clickable.
        Rect::new(area.x, area.y.saturating_add(index), width, 1).intersection(area)
    };
    [
        (Region::ProjectName, row(0, &model.project_name)),
        (Region::RepoPath, row(1, &model.repo_path)),
    ]
}

/// The region under the cell at `column`, `row`, if any.
pub fn hit_test(area: Rect, model: &PopupModel, column: u16, row: u16) -> Option<Region> {
    layout(area, model)
        .into_iter()
        .find(|(_, rect)| rect.contains((column, row).into()))
        .map(|(region, _)| region)
}

/// Where the popup's input events come from (the real terminal, or a script).
pub trait EventSource {
    fn next_event(&mut self) -> io::Result<Event>;
}

/// Puts the terminal into popup mode and back (raw mode, alternate screen...).
pub trait TerminalMode {
    fn enter(&mut self) -> io::Result<()>;
    fn leave(&mut self) -> io::Result<()>;
}

/// The real terminal: raw mode, alternate screen and mouse capture on enter,
/// all undone on leave.
pub struct CrosstermMode;

impl TerminalMode for CrosstermMode {
    fn enter(&mut self) -> io::Result<()> {
        use ratatui::crossterm::event::EnableMouseCapture;
        use ratatui::crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
        enable_raw_mode()?;
        ratatui::crossterm::execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)
    }

    fn leave(&mut self) -> io::Result<()> {
        use ratatui::crossterm::event::DisableMouseCapture;
        use ratatui::crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
        let screen =
            ratatui::crossterm::execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        disable_raw_mode()?;
        screen
    }
}

/// Input events read from the real terminal.
pub struct CrosstermEvents;

impl EventSource for CrosstermEvents {
    fn next_event(&mut self) -> io::Result<Event> {
        ratatui::crossterm::event::read()
    }
}

/// Run the popup on the real terminal until Esc.
pub fn run_on_terminal(model: &PopupModel) -> Result<PopupState, Box<dyn Error>> {
    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    run_popup_session(&mut CrosstermMode, &mut terminal, model, &mut CrosstermEvents)
}

/// Run the popup between `enter` and `leave`. The terminal is always left
/// again once entered, whether the popup closed normally or failed.
pub fn run_popup_session<B: Backend>(
    mode: &mut dyn TerminalMode,
    terminal: &mut Terminal<B>,
    model: &PopupModel,
    events: &mut dyn EventSource,
) -> Result<PopupState, Box<dyn Error>>
where
    B::Error: 'static,
{
    if let Err(err) = mode.enter() {
        // Entry may have got part-way (raw mode on, screen not switched): undo
        // whatever was done, keeping the original error.
        let _ = mode.leave();
        return Err(err.into());
    }
    let result = run_popup(terminal, model, events);
    let restored = mode.leave();
    let state = result?;
    restored?;
    Ok(state)
}

/// Draw the popup and process events until Esc is pressed, returning the
/// final interactive state.
pub fn run_popup<B: Backend>(
    terminal: &mut Terminal<B>,
    model: &PopupModel,
    events: &mut dyn EventSource,
) -> Result<PopupState, Box<dyn Error>>
where
    B::Error: 'static,
{
    let mut state = PopupState::default();
    let mut dirty = true;
    loop {
        if dirty {
            terminal.draw(|frame| render_popup(frame, model))?;
            dirty = false;
        }
        match events.next_event()? {
            Event::Key(key) if key.code == KeyCode::Esc => return Ok(state),
            // Only a left press can change anything; motion and drag events
            // (mouse capture sends many) need neither a size query nor a redraw.
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
                let area = Rect::from((Position::ORIGIN, terminal.size()?));
                let before = state.clone();
                state.handle_mouse(area, model, mouse);
                dirty = state != before;
            }
            Event::Resize(..) => dirty = true,
            _ => {}
        }
    }
}

/// What the popup shows: a resolved project, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupView {
    Project(PopupModel),
    NotARepository { path: String },
}

/// Render `view` once into a `width` x `height` text screen (no terminal
/// needed). Trailing blanks are trimmed from every line.
pub fn render_snapshot(view: &PopupView, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend is infallible");
    terminal
        .draw(|frame| render_view(frame, view))
        .expect("test backend is infallible");
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            let mut line = String::with_capacity(usize::from(width));
            let mut x = 0;
            while x < width {
                let symbol = buffer[(x, y)].symbol();
                line.push_str(symbol);
                // A wide glyph owns the following continuation cell(s) too.
                x += u16::try_from(display_width(symbol).max(1)).unwrap_or(1);
            }
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_view(frame: &mut Frame, view: &PopupView) {
    match view {
        PopupView::Project(model) => render_popup(frame, model),
        PopupView::NotARepository { path } => {
            let area = frame.area();
            let width = usize::from(area.width);
            let empty = Paragraph::new(vec![
                Line::from(truncate_left("not a Git repository", width)),
                Line::from(truncate_left(path, width)),
            ]);
            frame.render_widget(empty, area);
        }
    }
}

pub fn render_popup(frame: &mut Frame, model: &PopupModel) {
    let area = frame.area();
    let width = usize::from(area.width);
    let header = Paragraph::new(vec![
        Line::from(truncate_left(&model.project_name, width)),
        Line::from(truncate_left(&model.repo_path, width)),
    ]);
    frame.render_widget(header, area);
}

/// One feature as the popup lists it: a name and its status as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureRow {
    pub name: String,
    pub status: String,
}

/// Which feature row is selected, if any, and the first row shown.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FeatureListState {
    pub selected: Option<usize>,
    pub offset: usize,
}

impl FeatureListState {
    /// Up and Down move the selection within `len` rows and stop at the ends,
    /// scrolling just far enough to keep it in view. A selection left past the
    /// end of a shrunken list is clamped first, and cleared when the list is
    /// empty. Selecting never starts or edits anything.
    pub fn handle_key(&mut self, code: KeyCode, area: Rect, len: usize) {
        self.normalize(area, len);
        if len == 0 {
            return;
        }
        match code {
            KeyCode::Down => self.selected = Some(self.selected.map_or(0, |i| (i + 1).min(len - 1))),
            KeyCode::Up => self.selected = Some(self.selected.map_or(0, |i| i.saturating_sub(1))),
            _ => return,
        }
        self.offset = self.window(area, len);
    }

    /// A left press on a listed row selects it; the wheel over the list scrolls
    /// one row and stops at both ends, pulling the selection into view if it
    /// would leave. Any other place or event is ignored.
    pub fn handle_mouse(&mut self, area: Rect, len: usize, mouse: MouseEvent) {
        self.normalize(area, len);
        let list = list_area(area);
        let height = usize::from(list.height);
        // Nothing outside the list rows reacts: not the header, not beyond the area.
        if !list.contains((mouse.column, mouse.row).into()) {
            return;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let index = self.offset + usize::from(mouse.row - list.y);
                if index < len {
                    self.selected = Some(index);
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                if height == 0 {
                    return;
                }
                let max = len.saturating_sub(height);
                self.offset = if mouse.kind == MouseEventKind::ScrollDown {
                    (self.offset + 1).min(max)
                } else {
                    self.offset.saturating_sub(1)
                };
                if let Some(i) = self.selected {
                    self.selected = Some(i.clamp(self.offset, (self.offset + height - 1).min(len - 1)));
                }
            }
            _ => {}
        }
    }

    fn normalize(&mut self, area: Rect, len: usize) {
        self.selected = match self.selected {
            Some(_) if len == 0 => None,
            Some(i) => Some(i.min(len - 1)),
            None => None,
        };
        self.offset = self.window(area, len);
    }

    /// The first row shown: `offset`, kept within the list and moved just far
    /// enough that the selected row is inside the window.
    fn window(&self, area: Rect, len: usize) -> usize {
        window(self.selected, self.offset, usize::from(list_area(area).height), len)
    }
}

fn window(selected: Option<usize>, offset: usize, height: usize, len: usize) -> usize {
    if height == 0 || len == 0 {
        return 0;
    }
    let offset = offset.min(len.saturating_sub(height));
    match selected.map(|i| i.min(len - 1)) {
        Some(i) if i < offset => i,
        Some(i) if i >= offset + height => i + 1 - height,
        _ => offset,
    }
}

fn list_area(area: Rect) -> Rect {
    let top = HEADER_ROWS.min(area.height);
    Rect::new(area.x, area.y + top, area.width, area.height - top)
}

/// The header followed by one row per feature, in the order given, or an
/// explicit "no features" line when there are none. The status is always
/// printed as text and is kept visible when the name has to be shortened.
pub fn render_popup_with_features(frame: &mut Frame, model: &PopupModel, rows: &[FeatureRow]) {
    render_feature_list(frame, model, rows, &FeatureListState::default());
}

/// As `render_popup_with_features`, with the selected row marked by `> `.
pub fn render_feature_list(frame: &mut Frame, model: &PopupModel, rows: &[FeatureRow], state: &FeatureListState) {
    render_popup(frame, model);
    let area = frame.area();
    let width = usize::from(area.width);
    let list = list_area(area);
    if list.is_empty() {
        return;
    }
    // Only the rows that fit are formatted, however long the ledger is.
    let lines: Vec<Line> = if rows.is_empty() {
        vec![Line::from(truncate_left("no features", width))]
    } else {
        let height = usize::from(list.height);
        rows.iter()
            .enumerate()
            .skip(window(state.selected, state.offset, height, rows.len()))
            .take(height)
            .map(|(index, row)| {
                let selected = state.selected.map(|i| i.min(rows.len() - 1)) == Some(index);
                // The marker has priority over the row text on narrow popups.
                let marker = match (selected, width) {
                    (true, 0 | 1) => ">",
                    (false, 0 | 1) => " ",
                    (true, _) => "> ",
                    (false, _) => "  ",
                };
                let room = width.saturating_sub(MARKER_WIDTH);
                Line::from(format!("{marker}{}", feature_row_text(row, room)))
            })
            .collect()
    };
    frame.render_widget(Paragraph::new(lines), list);
}

/// Why the ledger could not be shown: where it was looked for and the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerErrorView {
    pub path: String,
    pub reason: String,
}

/// The header followed by a read-only panel naming the ledger path and why it
/// could not be used. There is no list, no selection and no action to take.
/// Control characters are replaced so nothing from the file can steer the
/// terminal.
pub fn render_ledger_error(frame: &mut Frame, model: &PopupModel, error: &LedgerErrorView) {
    render_popup(frame, model);
    let area = frame.area();
    let panel = list_area(area);
    if panel.is_empty() {
        return;
    }
    let width = usize::from(area.width);
    let printable = |text: &str| -> String {
        text.chars().map(|c| if c.is_control() { '\u{fffd}' } else { c }).collect()
    };
    let lines = vec![
        Line::from(truncate_left("ledger error", width)),
        Line::from(truncate_left(&printable(&error.path), width)),
        Line::from(truncate_right(&printable(&error.reason), width)),
    ];
    frame.render_widget(Paragraph::new(lines), panel);
}

/// One `dependsOn` entry with the status of that feature, or `None` when the
/// ledger has no feature of that name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyStatus {
    pub name: String,
    pub status: Option<String>,
}

/// Everything the detail view shows about the selected feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureDetail {
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub test_file: Option<String>,
    pub dependencies: Vec<DependencyStatus>,
}

/// Look each dependency up among `rows`, keeping the order given.
pub fn dependency_statuses(depends_on: &[String], rows: &[FeatureRow]) -> Vec<DependencyStatus> {
    depends_on
        .iter()
        .map(|name| DependencyStatus {
            name: name.clone(),
            status: rows.iter().find(|row| &row.name == name).map(|row| row.status.clone()),
        })
        .collect()
}

/// Draw `detail` read-only into `area`: name, status, description (or a
/// placeholder), the test file when recorded, and each dependency with its
/// current status, flagged when the ledger has no such feature. Control
/// characters from the ledger are replaced before drawing.
pub fn render_feature_detail(frame: &mut Frame, area: Rect, detail: &FeatureDetail) {
    let area = area.intersection(frame.area());
    if area.is_empty() {
        return;
    }
    let width = usize::from(area.width);
    let clean = |text: &str| -> String {
        text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
    };
    let mut text = vec![
        format!("{} [{}]", clean(&detail.name), clean(&detail.status)),
        match &detail.description {
            Some(description) => clean(description),
            None => "(no description)".to_string(),
        },
    ];
    if let Some(test_file) = &detail.test_file {
        text.push(format!("test file: {}", clean(test_file)));
    }
    if !detail.dependencies.is_empty() {
        text.push("depends on:".to_string());
        for dependency in &detail.dependencies {
            let status = match &dependency.status {
                Some(status) => clean(status),
                None => "not in ledger".to_string(),
            };
            text.push(format!("  {} ({status})", clean(&dependency.name)));
        }
    }
    let lines: Vec<Line> = text.iter().map(|line| Line::from(truncate_right(line, width))).collect();
    frame.render_widget(Paragraph::new(lines), area);
}

const MARKER_WIDTH: usize = 2;
const HEADER_ROWS: u16 = 2;

fn feature_row_text(row: &FeatureRow, width: usize) -> String {
    let status = truncate_left(&row.status, width);
    let status_width = display_width(&status);
    // One column of separation, when the name has any room at all.
    let name_room = width.saturating_sub(status_width + 1);
    if name_room == 0 {
        return status;
    }
    let name = truncate_right(&row.name, name_room);
    format!("{name} {status}")
}

/// Keep the head of `text` within `max` display columns, marking the cut with
/// an ellipsis.
fn truncate_right(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut budget = max - 1;
    let mut head = String::new();
    for cluster in text.graphemes(true) {
        let w = display_width(cluster);
        if w > budget {
            break;
        }
        budget -= w;
        head.push_str(cluster);
    }
    format!("{head}…")
}

fn display_width(text: &str) -> usize {
    Span::raw(text).width()
}

/// Keep the tail of `text` within `max` display columns, marking the cut with
/// an ellipsis. The tail of a path is the part that tells repos apart. Whole
/// grapheme clusters are kept or dropped, never split.
fn truncate_left(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut budget = max - 1; // room for the ellipsis
    let mut tail: Vec<&str> = Vec::new();
    for cluster in text.graphemes(true).rev() {
        let w = display_width(cluster);
        if w > budget {
            break;
        }
        budget -= w;
        tail.push(cluster);
    }
    tail.reverse();
    format!("…{}", tail.concat())
}
