use std::error::Error;
use std::io;

use ratatui::backend::{Backend, TestBackend};
use ratatui::crossterm::event::{Event, KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
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
                // The plain popup draws no chrome: hit-test the whole terminal.
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
    render_header(frame, frame.area(), model);
}

fn render_header(frame: &mut Frame, area: Rect, model: &PopupModel) {
    let width = usize::from(area.width);
    let header = Paragraph::new(vec![
        Line::from(truncate_left(&model.project_name, width)),
        Line::from(truncate_left(&model.repo_path, width)),
    ]);
    frame.render_widget(header, area);
}

/// Rows taken by the product header line, the bordered tab bar and the
/// section title.
pub const CHROME_ROWS: u16 = 5;
const CHROME_MIN_WIDTH: u16 = 20;
const CHROME_MIN_HEIGHT: u16 = 14;
const HEADER_TITLE: &str = "ORQ / Developer Control Plane";
const READY_BADGE: &str = "ready";
const FEATURES_TAB: &str = "Features";
const LEDGER_FILE: &str = ".tdd-features.json";

/// What is left of `area` once the chrome is drawn: all of it when the
/// viewport is too small to show the chrome at all.
pub fn chrome_body(area: Rect) -> Rect {
    if area.width < CHROME_MIN_WIDTH || area.height < CHROME_MIN_HEIGHT {
        return area;
    }
    Rect::new(area.x, area.y + CHROME_ROWS, area.width, area.height - CHROME_ROWS)
}

/// The product header (title, static "ready" badge on the right) and the
/// tab bar with only the Features tab, active in a rounded blue border.
/// Returns the area left for the body.
pub fn render_chrome(frame: &mut Frame, area: Rect) -> Rect {
    let body = chrome_body(area);
    if body == area {
        return area;
    }
    let width = usize::from(area.width);
    let badge_width = READY_BADGE.len();
    let title = truncate_right(HEADER_TITLE, width.saturating_sub(badge_width + 1));
    let gap = width.saturating_sub(display_width(&title) + badge_width);
    let header = Line::from(vec![
        Span::styled(title, Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(gap)),
        Span::styled(READY_BADGE, Style::new().fg(Color::Green)),
    ]);
    frame.render_widget(Paragraph::new(header), Rect::new(area.x, area.y, area.width, 1));

    let tab_width = u16::try_from(FEATURES_TAB.len() + 4).unwrap_or(u16::MAX).min(area.width);
    let tab = Rect::new(area.x, area.y + 1, tab_width, 3);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Blue));
    frame.render_widget(Paragraph::new(format!(" {FEATURES_TAB} ")).block(block), tab);

    let file = truncate_left(LEDGER_FILE, width.saturating_sub(FEATURES_TAB.len() + 1));
    let gap = width.saturating_sub(FEATURES_TAB.len() + display_width(&file));
    let title = Line::from(vec![
        Span::styled(FEATURES_TAB, Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(gap)),
        Span::styled(file, Style::new().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(title), Rect::new(area.x, area.y + 4, area.width, 1));
    body
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
        let card = usize::from(item_height(list));
        let height = usize::from(list.height) / card;
        // Nothing outside the list rows reacts: not the header, not beyond the area.
        if !list.contains((mouse.column, mouse.row).into()) {
            return;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let index = self.offset + usize::from(mouse.row - list.y) / card;
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
        let list = list_area(area);
        window(self.selected, self.offset, usize::from(list.height / item_height(list)), len)
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

/// Rows per feature: a bordered card when the list is wide and tall enough
/// for at least two of them (and a tall list, so short popups stay compact), otherwise the compact single row.
fn item_height(list: Rect) -> u16 {
    if list.width >= CARD_MIN_WIDTH && list.height >= CARD_MIN_HEIGHT {
        CARD_HEIGHT
    } else {
        1
    }
}

const CARD_HEIGHT: u16 = 4;
const CARD_MIN_WIDTH: u16 = 30;
const CARD_MIN_HEIGHT: u16 = 10;

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
    let list = list_area(frame.area());
    render_rows(frame, list, rows.len(), |i| &rows[i], &|_| None, state, "no features");
}

/// Rows `0..len` of `row` that fit in `list`, the selected one marked. Only
/// the visible window is fetched and formatted, however long the ledger is.
fn render_rows<'a>(
    frame: &mut Frame,
    list: Rect,
    len: usize,
    row: impl Fn(usize) -> &'a FeatureRow,
    depends: &dyn Fn(usize) -> Option<String>,
    state: &FeatureListState,
    empty: &str,
) {
    if list.is_empty() {
        return;
    }
    let width = usize::from(list.width);
    let card = item_height(list);
    if card > 1 && len > 0 {
        render_cards(frame, list, len, row, depends, state);
        return;
    }
    let lines: Vec<Line> = if len == 0 {
        vec![Line::from(truncate_left(empty, width))]
    } else {
        let height = usize::from(list.height);
        let first = window(state.selected, state.offset, height, len);
        let selected = state.selected.map(|i| i.min(len - 1));
        (first..len.min(first + height))
            .map(|index| {
                // The marker has priority over the row text on narrow popups.
                let marker = match (selected == Some(index), width) {
                    (true, 0 | 1) => ">",
                    (false, 0 | 1) => " ",
                    (true, _) => "> ",
                    (false, _) => "  ",
                };
                let room = width.saturating_sub(MARKER_WIDTH);
                let row = row(index);
                let (name, status) = feature_row_parts(row, room);
                Line::from(vec![
                    Span::raw(format!("{marker}{name}")),
                    Span::styled(status, status_style(&row.status)),
                ])
            })
            .collect()
    };
    frame.render_widget(Paragraph::new(lines), list);
}

/// Cards for the rows that fit whole in `list`, the selected one with a
/// brighter border and the `> ` marker (so selection does not rely on colour).
fn render_cards<'a>(
    frame: &mut Frame,
    list: Rect,
    len: usize,
    row: impl Fn(usize) -> &'a FeatureRow,
    depends: &dyn Fn(usize) -> Option<String>,
    state: &FeatureListState,
) {
    let visible = usize::from(list.height / CARD_HEIGHT);
    let first = window(state.selected, state.offset, visible, len);
    let selected = state.selected.map(|i| i.min(len - 1));
    for (slot, index) in (first..len.min(first + visible)).enumerate() {
        let top = list.y + u16::try_from(slot).unwrap_or(0) * CARD_HEIGHT;
        let area = Rect::new(list.x, top, list.width, CARD_HEIGHT);
        let border = if selected == Some(index) {
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::DarkGray)
        };
        let block = Block::bordered().border_type(BorderType::Rounded).border_style(border);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let width = usize::from(inner.width);
        let row = row(index);
        let summary = depends(index).unwrap_or_else(|| format!("#{}", index + 1));
        let marker = if selected == Some(index) { "> " } else { "  " };
        let (name, status) = feature_row_parts(row, width.saturating_sub(MARKER_WIDTH));
        let used = MARKER_WIDTH + display_width(&name) + display_width(&status);
        let pad = width.saturating_sub(used);
        let lines = vec![
            Line::styled(
                format!("{}{}", " ".repeat(MARKER_WIDTH.min(width)), truncate_right(&summary, width.saturating_sub(MARKER_WIDTH))),
                Style::new().fg(Color::DarkGray),
            ),
            Line::from(vec![
                Span::raw(format!("{marker}{name}")),
                Span::raw(" ".repeat(pad)),
                Span::styled(status, status_style(&row.status)),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

fn dependency_summary(depends_on: &[String]) -> String {
    if depends_on.is_empty() {
        "no dependencies".to_string()
    } else {
        format!("depends on {}", depends_on.join(", "))
    }
}

/// What a key did to the search: consumed it, left it for the list, or asked
/// for the popup to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKey {
    Handled,
    Ignored,
    Close,
}

/// The search field: `/` opens it, typing edits `query`, Enter keeps the
/// filter and leaves the field, and Esc clears before it ever closes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SearchState {
    pub active: bool,
    pub query: String,
}

impl SearchState {
    pub fn handle_key(&mut self, code: KeyCode) -> SearchKey {
        match code {
            KeyCode::Esc if self.active || !self.query.is_empty() => {
                self.active = false;
                self.query.clear();
                SearchKey::Handled
            }
            KeyCode::Esc => SearchKey::Close,
            KeyCode::Char('/') if !self.active => {
                self.active = true;
                SearchKey::Handled
            }
            KeyCode::Char(c) if self.active => {
                if !c.is_control() {
                    self.query.push(c);
                }
                SearchKey::Handled
            }
            KeyCode::Backspace if self.active => {
                self.query.pop();
                SearchKey::Handled
            }
            KeyCode::Enter if self.active => {
                self.active = false;
                SearchKey::Handled
            }
            _ => SearchKey::Ignored,
        }
    }
}

/// Indexes of the items whose name or description contains `query`, ignoring
/// case, in their original order. An empty query matches everything.
pub fn matching_features(items: &[(String, Option<String>)], query: &str) -> Vec<usize> {
    let needle = query.to_lowercase();
    items
        .iter()
        .enumerate()
        .filter(|(_, (name, description))| {
            needle.is_empty()
                || name.to_lowercase().contains(&needle)
                || description.as_deref().is_some_and(|d| d.to_lowercase().contains(&needle))
        })
        .map(|(index, _)| index)
        .collect()
}

/// The popup area as the list handlers must see it: one row shorter at the
/// top while the search field is drawn, so `list_area` of the result is
/// exactly where the filtered rows are rendered.
pub fn search_area(area: Rect, search: &SearchState) -> Rect {
    if search_field_shown(search) && area.height > 0 {
        Rect::new(area.x, area.y + 1, area.width, area.height - 1)
    } else {
        area
    }
}

fn search_field_shown(search: &SearchState) -> bool {
    search.active || !search.query.is_empty()
}

/// As `render_feature_list`, filtered by `search`. `items` are the name and
/// description of each of `rows`; the selection indexes the filtered rows.
/// Handlers for the same frame take `search_area(area, search)`.
pub fn render_feature_search(
    frame: &mut Frame,
    model: &PopupModel,
    rows: &[FeatureRow],
    items: &[(String, Option<String>)],
    search: &SearchState,
    state: &FeatureListState,
) {
    render_search_in(frame, frame.area(), model, rows, items, search, state, &|_| None);
}

fn render_search_in(
    frame: &mut Frame,
    area: Rect,
    model: &PopupModel,
    rows: &[FeatureRow],
    items: &[(String, Option<String>)],
    search: &SearchState,
    state: &FeatureListState,
    depends: &dyn Fn(usize) -> Option<String>,
) {
    render_header(frame, area, model);
    let full = list_area(area);
    if full.is_empty() {
        return;
    }
    if search_field_shown(search) {
        let field = format!("/{}", search.query.chars().filter(|c| !c.is_control()).collect::<String>());
        let line = truncate_left(&field, usize::from(full.width));
        frame.render_widget(Paragraph::new(line), Rect::new(full.x, full.y, full.width, 1));
    }
    let list = list_area(search_area(area, search));
    let matches: Vec<usize> = matching_features(items, &search.query)
        .into_iter()
        .filter(|&index| index < rows.len())
        .collect();
    let empty = if rows.is_empty() { "no features" } else { "no matches" };
    render_rows(frame, list, matches.len(), |i| &rows[matches[i]], &|i| depends(matches[i]), state, empty);
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
    render_ledger_error_in(frame, frame.area(), model, error);
}

fn render_ledger_error_in(frame: &mut Frame, area: Rect, model: &PopupModel, error: &LedgerErrorView) {
    render_header(frame, area, model);
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
    // Built once so the cost is linear in rows + dependencies; the first row
    // with a given name wins.
    let mut by_name: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for row in rows {
        by_name.entry(row.name.as_str()).or_insert(row.status.as_str());
    }
    depends_on
        .iter()
        .map(|name| DependencyStatus {
            name: name.clone(),
            status: by_name.get(name.as_str()).map(|status| (*status).to_string()),
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
        name_with_tag("", &clean(&detail.name), &format!("[{}]", clean(&detail.status)), width),
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
            text.push(name_with_tag("  ", &clean(&dependency.name), &format!("({status})"), width));
        }
    }
    let lines: Vec<Line> = text.iter().map(|line| Line::from(truncate_right(line, width))).collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// The detail view as display lines for `width` columns: the header, the
/// description and test file wrapped (never cut), and each dependency.
pub fn feature_detail_lines(detail: &FeatureDetail, width: usize) -> Vec<String> {
    let clean = |text: &str| -> String {
        text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
    };
    let mut lines =
        vec![name_with_tag("", &clean(&detail.name), &format!("[{}]", clean(&detail.status)), width)];
    match &detail.description {
        Some(description) => lines.extend(wrap_hard(&clean(description), width)),
        None => lines.push(truncate_right("(no description)", width)),
    }
    if let Some(test_file) = &detail.test_file {
        lines.extend(wrap_hard(&format!("test file: {}", clean(test_file)), width));
    }
    if !detail.dependencies.is_empty() {
        lines.push(truncate_right("depends on:", width));
        for dependency in &detail.dependencies {
            let status = match &dependency.status {
                Some(status) => clean(status),
                None => "not in ledger".to_string(),
            };
            lines.push(name_with_tag("  ", &clean(&dependency.name), &format!("({status})"), width));
        }
    }
    lines
}

/// Break `text` into lines of at most `width` display columns, at grapheme
/// cluster boundaries, so joined emoji and combining marks are never split.
fn wrap_hard(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for cluster in text.graphemes(true) {
        // A cluster wider than the whole viewport cannot be shown intact; an
        // ellipsis stands in for it so no line overflows.
        let (cluster, w) = if display_width(cluster) > width {
            ("…".to_string(), 1)
        } else {
            (cluster.to_string(), display_width(cluster))
        };
        if used + w > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        line.push_str(&cluster);
        used += w;
    }
    lines.push(line);
    lines
}

/// Draw `detail` into `area` like `render_feature_detail`, but wrapped and
/// starting `scroll` lines down, so all of it can be reached. The scroll is
/// clamped so the last line can reach the bottom and no further.
pub fn render_feature_detail_scrolled(frame: &mut Frame, area: Rect, detail: &FeatureDetail, scroll: usize) {
    let area = area.intersection(frame.area());
    if area.is_empty() {
        return;
    }
    let lines = feature_detail_lines(detail, usize::from(area.width));
    render_detail_lines(frame, area, &lines, scroll);
}

/// Draw only the visible slice of already laid-out `lines`, from `scroll`
/// (clamped so the last line can reach the bottom and no further).
fn render_detail_lines(frame: &mut Frame, area: Rect, lines: &[String], scroll: usize) {
    let height = usize::from(area.height);
    let start = scroll.min(lines.len().saturating_sub(height));
    let shown: Vec<Line> = lines.iter().skip(start).take(height).map(|l| Line::from(l.as_str())).collect();
    frame.render_widget(Paragraph::new(shown), area);
}

thread_local! {
    static DETAIL_LAYOUTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread has laid a detail view out (wrapped its text);
/// a diagnostic for tests that scrolling reuses the cached layout.
pub fn detail_layouts() -> usize {
    DETAIL_LAYOUTS.with(|count| count.get())
}

/// `prefix` + `name`, then `tag`, within `width` columns. The tag is kept
/// whole when the line fits, else when it leaves a few columns for the name; only the name gives way, so a status
/// or flag is never hidden by a long name, and the name never vanishes.
fn name_with_tag(prefix: &str, name: &str, tag: &str, width: usize) -> String {
    let head = format!("{prefix}{name}");
    if display_width(&head) + 1 + display_width(tag) <= width {
        return format!("{head} {tag}");
    }
    let tag = truncate_left(tag, (width / 2).max(width.saturating_sub(5)).max(1));
    let name_room = width.saturating_sub(display_width(&tag) + 1);
    if name_room == 0 {
        return tag;
    }
    format!("{} {tag}", truncate_right(&head, name_room))
}

const MARKER_WIDTH: usize = 2;
const HEADER_ROWS: u16 = 2;

/// The name part (with its separating space) and the status part of a row,
/// fitted to `width` columns. The status is what the badge colour applies to.
fn feature_row_parts(row: &FeatureRow, width: usize) -> (String, String) {
    let status = truncate_left(&row.status, width);
    let status_width = display_width(&status);
    // One column of separation, when the name has any room at all.
    let name_room = width.saturating_sub(status_width + 1);
    if name_room == 0 {
        return (String::new(), status);
    }
    (format!("{} ", truncate_right(&row.name, name_room)), status)
}

/// The badge colour of a status. Colour only reinforces the status text,
/// which is always drawn (SPEC.md §11): an unrecognised status is red.
pub fn status_style(status: &str) -> Style {
    let colour = match status {
        "completed" => Color::Green,
        "pending" => Color::Blue,
        "in_progress" => Color::Yellow,
        "draft" => Color::DarkGray,
        _ => Color::Red,
    };
    Style::new().fg(colour)
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

const PROMOTE_LABEL: &str = "[ Mark pending (p) ]";

/// A request to promote a draft feature to pending. The popup only raises it;
/// the caller performs the write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromoteIntent {
    pub feature: String,
}

fn promotable(selected: Option<&FeatureRow>) -> Option<&FeatureRow> {
    selected.filter(|row| row.status == "draft")
}

/// Splits `area` into the list area and, for a selected draft, a one-row
/// button area at the bottom.
pub fn promote_split(area: Rect, selected: Option<&FeatureRow>) -> (Rect, Option<Rect>) {
    if promotable(selected).is_none() || area.height == 0 || area.width == 0 {
        return (area, None);
    }
    let width = (PROMOTE_LABEL.chars().count() as u16).min(area.width);
    let button = Rect::new(area.x, area.bottom() - 1, width, 1);
    let list = Rect::new(area.x, area.y, area.width, area.height - 1);
    (list, Some(button))
}

pub fn render_promote_button(frame: &mut Frame, area: Rect, selected: Option<&FeatureRow>) {
    if let (_, Some(button)) = promote_split(area, selected) {
        let label: String = PROMOTE_LABEL.chars().take(button.width as usize).collect();
        frame.render_widget(Paragraph::new(label), button);
    }
}

pub fn promote_intent_for_key(code: KeyCode, selected: Option<&FeatureRow>) -> Option<PromoteIntent> {
    match (code, promotable(selected)) {
        (KeyCode::Char('p'), Some(row)) => Some(PromoteIntent { feature: row.name.clone() }),
        _ => None,
    }
}

pub fn promote_intent_for_mouse(area: Rect, selected: Option<&FeatureRow>, mouse: MouseEvent) -> Option<PromoteIntent> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return None;
    }
    let (_, button) = promote_split(area, selected);
    let button = button?;
    if !button.contains(Position::new(mouse.column, mouse.row)) {
        return None;
    }
    promotable(selected).map(|row| PromoteIntent { feature: row.name.clone() })
}

/// What happened to a promote the caller attempted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromoteOutcome {
    Saved,
    Failed { reason: String },
}

/// The rows to show and the message to display after an outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeView {
    pub rows: Vec<FeatureRow>,
    pub notice: Option<String>,
}

/// Applies a promote outcome: success marks the feature pending locally;
/// failure reports that nothing was saved and reloads from `fresh`.
pub fn apply_promote_outcome(
    rows: &[FeatureRow],
    feature: &str,
    outcome: &PromoteOutcome,
    fresh: &[FeatureRow],
) -> OutcomeView {
    match outcome {
        PromoteOutcome::Saved => {
            let mut rows = rows.to_vec();
            if let Some(row) = rows.iter_mut().find(|row| row.name == feature) {
                row.status = "pending".into();
            }
            OutcomeView { rows, notice: None }
        }
        PromoteOutcome::Failed { reason } => {
            let reason: String = reason.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
            OutcomeView {
                rows: fresh.to_vec(),
                notice: Some(format!("nothing was saved: {reason}")),
            }
        }
    }
}

/// Whether `code` asks to re-read the ledger on demand.
pub fn reload_requested(code: KeyCode) -> bool {
    code == KeyCode::Char('r')
}

/// Draws `notice`, if any, on the last row of `area`, clipped to its width.
pub fn render_notice(frame: &mut Frame, area: Rect, notice: Option<&str>) {
    let Some(text) = notice else { return };
    if area.width == 0 || area.height == 0 {
        return;
    }
    let line = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    frame.render_widget(Paragraph::new(truncate_right(text, area.width as usize)), line);
}

/// One feature as the browser holds it: everything the list, search and
/// detail views need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserEntry {
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub depends_on: Vec<String>,
    pub test_file: Option<String>,
}

/// Where the browser's data lives: reads the ledger and performs promotes.
/// The browser itself never touches a file.
pub trait BrowserBackend {
    fn load(&mut self) -> Result<Vec<BrowserEntry>, LedgerErrorView>;
    fn promote(&mut self, feature: &str) -> PromoteOutcome;
}

/// `orq ui`'s popup on the real terminal: the feature browser between `enter`
/// and `leave`.
pub fn run_browser_on_terminal(model: &PopupModel, backend: &mut dyn BrowserBackend) -> Result<(), Box<dyn Error>> {
    let crossterm = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(crossterm)?;
    run_browser_session(&mut CrosstermMode, &mut terminal, model, backend, &mut CrosstermEvents)
}

/// As `run_popup_session`, for the feature browser.
pub fn run_browser_session<B: Backend>(
    mode: &mut dyn TerminalMode,
    terminal: &mut Terminal<B>,
    model: &PopupModel,
    backend: &mut dyn BrowserBackend,
    events: &mut dyn EventSource,
) -> Result<(), Box<dyn Error>>
where
    B::Error: 'static,
{
    if let Err(err) = mode.enter() {
        let _ = mode.leave();
        return Err(err.into());
    }
    let result = run_browser(terminal, model, backend, events);
    let restored = mode.leave();
    result?;
    restored?;
    Ok(())
}

struct Browser {
    entries: Vec<BrowserEntry>,
    /// Derived from `entries` once per load, so redraws and keys do not clone the ledger.
    rows: Vec<FeatureRow>,
    items: Vec<(String, Option<String>)>,
    error: Option<LedgerErrorView>,
    notice: Option<String>,
    search: SearchState,
    list: FeatureListState,
    /// The open detail view, built once when it is opened or the data reloads.
    detail: Option<DetailView>,
}

struct DetailView {
    detail: FeatureDetail,
    row: FeatureRow,
    scroll: usize,
    /// `detail` wrapped for `width` columns; rebuilt only when the width
    /// changes (a reload builds a fresh view, so it starts empty).
    lines: Vec<String>,
    width: Option<usize>,
}

impl DetailView {
    fn layout(&mut self, width: usize) -> &[String] {
        if self.width != Some(width) {
            self.lines = feature_detail_lines(&self.detail, width);
            self.width = Some(width);
            DETAIL_LAYOUTS.with(|count| count.set(count.get() + 1));
        }
        &self.lines
    }
}

impl Browser {
    fn set_entries(&mut self, entries: Vec<BrowserEntry>) {
        self.rows = entries
            .iter()
            .map(|e| FeatureRow { name: e.name.clone(), status: e.status.clone() })
            .collect();
        self.items = entries.iter().map(|e| (e.name.clone(), e.description.clone())).collect();
        self.entries = entries;
        // The open detail follows the reloaded data, or closes when its feature is gone.
        if let Some(open) = self.detail.take() {
            self.detail = self.detail_for(&open.row.name).map(|view| DetailView { scroll: open.scroll, ..view });
        }
    }

    fn detail_for(&self, name: &str) -> Option<DetailView> {
        let entry = self.entries.iter().find(|e| e.name == name)?;
        Some(DetailView {
            detail: FeatureDetail {
                name: entry.name.clone(),
                description: entry.description.clone(),
                status: entry.status.clone(),
                test_file: entry.test_file.clone(),
                dependencies: dependency_statuses(&entry.depends_on, &self.rows),
            },
            row: FeatureRow { name: entry.name.clone(), status: entry.status.clone() },
            scroll: 0,
            lines: Vec::new(),
            width: None,
        })
    }

    fn rows(&self) -> &[FeatureRow] {
        &self.rows
    }

    fn items(&self) -> &[(String, Option<String>)] {
        &self.items
    }

    /// The row the selection indexes once the search filter is applied.
    fn selected_row(&self) -> Option<FeatureRow> {
        let matches = matching_features(&self.items, &self.search.query);
        let index = self.list.selected?.min(matches.len().checked_sub(1)?);
        self.rows.get(*matches.get(index)?).cloned()
    }

    /// Scroll the open detail by `delta` lines within `text`; true when it moved.
    fn scroll_detail(&mut self, text: Rect, delta: isize) -> bool {
        let Some(open) = &mut self.detail else {
            return false;
        };
        let total = open.layout(usize::from(text.width)).len();
        let max = total.saturating_sub(usize::from(text.height));
        let next = open.scroll.min(max).saturating_add_signed(delta).min(max);
        let moved = next != open.scroll;
        open.scroll = next;
        moved
    }

    fn apply_load(&mut self, loaded: Result<Vec<BrowserEntry>, LedgerErrorView>) {
        match loaded {
            Ok(entries) => {
                self.set_entries(entries);
                self.error = None;
            }
            Err(error) => {
                self.set_entries(Vec::new());
                self.error = Some(error);
            }
        }
    }

    fn promote(&mut self, backend: &mut dyn BrowserBackend, intent: PromoteIntent) {
        let outcome = backend.promote(&intent.feature);
        let fresh = backend.load();
        let fresh_rows: Vec<FeatureRow> = match &fresh {
            Ok(entries) => entries
                .iter()
                .map(|e| FeatureRow { name: e.name.clone(), status: e.status.clone() })
                .collect(),
            Err(_) => Vec::new(),
        };
        let view = apply_promote_outcome(self.rows(), &intent.feature, &outcome, &fresh_rows);
        // A successful load is authoritative, whatever the outcome was: it
        // already includes this promote and any concurrent writer's changes.
        self.apply_load(fresh);
        self.notice = view.notice;
    }

    fn draw(&mut self, frame: &mut Frame, model: &PopupModel) {
        let full = frame.area();
        let area = render_chrome(frame, full);
        if let Some(error) = &self.error {
            render_ledger_error_in(frame, area, model, error);
            return;
        }
        let notice = self.notice.is_some();
        if let Some(open) = &mut self.detail {
            let body = body_area(area, notice);
            let (text, _) = promote_split(body, Some(&open.row));
            let text = text.intersection(area);
            let scroll = open.scroll;
            if !text.is_empty() {
                let lines = open.layout(usize::from(text.width));
                render_detail_lines(frame, text, lines, scroll);
            }
            render_promote_button(frame, body, Some(&open.row));
            render_notice(frame, area, self.notice.as_deref());
            return;
        }
        let body_height = area.height.saturating_sub(u16::from(self.notice.is_some()));
        let body = Rect::new(area.x, area.y, area.width, body_height);
        let selected = self.selected_row();
        let (list, promote_area, panel) = list_layout(body, selected.as_ref());
        let entries = &self.entries;
        let depends = |index: usize| entries.get(index).map(|e| dependency_summary(&e.depends_on));
        render_search_in(frame, list, model, self.rows(), self.items(), &self.search, &self.list, &depends);
        match panel {
            Some(panel) => render_selected_panel(frame, panel, promote_area, selected.as_ref()),
            None => render_promote_button(frame, body, selected.as_ref()),
        }
        render_notice(frame, area, self.notice.as_deref());
    }
}

/// The feature browser: lists the backend's features with search, selection,
/// scrolling, and promote (key or button). A ledger that cannot be loaded is
/// shown read-only with its error. Esc clears a search first, then closes.
pub fn run_browser<B: Backend>(
    terminal: &mut Terminal<B>,
    model: &PopupModel,
    backend: &mut dyn BrowserBackend,
    events: &mut dyn EventSource,
) -> Result<(), Box<dyn Error>>
where
    B::Error: 'static,
{
    let mut browser = Browser {
        entries: Vec::new(),
        rows: Vec::new(),
        items: Vec::new(),
        error: None,
        notice: None,
        search: SearchState::default(),
        list: FeatureListState::default(),
        detail: None,
    };
    browser.apply_load(backend.load());
    let mut dirty = true;
    loop {
        if dirty {
            terminal.draw(|frame| browser.draw(frame, model))?;
            dirty = false;
        }
        match events.next_event()? {
            Event::Key(key) => {
                if browser.error.is_some() {
                    match key.code {
                        KeyCode::Esc => return Ok(()),
                        code if reload_requested(code) => {
                            browser.apply_load(backend.load());
                            dirty = true;
                        }
                        _ => {}
                    }
                    continue;
                }
                if let Some(open) = &browser.detail {
                    let row = open.row.clone();
                    let scrolls = matches!(key.code, KeyCode::Down | KeyCode::Up | KeyCode::PageDown | KeyCode::PageUp);
                    // Only the scroll keys need the viewport, so ignored keys query nothing.
                    let text = if scrolls {
                        let area = chrome_body(Rect::from((Position::ORIGIN, terminal.size()?)));
                        promote_split(body_area(area, browser.notice.is_some()), Some(&row)).0
                    } else {
                        Rect::default()
                    };
                    match key.code {
                        KeyCode::Esc | KeyCode::Backspace => {
                            browser.detail = None;
                            dirty = true;
                        }
                        code if reload_requested(code) => {
                            browser.notice = None;
                            browser.apply_load(backend.load());
                            dirty = true;
                        }
                        KeyCode::Down => dirty = browser.scroll_detail(text, 1),
                        KeyCode::Up => dirty = browser.scroll_detail(text, -1),
                        KeyCode::PageDown => dirty = browser.scroll_detail(text, text.height.max(1) as isize),
                        KeyCode::PageUp => dirty = browser.scroll_detail(text, -(text.height.max(1) as isize)),
                        code => {
                            if let Some(intent) = promote_intent_for_key(code, Some(&row)) {
                                browser.promote(backend, intent);
                                dirty = true;
                            }
                        }
                    }
                    continue;
                }
                if key.code == KeyCode::Enter && !browser.search.active {
                    if let Some(row) = browser.selected_row() {
                        browser.detail = browser.detail_for(&row.name);
                        dirty = true;
                    }
                    continue;
                }
                let area = chrome_body(Rect::from((Position::ORIGIN, terminal.size()?)));
                let selected = browser.selected_row();
                let intent = if browser.search.active {
                    None
                } else {
                    promote_intent_for_key(key.code, selected.as_ref())
                };
                if browser.search.active || intent.is_none() && !reload_requested(key.code) {
                    match browser.search.handle_key(key.code) {
                        SearchKey::Close => return Ok(()),
                        SearchKey::Handled => {
                            browser.list.selected = None;
                            browser.list.offset = 0;
                            dirty = true;
                            continue;
                        }
                        SearchKey::Ignored => {}
                    }
                }
                if let Some(intent) = intent {
                    browser.promote(backend, intent);
                } else if reload_requested(key.code) && !browser.search.active {
                    browser.notice = None;
                    browser.apply_load(backend.load());
                } else {
                    let body = body_area(area, browser.notice.is_some());
                    let (list, _, _) = list_layout(body, selected.as_ref());
                    let len = matching_features(browser.items(), &browser.search.query).len();
                    browser.list.handle_key(key.code, search_area(list, &browser.search), len);
                }
                dirty = true;
            }
            Event::Mouse(mouse) => {
                let acted_on = matches!(
                    mouse.kind,
                    MouseEventKind::Down(MouseButton::Left) | MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                );
                if browser.error.is_some() || !acted_on {
                    continue;
                }
                let area = chrome_body(Rect::from((Position::ORIGIN, terminal.size()?)));
                let body = body_area(area, browser.notice.is_some());
                if let Some(open) = &browser.detail {
                    let row = open.row.clone();
                    let text = promote_split(body, Some(&row)).0;
                    if let Some(intent) = promote_intent_for_mouse(body, Some(&row), mouse) {
                        browser.promote(backend, intent);
                        dirty = true;
                    } else if mouse.kind == MouseEventKind::ScrollDown {
                        dirty = browser.scroll_detail(text, 1);
                    } else if mouse.kind == MouseEventKind::ScrollUp {
                        dirty = browser.scroll_detail(text, -1);
                    }
                    continue;
                }
                let selected = browser.selected_row();
                let (list, promote_area, _) = list_layout(body, selected.as_ref());
                if let Some(intent) = promote_intent_for_mouse(promote_area, selected.as_ref(), mouse) {
                    browser.promote(backend, intent);
                } else {
                    let len = matching_features(browser.items(), &browser.search.query).len();
                    browser.list.handle_mouse(search_area(list, &browser.search), len, mouse);
                }
                dirty = true;
            }
            Event::Resize(..) => dirty = true,
            _ => {}
        }
    }
}

/// Rows the selected-feature panel needs (borders included).
pub const PANEL_ROWS: u16 = 3;
const PANEL_MIN_WIDTH: u16 = 30;
const PANEL_MIN_BODY_HEIGHT: u16 = 14;
const PANEL_HINT: &str = " ↑↓ move · Enter open · / search · r reload · Esc close ";

/// Splits the browser body into the list and, on a roomy viewport, the
/// selected-feature panel below it. Without a panel the promote button
/// keeps its place under the list.
fn list_layout(body: Rect, selected: Option<&FeatureRow>) -> (Rect, Rect, Option<Rect>) {
    if body.width >= PANEL_MIN_WIDTH && body.height >= PANEL_MIN_BODY_HEIGHT {
        let list = Rect::new(body.x, body.y, body.width, body.height - PANEL_ROWS);
        let panel = Rect::new(body.x, list.bottom(), body.width, PANEL_ROWS);
        let inner = Rect::new(panel.x + 1, panel.y + 1, panel.width - 2, 1);
        (list, inner, Some(panel))
    } else {
        let (list, _) = promote_split(body, selected);
        (list, body, None)
    }
}

fn render_selected_panel(frame: &mut Frame, panel: Rect, inner: Rect, selected: Option<&FeatureRow>) {
    let title = match selected {
        Some(row) => format!(" Selected feature · {} ", row.name),
        None => " Selected feature ".to_string(),
    };
    let title: String = title.chars().take(usize::from(panel.width.saturating_sub(2))).collect();
    let hint: String = PANEL_HINT.chars().take(usize::from(panel.width.saturating_sub(2))).collect();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title)
        .title_bottom(Line::from(hint).style(Style::default().fg(Color::DarkGray)));
    frame.render_widget(block, panel);
    let Some(row) = selected else {
        frame.render_widget(Paragraph::new("No feature selected").style(Style::default().fg(Color::DarkGray)), inner);
        return;
    };
    if promotable(selected).is_some() {
        render_promote_button(frame, inner, selected);
    } else {
        frame.render_widget(
            Paragraph::new("Mark pending is only for drafts").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
    }
    let status_width = u16::try_from(row.status.chars().count()).unwrap_or(u16::MAX);
    if inner.width > status_width + PROMOTE_LABEL.chars().count() as u16 {
        let at = Rect::new(inner.right() - status_width, inner.y, status_width, 1);
        frame.render_widget(Paragraph::new(row.status.clone()).style(status_style(&row.status)), at);
    }
}

fn body_area(area: Rect, notice: bool) -> Rect {
    Rect::new(area.x, area.y, area.width, area.height.saturating_sub(u16::from(notice)))
}
