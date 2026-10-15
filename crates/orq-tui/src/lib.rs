use std::collections::HashSet;
use std::error::Error;
use std::io;

mod form;
pub use form::{FeatureFields, FormSaveOutcome};

mod preview;
pub use preview::{AcceptedCandidate, CandidateKind, CandidateView};
use preview::{PreviewAction, PreviewState};
use form::{FormAction, FormState};

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

    /// Waits up to `timeout` for an event. Sources that cannot time out block
    /// like `next_event`, which is right for scripted input.
    fn poll_event(&mut self, _timeout: std::time::Duration) -> io::Result<Option<Event>> {
        self.next_event().map(Some)
    }
}

/// Puts the terminal into popup mode and back (raw mode, alternate screen...).
pub trait TerminalMode {
    fn enter(&mut self) -> io::Result<()>;
    fn leave(&mut self) -> io::Result<()>;
}

/// The title the interactive UI shows in its terminal or popup chrome.
pub const TERMINAL_TITLE: &str = "ORQ";

/// Saves the current terminal title (xterm `CSI 22;2 t`), then sets
/// [`TERMINAL_TITLE`].
pub fn enter_title_sequence(out: &mut impl io::Write) -> io::Result<()> {
    out.write_all(b"\x1b[22;2t")?;
    ratatui::crossterm::queue!(out, ratatui::crossterm::terminal::SetTitle(TERMINAL_TITLE))?;
    out.flush()
}

/// Restores the title saved by [`enter_title_sequence`] (xterm `CSI 23;2 t`).
pub fn leave_title_sequence(out: &mut impl io::Write) -> io::Result<()> {
    out.write_all(b"\x1b[23;2t")?;
    out.flush()
}

/// Remembers whether this session pushed a title, so `leave` pops only what
/// `enter` saved (a failed entry must not pop a parent's saved title).
#[derive(Debug, Default)]
pub struct TitleState {
    saved: bool,
}

impl TitleState {
    /// Pushes the current title and sets ORQ; records the push once written.
    pub fn enter(&mut self, out: &mut impl io::Write) -> io::Result<()> {
        out.write_all(b"\x1b[22;2t")?;
        self.saved = true;
        ratatui::crossterm::queue!(out, ratatui::crossterm::terminal::SetTitle(TERMINAL_TITLE))?;
        out.flush()
    }

    /// Pops the saved title if, and only if, this session pushed one.
    pub fn leave(&mut self, out: &mut impl io::Write) -> io::Result<()> {
        if !std::mem::take(&mut self.saved) {
            return Ok(());
        }
        leave_title_sequence(out)
    }
}

/// The real terminal: raw mode, alternate screen, mouse capture and the ORQ
/// title on enter, all undone on leave.
#[derive(Default)]
pub struct CrosstermMode {
    title: TitleState,
}

impl TerminalMode for CrosstermMode {
    fn enter(&mut self) -> io::Result<()> {
        use ratatui::crossterm::event::EnableMouseCapture;
        use ratatui::crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
        enable_raw_mode()?;
        ratatui::crossterm::execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        self.title.enter(&mut io::stdout())
    }

    fn leave(&mut self) -> io::Result<()> {
        use ratatui::crossterm::event::DisableMouseCapture;
        use ratatui::crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
        let title = self.title.leave(&mut io::stdout());
        let screen =
            ratatui::crossterm::execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        disable_raw_mode()?;
        title.and(screen)
    }
}

/// Input events read from the real terminal.
pub struct CrosstermEvents;

impl EventSource for CrosstermEvents {
    fn next_event(&mut self) -> io::Result<Event> {
        ratatui::crossterm::event::read()
    }

    fn poll_event(&mut self, timeout: std::time::Duration) -> io::Result<Option<Event>> {
        if ratatui::crossterm::event::poll(timeout)? {
            ratatui::crossterm::event::read().map(Some)
        } else {
            Ok(None)
        }
    }
}

/// Run the popup on the real terminal until Esc.
pub fn run_on_terminal(model: &PopupModel) -> Result<PopupState, Box<dyn Error>> {
    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    run_popup_session(&mut CrosstermMode::default(), &mut terminal, model, &mut CrosstermEvents)
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
/// tab bar with the Features tab active. Returns the area left for the body.
pub fn render_chrome(frame: &mut Frame, area: Rect) -> Rect {
    render_chrome_tabs(frame, area, Tab::Features)
}

/// As `render_chrome`, with `active` drawn in the blue border and the other
/// tab dimmed.
pub fn render_chrome_tabs(frame: &mut Frame, area: Rect, active: Tab) -> Rect {
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

    for (tab, rect) in tab_rects(area) {
        let (label, colour) = match tab {
            Tab::Features => (FEATURES_TAB, Color::Blue),
            Tab::Settings => (SETTINGS_TAB, Color::Blue),
        };
        let colour = if tab == active { colour } else { Color::DarkGray };
        let block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(colour));
        frame.render_widget(Paragraph::new(format!(" {label} ")).block(block), rect);
    }

    let (name, file) = match active {
        Tab::Features => (FEATURES_TAB, LEDGER_FILE),
        Tab::Settings => (SETTINGS_TAB, SETTINGS_FILE),
    };
    let file = truncate_left(file, width.saturating_sub(name.len() + 1));
    let gap = width.saturating_sub(name.len() + display_width(&file));
    let title = Line::from(vec![
        Span::styled(name, Style::new().add_modifier(Modifier::BOLD)),
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
                let slot = usize::from(mouse.row - list.y) / card;
                // Rows below the last whole card are blank: nothing to select there.
                let index = self.offset + slot;
                if slot < height && index < len {
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
    render_rows(frame, list, rows.len(), |i| &rows[i], &|_| None, None, state, "no features");
}

/// Rows `0..len` of `row` that fit in `list`, the selected one marked. Only
/// the visible window is fetched and formatted, however long the ledger is.
fn render_rows<'a>(
    frame: &mut Frame,
    list: Rect,
    len: usize,
    row: impl Fn(usize) -> &'a FeatureRow,
    depends: &dyn Fn(usize) -> Option<String>,
    marked: Option<&dyn Fn(usize) -> bool>,
    state: &FeatureListState,
    empty: &str,
) {
    if list.is_empty() {
        return;
    }
    let width = usize::from(list.width);
    let card = item_height(list);
    if card > 1 && len > 0 {
        render_cards(frame, list, len, row, depends, marked, state);
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
                let mark = mark_prefix(marked, index);
                let room = width.saturating_sub(MARKER_WIDTH + mark.chars().count());
                let row = row(index);
                let (name, status) = feature_row_parts(row, room);
                Line::from(vec![
                    Span::styled(format!("{marker}{mark}{name}"), name_style(selected == Some(index))),
                    Span::styled(status, status_style(&row.status)),
                ])
            })
            .collect()
    };
    frame.render_widget(Paragraph::new(lines), list);
}

const MARK_ON: &str = "[x] ";
const MARK_OFF: &str = "[ ] ";

/// The check box drawn before a name once any row is marked, so the names
/// stay aligned; empty while nothing is marked. `index` is the row's index
/// in the list being drawn.
fn mark_prefix(marked: Option<&dyn Fn(usize) -> bool>, index: usize) -> &'static str {
    match marked {
        Some(is_marked) if is_marked(index) => MARK_ON,
        Some(_) => MARK_OFF,
        None => "",
    }
}

/// The feature name's style: bright white and bold when selected, so the
/// selection reads clearly on dark terminals; the default otherwise.
fn name_style(selected: bool) -> Style {
    if selected {
        Style::new().fg(Color::White).add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    }
}

/// Cards for the rows that fit whole in `list`, the selected one with a
/// brighter border and the `> ` marker (so selection does not rely on colour).
fn render_cards<'a>(
    frame: &mut Frame,
    list: Rect,
    len: usize,
    row: impl Fn(usize) -> &'a FeatureRow,
    depends: &dyn Fn(usize) -> Option<String>,
    marked: Option<&dyn Fn(usize) -> bool>,
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
        let mark = mark_prefix(marked, index);
        let (name, status) = feature_row_parts(row, width.saturating_sub(MARKER_WIDTH + mark.chars().count()));
        let name = format!("{mark}{name}");
        let used = MARKER_WIDTH + display_width(&name) + display_width(&status);
        let pad = width.saturating_sub(used);
        let is_selected = selected == Some(index);
        let summary_style = if is_selected { name_style(true) } else { Style::new().fg(Color::DarkGray) };
        let lines = vec![
            Line::styled(
                format!("{}{}", " ".repeat(MARKER_WIDTH.min(width)), truncate_right(&summary, width.saturating_sub(MARKER_WIDTH))),
                summary_style,
            ),
            Line::from(vec![
                Span::styled(format!("{marker}{name}"), name_style(is_selected)),
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

/// The statuses the browser knows the lifecycle order of; any other status
/// is listed after them under its own name.
const KNOWN_STATUSES: [&str; 4] = ["draft", "pending", "in_progress", "completed"];
const FILTER_ALL: &str = "All";

/// Whether `status` passes the dropdown's filter; `None` is "All".
fn status_matches(filter: Option<&str>, status: &str) -> bool {
    match filter {
        None => true,
        Some(wanted) => status == wanted,
    }
}

/// "All" plus every known status in lifecycle order (present or not), then
/// each distinct other status present by its own name, sorted.
fn status_options(rows: &[FeatureRow]) -> Vec<String> {
    let mut options = vec![FILTER_ALL.to_string()];
    options.extend(KNOWN_STATUSES.iter().map(|s| (*s).to_string()));
    let unknown: std::collections::BTreeSet<&str> =
        rows.iter().map(|r| r.status.as_str()).filter(|s| !KNOWN_STATUSES.contains(s)).collect();
    options.extend(unknown.into_iter().map(str::to_string));
    options
}

/// Longest status name shown in the control or the dropdown, in terminal cells;
/// longer names are clipped for display only (matching uses the full name).
const MAX_STATUS_CELLS: usize = 20;

/// Columns kept free beside the control in the header row.
const FILTER_HEADER_RESERVE: usize = 16;

/// The control's text for a header `list_width` cells wide. The status name
/// is clipped to what the header can spare, but never below "All"'s width, so
/// the control stays usable whenever the initial "Status: All ▾" fits.
fn filter_label(filter: Option<&str>, list_width: u16) -> String {
    let frame = display_width("Status:  ▾");
    let spare = usize::from(list_width).saturating_sub(FILTER_HEADER_RESERVE + frame);
    let budget = spare.clamp(display_width(FILTER_ALL), MAX_STATUS_CELLS);
    let name = truncate_right(filter.unwrap_or(FILTER_ALL), budget);
    format!("Status: {name} ▾")
}

/// Index of the active option. Option 0 is always the unfiltered sentinel, so
/// a status that happens to be called "All" is still a distinct, later option.
fn active_option(options: &[String], filter: Option<&str>) -> usize {
    filter.and_then(|f| options.iter().skip(1).position(|o| o == f)).map_or(0, |i| i + 1)
}

/// The one-row control on the right of the header's first row.
fn filter_control_rect(list: Rect, filter: Option<&str>) -> Option<Rect> {
    let width = u16::try_from(display_width(&filter_label(filter, list.width))).ok()?;
    if list.height == 0 || list.width < width + 16 {
        return None;
    }
    Some(Rect::new(list.right() - width, list.y, width, 1))
}

/// The bordered option list, hung under the control; option `i` is on row
/// `y + 1 + i` inside it.
/// First option shown when `shown` rows are visible and `highlight` must stay in view.
fn dropdown_top(shown: usize, highlight: usize) -> usize {
    highlight.saturating_sub(shown.saturating_sub(1))
}

fn dropdown_rect(list: Rect, control: Rect, options: &[String]) -> Rect {
    let widest = options.iter().map(|o| display_width(o).min(MAX_STATUS_CELLS)).max().unwrap_or(0);
    let width = u16::try_from(widest + 4).unwrap_or(u16::MAX).min(list.width);
    let height = u16::try_from(options.len() + 2).unwrap_or(u16::MAX).min(list.bottom().saturating_sub(control.bottom()));
    let x = control.right().saturating_sub(width).max(list.x);
    Rect::new(x, control.bottom(), width, height)
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

/// As `matching_features`, over rows parallel to `items`. A query matches the
/// name or description only, ignoring case; the status is never searched (it
/// has its own filter).
pub fn matching_rows(_rows: &[FeatureRow], items: &[(String, Option<String>)], query: &str) -> Vec<usize> {
    matching_rows_visiting(_rows, items, query, |_| {})
}

/// As `matching_rows`, calling `on_visit` with the index of every row it
/// examines, so tests can assert the filter does a single pass.
pub fn matching_rows_visiting(
    _rows: &[FeatureRow],
    items: &[(String, Option<String>)],
    query: &str,
    mut on_visit: impl FnMut(usize),
) -> Vec<usize> {
    let needle = query.to_lowercase();
    items
        .iter()
        .enumerate()
        .filter(|(index, (name, description))| {
            on_visit(*index);
            needle.is_empty()
                || name.to_lowercase().contains(&needle)
                || description.as_deref().is_some_and(|d| d.to_lowercase().contains(&needle))
        })
        .map(|(index, _)| index)
        .collect()
}

/// Rows taken by the search field: a bordered box, or one plain row when the
/// popup is too short to show the box and at least a few list rows.
const SEARCH_BOX_ROWS: u16 = 3;
const SEARCH_BOX_MIN_LIST_ROWS: u16 = 3;

fn search_rows(area: Rect, search: &SearchState) -> u16 {
    if !search_field_shown(search) || area.height == 0 {
        0
    } else if area.height >= HEADER_ROWS + SEARCH_BOX_ROWS + SEARCH_BOX_MIN_LIST_ROWS {
        SEARCH_BOX_ROWS
    } else {
        1
    }
}

/// The popup area as the list handlers must see it: shorter at the top by the
/// rows the search field takes (a three-row box, or one row when the popup is
/// short), so `list_area` of the result is exactly where the filtered rows
/// are rendered.
pub fn search_area(area: Rect, search: &SearchState) -> Rect {
    let rows = search_rows(area, search);
    Rect::new(area.x, area.y + rows, area.width, area.height - rows)
}

const SEARCH_PLACEHOLDER: &str = "Search by feature name…";

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
    render_search_in(frame, frame.area(), model, rows, items, None, search, state, &|_| None, None);
}

fn render_search_in(
    frame: &mut Frame,
    area: Rect,
    model: &PopupModel,
    rows: &[FeatureRow],
    items: &[(String, Option<String>)],
    status_filter: Option<&str>,
    search: &SearchState,
    state: &FeatureListState,
    depends: &dyn Fn(usize) -> Option<String>,
    marked: Option<&dyn Fn(usize) -> bool>,
) {
    render_header(frame, area, model);
    let full = list_area(area);
    if full.is_empty() {
        return;
    }
    let rows_taken = search_rows(area, search);
    if rows_taken > 0 {
        let query = search.query.chars().filter(|c| !c.is_control()).collect::<String>();
        let boxed = rows_taken == SEARCH_BOX_ROWS;
        let field = Rect::new(full.x, full.y, full.width, rows_taken.min(full.height));
        let inner = if boxed {
            let block = Block::bordered().border_type(BorderType::Rounded).title("Search");
            let inner = block.inner(field);
            frame.render_widget(block, field);
            inner
        } else {
            field
        };
        let width = usize::from(inner.width);
        let line = if query.is_empty() {
            // An open, empty field hints at what can be searched, dimmed so it
            // reads as a hint and never as a query.
            let hint = truncate_right(SEARCH_PLACEHOLDER, width.saturating_sub(1));
            Line::from(vec![
                Span::raw("/"),
                Span::styled(hint, Style::new().add_modifier(Modifier::DIM)),
            ])
        } else {
            Line::from(truncate_left(&format!("/{query}"), width))
        };
        if !inner.is_empty() {
            frame.render_widget(Paragraph::new(line), Rect::new(inner.x, inner.y, inner.width, 1));
        }
    }
    let list = list_area(search_area(area, search));
    let matches: Vec<usize> = matching_rows(rows, items, &search.query)
        .into_iter()
        .filter(|&index| rows.get(index).is_some_and(|row| status_matches(status_filter, &row.status)))
        .collect();
    let empty = if rows.is_empty() { "no features" } else { "no matches" };
    let matched = &matches;
    let marked_match = marked.map(|is_marked| move |i: usize| is_marked(matched[i]));
    render_rows(
        frame,
        list,
        matches.len(),
        |i| &rows[matches[i]],
        &|i| depends(matches[i]),
        marked_match.as_ref().map(|f| f as &dyn Fn(usize) -> bool),
        state,
        empty,
    );
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

/// Narrowest width a section box is drawn at; below it the plain lines are used.
const SECTION_MIN_WIDTH: usize = 8;

/// One bordered section as rows: a titled top rule, `body` wrapped inside
/// `│ … │`, and a bottom rule, every line exactly `width` columns. Each row
/// carries whether it is a status-tag row (`tagged` marks every body row;
/// rules never are).
fn section_rows(title: &str, body: &[String], width: usize, tagged: bool) -> Vec<(String, bool)> {
    let inner = width.saturating_sub(4);
    let title = truncate_right(&format!(" {title} "), width.saturating_sub(2));
    let rule = width.saturating_sub(2 + display_width(&title));
    let mut lines = vec![(format!("╭{title}{}╮", "─".repeat(rule)), false)];
    for text in body {
        let pad = inner.saturating_sub(display_width(text));
        lines.push((format!("│ {text}{} │", " ".repeat(pad)), tagged));
    }
    lines.push((format!("╰{}╯", "─".repeat(width.saturating_sub(2))), false));
    lines
}

/// The detail view as bordered sections for `width` columns: the name and
/// status, Description, Test file (when recorded) and Dependencies. Text is
/// wrapped, never cut, and control characters are replaced.
pub fn detail_section_lines(detail: &FeatureDetail, width: usize) -> Vec<String> {
    detail_section_rows(detail, width).into_iter().map(|(text, _)| text).collect()
}

/// `detail_section_lines` with a flag per row: true for the rows that end in
/// a status tag (the Feature and Dependencies bodies), so colouring never has
/// to infer a section from its (possibly truncated) heading.
fn detail_section_rows(detail: &FeatureDetail, width: usize) -> Vec<(String, bool)> {
    if width < SECTION_MIN_WIDTH {
        return feature_detail_lines(detail, width).into_iter().map(|text| (text, false)).collect();
    }
    let inner = width - 4;
    let clean = |text: &str| -> String {
        text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
    };
    let mut rows = section_rows(
        "Feature",
        &[name_with_tag("", &clean(&detail.name), &format!("[{}]", clean(&detail.status)), inner)],
        width,
        true,
    );
    let description = match &detail.description {
        Some(description) => wrap_hard(&clean(description), inner),
        None => vec![truncate_right("(no description)", inner)],
    };
    rows.extend(section_rows("Description", &description, width, false));
    if let Some(test_file) = &detail.test_file {
        rows.extend(section_rows("Test file", &wrap_hard(&clean(test_file), inner), width, false));
    }
    let dependencies: Vec<String> = if detail.dependencies.is_empty() {
        vec![truncate_right("(none)", inner)]
    } else {
        detail
            .dependencies
            .iter()
            .map(|dependency| {
                let status = match &dependency.status {
                    Some(status) => clean(status),
                    None => "not in ledger".to_string(),
                };
                name_with_tag("", &clean(&dependency.name), &format!("({status})"), inner)
            })
            .collect()
    };
    rows.extend(section_rows("Dependencies", &dependencies, width, !detail.dependencies.is_empty()));
    rows
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
    render_detail_lines(frame, area, &lines, &[], scroll);
}

/// Draw only the visible slice of already laid-out `lines`, from `scroll`
/// (clamped so the last line can reach the bottom and no further). Rows
/// flagged in `tagged` take their trailing status tag's list colour; the
/// status text is always drawn, colour only adds to it.
fn render_detail_lines(frame: &mut Frame, area: Rect, lines: &[String], tagged: &[bool], scroll: usize) {
    let height = usize::from(area.height);
    let start = scroll.min(lines.len().saturating_sub(height));
    let shown: Vec<Line> = lines
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(index, line)| {
            if tagged.get(index).copied().unwrap_or(false) { status_line(line) } else { Line::from(line.as_str()) }
        })
        .collect();
    frame.render_widget(Paragraph::new(shown), area);
}

/// `row` with its trailing `[status]` / `(status)` tag styled by the status,
/// when it has one (the tag sits before the closing border).
fn status_line(row: &str) -> Line<'_> {
    let body = row.trim_end_matches('│').trim_end();
    let close = match body.chars().last() {
        Some(']') => '[',
        Some(')') => '(',
        _ => return Line::from(row),
    };
    let Some(open) = body.rfind(close) else {
        return Line::from(row);
    };
    let tag = &body[open..];
    let status = &tag[1..tag.len() - 1];
    if status.is_empty() || !row.starts_with('│') {
        return Line::from(row);
    }
    Line::from(vec![
        Span::raw(&row[..open]),
        Span::styled(tag, status_style(status)),
        Span::raw(&row[open + tag.len()..]),
    ])
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

/// The button that promotes the marked drafts, `count` of them.
fn bulk_label(count: usize) -> String {
    format!("[ Promote {count} to pending (p) ]")
}

/// Splits `area` into the list area and, when marked drafts can be promoted,
/// the one-row bulk button at the bottom.
fn bulk_split(area: Rect, count: usize) -> (Rect, Option<Rect>) {
    if count == 0 || area.height == 0 || area.width == 0 {
        return (area, None);
    }
    let width = u16::try_from(bulk_label(count).chars().count()).unwrap_or(u16::MAX).min(area.width);
    let button = Rect::new(area.x, area.bottom() - 1, width, 1);
    (Rect::new(area.x, area.y, area.width, area.height - 1), Some(button))
}

/// Whether `mouse` pressed the bulk button drawn at the bottom of `area`.
fn bulk_hit(area: Rect, count: usize, mouse: MouseEvent) -> bool {
    mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && bulk_split(area, count).1.is_some_and(|b| b.contains(Position::new(mouse.column, mouse.row)))
}

fn render_bulk_button(frame: &mut Frame, area: Rect, count: usize) {
    if let (_, Some(button)) = bulk_split(area, count) {
        let label: String = bulk_label(count).chars().take(usize::from(button.width)).collect();
        frame.render_widget(Paragraph::new(label), button);
    }
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

    /// The user-level settings the Settings tab shows. Backends without
    /// settings keep this default, which says so.
    fn load_settings(&mut self) -> SettingsView {
        SettingsView { error: Some("settings are not available".into()), ..SettingsView::default() }
    }

    /// Installed harnesses with their model suggestions, for the Settings
    /// tab's picker. Free text stays possible, so empty is acceptable.
    fn harness_options(&mut self) -> Vec<HarnessOption> {
        Vec::new()
    }

    /// Persists both roles; the reason is shown inline when it fails.
    fn save_settings(&mut self, _implementer: &AgentSetting, _reviewer: &AgentSetting) -> Result<(), String> {
        Err("settings are not available".into())
    }

    /// Whether the last `load` failed only because the file does not exist
    /// yet, which a first save creates. Any other failure is not writable.
    fn ledger_absent(&self) -> bool {
        false
    }

    /// The editable fields of `name` as stored, or `None` when it cannot be
    /// read (the edit form is then not opened).
    fn feature_fields(&mut self, _name: &str) -> Option<FeatureFields> {
        None
    }

    /// Changes the fields of an existing feature (`fields.name` picks it).
    fn edit_feature(&mut self, _fields: &FeatureFields) -> FormSaveOutcome {
        FormSaveOutcome::Failed { reason: "editing features is not available".into() }
    }

    /// Appends a draft feature. Backends that cannot write say so.
    fn add_feature(&mut self, _fields: &FeatureFields) -> FormSaveOutcome {
        FormSaveOutcome::Failed { reason: "adding features is not available".into() }
    }

    /// Runs the generation agent on the chosen spec and returns what it
    /// proposed, checked against the current ledger. The reason is shown as a
    /// notice; manual creation stays available.
    fn generate_candidates(&mut self) -> Result<Vec<CandidateView>, String> {
        Err("generating from a spec is not available".into())
    }

    /// Starts generation without blocking the UI. The default runs
    /// `generate_candidates` to completion and returns a finished job.
    fn start_generation(&mut self) -> Result<Box<dyn GenerationJob>, String> {
        Ok(Box::new(FinishedJob(Some(self.generate_candidates()))))
    }

    /// Appends the accepted candidates in one atomic save. On failure nothing
    /// is written and the proposal stays available for a retry.
    fn accept_candidates(&mut self, _accepted: &[AcceptedCandidate]) -> FormSaveOutcome {
        FormSaveOutcome::Failed { reason: "adding generated features is not available".into() }
    }
}

/// A generation running off the UI thread.
pub trait GenerationJob {
    /// `Some` once, when the run has finished.
    fn poll(&mut self) -> Option<Result<Vec<CandidateView>, String>>;
    /// Stops the run; its result is discarded.
    fn cancel(&mut self);
}

struct FinishedJob(Option<Result<Vec<CandidateView>, String>>);

impl GenerationJob for FinishedJob {
    fn poll(&mut self) -> Option<Result<Vec<CandidateView>, String>> {
        self.0.take()
    }
    fn cancel(&mut self) {}
}

/// A harness and model name, as typed in the Settings tab. Names only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSetting {
    pub harness: String,
    pub model: String,
}

/// An installed harness and the model names it can be offered with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessOption {
    pub harness: String,
    pub models: Vec<String>,
}

/// What the Settings tab shows: the saved values and what the project's
/// `.tdd-config.json` says over them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsView {
    pub implementer: AgentSetting,
    pub reviewer: AgentSetting,
    /// The reviewer `.tdd-config.json` names, when it overrides the saved one.
    pub reviewer_override: Option<String>,
    /// Why the settings file could not be used, if it could not.
    pub error: Option<String>,
}

/// The tabs of the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Features,
    Settings,
}

const SETTINGS_TAB: &str = "Settings";
const SETTINGS_FILE: &str = "settings.toml";
const SETTINGS_HINT: &str = " ↑↓ move/pick · Enter edit/save · Esc cancel/back · Tab features ";
/// Settings fields in display order: (role, field name).
const SETTINGS_FIELDS: [(&str, &str); 4] =
    [("Implementer", "harness"), ("Implementer", "model"), ("Reviewer", "harness"), ("Reviewer", "model")];
/// Body row of each field, relative to the top of the body.
const SETTINGS_FIELD_ROWS: [u16; 4] = [1, 2, 4, 5];

/// Strips what a terminal would interpret from text that came from a file.
fn printable(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

struct SettingsState {
    view: SettingsView,
    selected: usize,
    /// Installed harnesses and model suggestions; the picker's choices.
    options: Vec<HarnessOption>,
    /// The text being typed into the selected field.
    editing: Option<String>,
    status: Option<String>,
}

impl SettingsState {
    fn value(&self, field: usize) -> &str {
        match field {
            0 => &self.view.implementer.harness,
            1 => &self.view.implementer.model,
            2 => &self.view.reviewer.harness,
            _ => &self.view.reviewer.model,
        }
    }

    /// Replaces the edited text with the next (or previous) suggestion.
    fn pick(&mut self, forward: bool) {
        let Some(current) = self.editing.clone() else {
            return;
        };
        let choices: Vec<String> = if self.selected % 2 == 0 {
            self.options.iter().map(|o| o.harness.clone()).collect()
        } else {
            let harness = if self.selected == 1 { &self.view.implementer.harness } else { &self.view.reviewer.harness };
            self.options.iter().find(|o| &o.harness == harness).map(|o| o.models.clone()).unwrap_or_default()
        };
        if choices.is_empty() {
            return;
        }
        let len = choices.len();
        let next = match (choices.iter().position(|c| *c == current), forward) {
            (Some(i), true) => (i + 1) % len,
            (Some(i), false) => (i + len - 1) % len,
            (None, true) => 0,
            (None, false) => len - 1,
        };
        self.editing = Some(choices[next].clone());
    }

    fn begin_edit(&mut self, field: usize) {
        self.selected = field.min(SETTINGS_FIELDS.len() - 1);
        self.editing = Some(self.value(self.selected).to_string());
        self.status = None;
    }

    /// Saves the typed value; on failure the edit stays open with the reason.
    fn commit(&mut self, backend: &mut dyn BrowserBackend) {
        let Some(text) = self.editing.clone() else {
            return;
        };
        let (mut implementer, mut reviewer) = (self.view.implementer.clone(), self.view.reviewer.clone());
        match self.selected {
            0 => implementer.harness = text,
            1 => implementer.model = text,
            2 => reviewer.harness = text,
            _ => reviewer.model = text,
        }
        // A role is saved whole: while one of its fields is still empty the
        // typed text stays as an unsaved draft instead of being rejected.
        let half_filled = [("Implementer", &implementer), ("Reviewer", &reviewer)]
            .into_iter()
            .find(|(_, role)| role.harness.trim().is_empty() != role.model.trim().is_empty());
        if let Some((name, _)) = half_filled {
            self.view.implementer = implementer;
            self.view.reviewer = reviewer;
            self.editing = None;
            self.status = Some(format!("{name}: fill both harness and model to save"));
            return;
        }
        match backend.save_settings(&implementer, &reviewer) {
            Ok(()) => {
                // Reload so overrides and validation reflect what was persisted.
                self.view = backend.load_settings();
                self.editing = None;
                self.status = Some("Saved".to_string());
            }
            Err(reason) => self.status = Some(format!("Not saved: {}", printable(&reason))),
        }
    }

    /// Returns true when the key leaves the tab.
    fn handle_key(&mut self, code: KeyCode, backend: &mut dyn BrowserBackend) -> bool {
        match (&mut self.editing, code) {
            (Some(_), KeyCode::Esc) => {
                self.editing = None;
                self.status = None;
            }
            (Some(_), KeyCode::Enter) => self.commit(backend),
            (Some(_), KeyCode::Down) => self.pick(true),
            (Some(_), KeyCode::Up) => self.pick(false),
            (Some(text), KeyCode::Backspace) => {
                text.pop();
            }
            (Some(text), KeyCode::Char(c)) if !c.is_control() => text.push(c),
            (Some(_), _) => {}
            (None, KeyCode::Esc | KeyCode::Tab) => return true,
            (None, KeyCode::Down) => self.selected = (self.selected + 1).min(SETTINGS_FIELDS.len() - 1),
            (None, KeyCode::Up) => self.selected = self.selected.saturating_sub(1),
            (None, KeyCode::Enter) => self.begin_edit(self.selected),
            (None, _) => {}
        }
        false
    }

    fn handle_mouse(&mut self, body: Rect, mouse: MouseEvent) {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }
        let hit = SETTINGS_FIELD_ROWS.iter().position(|&row| {
            let rect = Rect::new(body.x, body.y.saturating_add(row), body.width, 1).intersection(body);
            rect.contains((mouse.column, mouse.row).into())
        });
        if let Some(field) = hit {
            self.begin_edit(field);
        }
    }
}

/// The rectangle of each tab in the tab bar of `area`; the Settings tab is
/// left out when it does not fit. Empty when the chrome is not drawn.
fn tab_rects(area: Rect) -> Vec<(Tab, Rect)> {
    if chrome_body(area) == area {
        return Vec::new();
    }
    let width = |label: &str| u16::try_from(label.len() + 4).unwrap_or(u16::MAX);
    let features = Rect::new(area.x, area.y + 1, width(FEATURES_TAB).min(area.width), 3);
    let mut rects = vec![(Tab::Features, features)];
    let settings_width = width(SETTINGS_TAB);
    if features.right() + settings_width <= area.right() {
        rects.push((Tab::Settings, Rect::new(features.right(), area.y + 1, settings_width, 3)));
    }
    rects
}

const EMPTY_TITLE: &str = "No feature file yet";
const GENERATE_LABEL: &str = "[ Generate from Spec ]";

/// The Add and Generate buttons of the empty state, in `area` (the body).
fn empty_state_rects(area: Rect) -> (Rect, Rect) {
    let button = |row: u16, label: &str| {
        let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX).min(area.width);
        Rect::new(area.x, area.y.saturating_add(row).min(area.bottom()), width, 1)
    };
    (button(3, ADD_LABEL), button(5, GENERATE_LABEL))
}

/// The absent-file state: where the file will go and the two ways to start.
fn render_empty_state(frame: &mut Frame, area: Rect, error: &LedgerErrorView) {
    let width = usize::from(area.width);
    let dim = Style::new().fg(Color::DarkGray);
    let (add, generate) = empty_state_rects(area);
    let mut put = |rect: Rect, line: Line<'static>| {
        if rect.y < area.bottom() {
            frame.render_widget(Paragraph::new(line), Rect::new(area.x, rect.y, area.width, 1));
        }
    };
    let bold = Style::new().add_modifier(Modifier::BOLD);
    put(Rect::new(area.x, area.y, 0, 1), Line::from(Span::styled(EMPTY_TITLE, bold)));
    let path = truncate_left(&printable(&error.path), width);
    put(Rect::new(area.x, area.y + 1, 0, 1), Line::from(Span::styled(path, dim)));
    put(add, Line::from(Span::styled(ADD_LABEL, Style::new().fg(Color::Green))));
    let label = truncate_right(GENERATE_LABEL, width);
    put(generate, Line::from(Span::styled(label, Style::new().fg(Color::Green))));
}

const ADD_LABEL: &str = "[ + Add Feature ]";

/// The Add Feature button on the tab bar's middle row, right-aligned; `None`
/// when the chrome is not drawn or the tabs leave no room for it.
fn add_button_rect(full: Rect) -> Option<Rect> {
    let tabs_end = tab_rects(full).iter().map(|(_, rect)| rect.right()).max()?;
    let width = u16::try_from(ADD_LABEL.len()).ok()?;
    let x = full.right().checked_sub(width + 1)?;
    (x > tabs_end).then(|| Rect::new(x, full.y + 2, width, 1))
}

fn render_settings(frame: &mut Frame, body: Rect, state: &SettingsState) {
    let dim = Style::new().fg(Color::DarkGray);
    let mut put = |row: u16, line: Line<'static>| {
        if row < body.height {
            let rect = Rect::new(body.x, body.y + row, body.width, 1);
            frame.render_widget(Paragraph::new(line), rect);
        }
    };
    let width = usize::from(body.width);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    put(0, Line::from(Span::styled(truncate_right(" Implementer", width), bold)));
    put(3, Line::from(Span::styled(truncate_right(" Reviewer", width), bold)));
    for (field, ((_, name), row)) in SETTINGS_FIELDS.iter().zip(SETTINGS_FIELD_ROWS).enumerate() {
        let marker = if field == state.selected { ">" } else { " " };
        let value = match (&state.editing, field == state.selected) {
            (Some(text), true) => format!("{text}▏"),
            _ => printable(state.value(field)),
        };
        let style = if field == state.selected { Style::new().fg(Color::White) } else { Style::new() };
        let text = format!(" {marker} {name:<8} {value}");
        put(row, Line::from(Span::styled(truncate_right(&text, width), style)));
    }
    if let Some(reviewer) = &state.view.reviewer_override {
        let note = format!("   overridden by .tdd-config.json: {}", printable(reviewer));
        put(6, Line::from(Span::styled(truncate_right(&note, width), Style::new().fg(Color::Yellow))));
    }
    if let Some(error) = &state.view.error {
        let text = format!(" {}", printable(error));
        put(7, Line::from(Span::styled(truncate_right(&text, width), Style::new().fg(Color::Red))));
    }
    if let Some(status) = &state.status {
        put(8, Line::from(truncate_right(&format!(" {status}"), width)));
    }
    if body.height > 0 {
        put(body.height - 1, Line::from(Span::styled(truncate_right(SETTINGS_HINT, width), dim)));
    }
}

/// `orq ui`'s popup on the real terminal: the feature browser between `enter`
/// and `leave`.
pub fn run_browser_on_terminal(model: &PopupModel, backend: &mut dyn BrowserBackend) -> Result<(), Box<dyn Error>> {
    let crossterm = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(crossterm)?;
    run_browser_session(&mut CrosstermMode::default(), &mut terminal, model, backend, &mut CrosstermEvents)
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
    /// Names of the rows marked with Space; never written anywhere.
    marked: HashSet<String>,
    /// The status the dropdown filters to; `None` is "All".
    status_filter: Option<String>,
    /// The highlighted option while the dropdown is open.
    dropdown: Option<usize>,
    tab: Tab,
    settings: SettingsState,
    /// The open Add Feature form; it covers the Features body.
    form: Option<FormState>,
    /// The open generation preview; it covers the Features body.
    preview: Option<PreviewState>,
    /// The ledger file does not exist yet (and nothing else is wrong).
    absent: bool,
    /// A generation running in the background; only Esc is handled meanwhile.
    generating: Option<Box<dyn GenerationJob>>,
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(job) = self.generating.as_mut() {
            job.cancel();
        }
    }
}

struct DetailView {
    detail: FeatureDetail,
    row: FeatureRow,
    scroll: usize,
    /// `detail` wrapped for `width` columns; rebuilt only when the width
    /// changes (a reload builds a fresh view, so it starts empty).
    lines: Vec<String>,
    /// Per row of `lines`: whether it ends in a status tag to colour.
    tagged: Vec<bool>,
    width: Option<usize>,
}

impl DetailView {
    fn layout(&mut self, width: usize) -> (&[String], &[bool]) {
        if self.width != Some(width) {
            let (lines, tagged) = detail_section_rows(&self.detail, width).into_iter().unzip();
            self.lines = lines;
            self.tagged = tagged;
            self.width = Some(width);
            DETAIL_LAYOUTS.with(|count| count.set(count.get() + 1));
        }
        (&self.lines, &self.tagged)
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
        if !self.marked.is_empty() {
            let present: HashSet<&str> = self.entries.iter().map(|e| e.name.as_str()).collect();
            self.marked.retain(|name| present.contains(name.as_str()));
        }
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
            tagged: Vec::new(),
            width: None,
        })
    }

    fn rows(&self) -> &[FeatureRow] {
        &self.rows
    }

    fn items(&self) -> &[(String, Option<String>)] {
        &self.items
    }

    /// Indexes of the rows the name search and the status filter both pass.
    fn visible(&self) -> Vec<usize> {
        let filter = self.status_filter.as_deref();
        matching_rows(&self.rows, &self.items, &self.search.query)
            .into_iter()
            .filter(|&i| self.rows.get(i).is_some_and(|row| status_matches(filter, &row.status)))
            .collect()
    }

    /// How many dropdown options the current layout can show (0 when the
    /// control itself does not fit); `area` is the chrome body, as drawn.
    fn dropdown_shown(&self, area: Rect) -> usize {
        let body = body_area(area, self.notice.is_some());
        let selected = self.selected_row();
        let (list, _, _) = list_layout(body, selected.as_ref(), self.marked_drafts());
        let options = status_options(&self.rows);
        filter_control_rect(list, self.status_filter.as_deref())
            .map_or(0, |c| usize::from(Block::bordered().inner(dropdown_rect(list, c, &options)).height))
    }

    /// Opens the dropdown on the active option, or closes it.
    fn toggle_dropdown(&mut self) {
        self.dropdown = match self.dropdown {
            Some(_) => None,
            None => {
                let options = status_options(&self.rows);
                Some(active_option(&options, self.status_filter.as_deref()))
            }
        };
    }

    /// Applies option `index` and closes the dropdown; the selection restarts.
    fn choose_status(&mut self, index: usize) {
        let options = status_options(&self.rows);
        self.status_filter = match options.get(index) {
            Some(option) if index > 0 => Some(option.clone()),
            _ => None,
        };
        self.dropdown = None;
        self.list.selected = None;
        self.list.offset = 0;
    }

    /// The row the selection indexes once the search and status filters apply.
    fn selected_row(&self) -> Option<FeatureRow> {
        let matches = self.visible();
        let index = self.list.selected?.min(matches.len().checked_sub(1)?);
        self.rows.get(*matches.get(index)?).cloned()
    }

    /// Scroll the open detail by `delta` lines within `text`; true when it moved.
    fn scroll_detail(&mut self, text: Rect, delta: isize) -> bool {
        let Some(open) = &mut self.detail else {
            return false;
        };
        let total = open.layout(usize::from(text.width)).0.len();
        let max = total.saturating_sub(usize::from(text.height));
        let next = open.scroll.min(max).saturating_add_signed(delta).min(max);
        let moved = next != open.scroll;
        open.scroll = next;
        moved
    }

    /// Reads the ledger again and notes whether it is merely absent.
    fn reload(&mut self, backend: &mut dyn BrowserBackend) {
        self.apply_load(backend.load());
        self.absent = self.error.is_some() && backend.ledger_absent();
    }

    fn apply_load(&mut self, loaded: Result<Vec<BrowserEntry>, LedgerErrorView>) {
        match loaded {
            Ok(entries) => {
                self.set_entries(entries);
                self.error = None;
            }
            Err(error) => {
                // The ledger could not be read, not emptied: marks that
                // survived the writes are still the user's to retry.
                let marked = std::mem::take(&mut self.marked);
                self.set_entries(Vec::new());
                self.marked = marked;
                self.error = Some(error);
            }
        }
    }

    /// How many marked features are drafts, i.e. what the bulk button would promote.
    fn marked_drafts(&self) -> usize {
        if self.marked.is_empty() {
            return 0;
        }
        self.entries.iter().filter(|e| e.status == "draft" && self.marked.contains(&e.name)).count()
    }

    /// Promotes every marked draft, one write each in list order, then
    /// reloads. Saved marks are cleared and failed ones stay; marks on
    /// anything that is not a draft are skipped and counted.
    fn promote_marked(&mut self, backend: &mut dyn BrowserBackend) {
        let mut saved = 0usize;
        let mut skipped = 0usize;
        let mut failed: Vec<(String, String)> = Vec::new();
        let targets: Vec<(String, bool)> = self
            .entries
            .iter()
            .filter(|e| self.marked.contains(&e.name))
            .map(|e| (e.name.clone(), e.status == "draft"))
            .collect();
        for (name, is_draft) in targets {
            if !is_draft {
                skipped += 1;
                continue;
            }
            match backend.promote(&name) {
                PromoteOutcome::Saved => {
                    saved += 1;
                    self.marked.remove(&name);
                }
                PromoteOutcome::Failed { reason } => {
                    let reason: String = reason.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
                    failed.push((name, reason));
                }
            }
        }
        self.reload(backend);
        let mut parts = vec![format!("saved {saved}")];
        if !failed.is_empty() {
            let detail: Vec<String> = failed.iter().map(|(n, r)| format!("{n}: {r}")).collect();
            parts.push(format!("{} failed ({})", failed.len(), detail.join("; ")));
        }
        if skipped > 0 {
            parts.push(format!("{skipped} skipped (not draft)"));
        }
        self.notice = Some(parts.join(" · "));
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

    fn open_add_form(&mut self) {
        let candidates = self.entries.iter().map(|e| e.name.clone()).collect();
        self.form = Some(FormState::new_add(candidates));
        self.notice = None;
    }

    /// Opens the form on `name` (the open detail's feature, else the selected
    /// row). Does nothing when the backend cannot supply its fields.
    fn open_edit_form(&mut self, backend: &mut dyn BrowserBackend) {
        let target = match &self.detail {
            Some(open) => Some(open.row.clone()),
            None => self.selected_row(),
        };
        let Some(row) = target else {
            return;
        };
        let Some(current) = backend.feature_fields(&row.name) else {
            return;
        };
        let candidates = self.entries.iter().map(|e| e.name.clone()).collect();
        self.form = Some(FormState::new_edit(candidates, &current, &row.status));
        self.notice = None;
    }

    /// Saves the open form through `backend`. `area` is the chrome body.
    fn submit_form(&mut self, backend: &mut dyn BrowserBackend, area: Rect) {
        let Some(fields) = self.form.as_ref().map(FormState::fields) else {
            return;
        };
        let editing = self.form.as_ref().is_some_and(FormState::is_edit);
        let outcome = if editing { backend.edit_feature(&fields) } else { backend.add_feature(&fields) };
        match outcome {
            FormSaveOutcome::Saved => {
                self.form = None;
                self.reload(backend);
                self.notice = None;
                self.reveal(&fields.name, area);
            }
            FormSaveOutcome::Invalid { reason } | FormSaveOutcome::Failed { reason } => {
                if let Some(form) = &mut self.form {
                    form.error = Some(reason);
                }
            }
            FormSaveOutcome::Conflict { reason } => {
                self.form = None;
                self.reload(backend);
                let reason = printable(&reason);
                self.notice = Some(format!("The feature file changed ({reason}); review it and try again"));
            }
        }
    }

    /// Selects `name` and scrolls it into view; when the search or status
    /// filter hides it, says so instead.
    fn reveal(&mut self, name: &str, area: Rect) {
        let Some(index) = self.rows.iter().position(|r| r.name == name) else {
            return;
        };
        let visible = self.visible();
        match visible.iter().position(|&i| i == index) {
            Some(position) => {
                self.list.selected = Some(position);
                let body = body_area(area, self.notice.is_some());
                let selected = self.selected_row();
                let (list, _, _) = list_layout(body, selected.as_ref(), self.marked_drafts());
                self.list.offset = self.list.window(search_area(list, &self.search), visible.len());
            }
            None => {
                let filter = self.status_filter.as_deref();
                let by_status = self.rows.get(index).is_some_and(|r| !status_matches(filter, &r.status));
                let by = if by_status { "the status filter" } else { "the search" };
                self.notice = Some(format!("Saved \"{}\", hidden by {by}", printable(name)));
            }
        }
    }

    /// Runs the generation agent and opens the preview of its proposal.
    fn open_generation(&mut self, backend: &mut dyn BrowserBackend) {
        self.notice = None;
        match backend.start_generation() {
            Ok(job) => {
                self.generating = Some(job);
                self.notice = Some("Generating… (Esc to cancel)".to_string());
            }
            Err(reason) => self.notice = Some(format!("Generation failed: {}", printable(&reason))),
        }
    }

    /// Opens the preview of a finished generation, or reports its failure.
    fn finish_generation(&mut self, result: Result<Vec<CandidateView>, String>) {
        self.generating = None;
        self.notice = None;
        match result {
            Ok(candidates) if candidates.is_empty() => {
                self.notice = Some("The agent proposed no features".to_string());
            }
            Ok(candidates) => self.preview = Some(PreviewState::new(candidates)),
            Err(reason) => self.notice = Some(format!("Generation failed: {}", printable(&reason))),
        }
    }

    /// Adds the accepted candidates; `area` is the chrome body.
    fn accept_preview(&mut self, backend: &mut dyn BrowserBackend, accepted: Vec<AcceptedCandidate>, area: Rect) {
        match backend.accept_candidates(&accepted) {
            FormSaveOutcome::Saved => {
                self.preview = None;
                self.reload(backend);
                self.notice = None;
                if let Some(first) = accepted.first() {
                    self.reveal(&first.name, area);
                }
                self.notice.get_or_insert_with(|| format!("Added {} generated feature(s)", accepted.len()));
            }
            FormSaveOutcome::Invalid { reason } | FormSaveOutcome::Failed { reason } => {
                if let Some(preview) = &mut self.preview {
                    preview.error = Some(format!("Not saved: {reason}"));
                }
            }
            FormSaveOutcome::Conflict { reason } => {
                self.preview = None;
                self.reload(backend);
                let reason = printable(&reason);
                self.notice = Some(format!("The feature file changed ({reason}); generate again"));
            }
        }
    }

    fn open_settings(&mut self, backend: &mut dyn BrowserBackend) {
        self.settings = SettingsState {
            view: backend.load_settings(),
            options: backend.harness_options(),
            selected: 0,
            editing: None,
            status: None,
        };
        self.tab = Tab::Settings;
    }

    fn draw(&mut self, frame: &mut Frame, model: &PopupModel) {
        let full = frame.area();
        let area = render_chrome_tabs(frame, full, self.tab);
        if self.tab == Tab::Settings {
            render_settings(frame, area, &self.settings);
            return;
        }
        if let Some(preview) = &self.preview {
            preview.render(frame, body_area(area, self.notice.is_some()));
            render_notice(frame, area, self.notice.as_deref());
            return;
        }
        if let Some(form) = &self.form {
            form.render(frame, body_area(area, self.notice.is_some()));
            render_notice(frame, area, self.notice.as_deref());
            return;
        }
        if self.error.is_none() {
            if let Some(button) = add_button_rect(full) {
                let style = Style::new().fg(Color::Green);
                frame.render_widget(Paragraph::new(Span::styled(ADD_LABEL, style)), button);
            }
        }
        if let (Some(error), true) = (&self.error, self.absent) {
            render_empty_state(frame, area, error);
            render_notice(frame, area, self.notice.as_deref());
            return;
        }
        if let Some(error) = &self.error {
            render_ledger_error_in(frame, area, model, error);
            render_notice(frame, area, self.notice.as_deref());
            return;
        }
        let notice = self.notice.is_some();
        if let Some(open) = &mut self.detail {
            let body = body_area(area, notice);
            let (text, _) = promote_split(body, Some(&open.row));
            let panel = text.intersection(area);
            let scroll = open.scroll;
            if !panel.is_empty() {
                let text = detail_viewport(panel);
                let mut position = None;
                if !text.is_empty() {
                    let total = open.layout(usize::from(text.width)).0.len();
                    position = scroll_position(scroll, usize::from(text.height), total);
                }
                frame.render_widget(detail_block(&open.detail.name, panel.width, position.as_deref()), panel);
                if !text.is_empty() {
                    let (lines, tagged) = open.layout(usize::from(text.width));
                    render_detail_lines(frame, text, lines, tagged, scroll);
                }
            }
            render_promote_button(frame, body, Some(&open.row));
            render_notice(frame, area, self.notice.as_deref());
            return;
        }
        let body_height = area.height.saturating_sub(u16::from(self.notice.is_some()));
        let body = Rect::new(area.x, area.y, area.width, body_height);
        let selected = self.selected_row();
        let bulk = self.marked_drafts();
        let (list, promote_area, panel) = list_layout(body, selected.as_ref(), bulk);
        let entries = &self.entries;
        let depends = |index: usize| entries.get(index).map(|e| dependency_summary(&e.depends_on));
        let marks = &self.marked;
        let is_marked = |index: usize| entries.get(index).is_some_and(|e| marks.contains(&e.name));
        let marked: Option<&dyn Fn(usize) -> bool> = if marks.is_empty() { None } else { Some(&is_marked) };
        render_search_in(
            frame,
            list,
            model,
            self.rows(),
            self.items(),
            self.status_filter.as_deref(),
            &self.search,
            &self.list,
            &depends,
            marked,
        );
        let filter = self.status_filter.as_deref();
        if let Some(control) = filter_control_rect(list, filter) {
            frame.render_widget(ratatui::widgets::Clear, control);
            frame.render_widget(Paragraph::new(filter_label(filter, list.width)), control);
            if let Some(highlight) = self.dropdown {
                let options = status_options(&self.rows);
                let rect = dropdown_rect(list, control, &options);
                frame.render_widget(ratatui::widgets::Clear, rect);
                let block = Block::bordered().border_type(BorderType::Rounded);
                let inner = block.inner(rect);
                frame.render_widget(block, rect);
                let width = usize::from(inner.width);
                let active = active_option(&options, filter);
                let top = dropdown_top(usize::from(inner.height), highlight);
                for (i, option) in options.iter().enumerate().skip(top).take(usize::from(inner.height)) {
                    let marker = if i == highlight { "> " } else { "  " };
                    let at = i - top;
                    let line = truncate_right(&format!("{marker}{option}"), width);
                    let style = if i == active { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
                    let row = Rect::new(inner.x, inner.y + u16::try_from(at).unwrap_or(0), inner.width, 1);
                    frame.render_widget(Paragraph::new(Span::styled(line, style)), row);
                }
            }
        }
        match panel {
            Some(panel) => render_selected_panel(frame, panel, promote_area, selected.as_ref(), marks.len(), bulk),
            None if bulk > 0 => render_bulk_button(frame, body, bulk),
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
        marked: HashSet::new(),
        status_filter: None,
        dropdown: None,
        tab: Tab::Features,
        settings: SettingsState {
            view: SettingsView::default(),
            options: Vec::new(),
            selected: 0,
            editing: None,
            status: None,
        },
        form: None,
        preview: None,
        absent: false,
        generating: None,
    };
    browser.reload(backend);
    let mut dirty = true;
    loop {
        if dirty {
            terminal.draw(|frame| browser.draw(frame, model))?;
            dirty = false;
        }
        let event = if browser.generating.is_some() {
            let finished = browser.generating.as_mut().and_then(|job| job.poll());
            if let Some(result) = finished {
                browser.finish_generation(result);
                dirty = true;
                continue;
            }
            match events.poll_event(std::time::Duration::from_millis(50))? {
                Some(Event::Key(key)) if key.code == KeyCode::Esc => {
                    if let Some(mut job) = browser.generating.take() {
                        job.cancel();
                    }
                    browser.notice = Some("Generation cancelled".to_string());
                    dirty = true;
                }
                Some(Event::Resize(..)) => dirty = true,
                _ => {}
            }
            continue;
        } else {
            events.next_event()?
        };
        match event {
            Event::Key(key) => {
                if let Some(preview) = browser.preview.as_mut() {
                    match preview.handle_key(key.code) {
                        PreviewAction::Cancel => browser.preview = None,
                        PreviewAction::Accept(accepted) => {
                            let area = chrome_body(Rect::from((Position::ORIGIN, terminal.size()?)));
                            browser.accept_preview(backend, accepted, area);
                        }
                        PreviewAction::None => {}
                    }
                    dirty = true;
                    continue;
                }
                if let Some(form) = browser.form.as_mut() {
                    match form.handle_key(key) {
                        FormAction::Cancel => browser.form = None,
                        FormAction::Save => {
                            let area = chrome_body(Rect::from((Position::ORIGIN, terminal.size()?)));
                            browser.submit_form(backend, area);
                        }
                        FormAction::None => {}
                    }
                    dirty = true;
                    continue;
                }
                if browser.tab == Tab::Settings {
                    if browser.settings.handle_key(key.code, backend) {
                        browser.tab = Tab::Features;
                    }
                    dirty = true;
                    continue;
                }
                if key.code == KeyCode::Tab
                    && !browser.search.active
                    && browser.detail.is_none()
                    && browser.dropdown.is_none()
                {
                    browser.open_settings(backend);
                    dirty = true;
                    continue;
                }
                if browser.error.is_some() {
                    match key.code {
                        KeyCode::Esc => return Ok(()),
                        KeyCode::Char('a') if browser.absent => {
                            browser.open_add_form();
                            dirty = true;
                        }
                        KeyCode::Char('g') if browser.absent => {
                            browser.open_generation(backend);
                            dirty = true;
                        }
                        code if reload_requested(code) => {
                            browser.reload(backend);
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
                        detail_viewport(promote_split(body_area(area, browser.notice.is_some()), Some(&row)).0)
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
                            browser.reload(backend);
                            dirty = true;
                        }
                        KeyCode::Char('e') => {
                            browser.open_edit_form(backend);
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
                if let Some(highlight) = browser.dropdown {
                    let area = chrome_body(Rect::from((Position::ORIGIN, terminal.size()?)));
                    if browser.dropdown_shown(area) == 0 {
                        // The layout shrank under an open dropdown: no option is visible.
                        browser.dropdown = None;
                        dirty = true;
                        continue;
                    }
                    let last = status_options(&browser.rows).len().saturating_sub(1);
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('f') => browser.dropdown = None,
                        KeyCode::Down => browser.dropdown = Some((highlight + 1).min(last)),
                        KeyCode::Up => browser.dropdown = Some(highlight.saturating_sub(1)),
                        KeyCode::Enter => browser.choose_status(highlight),
                        _ => {}
                    }
                    dirty = true;
                    continue;
                }
                if key.code == KeyCode::Char('e') && !browser.search.active {
                    browser.open_edit_form(backend);
                    dirty = true;
                    continue;
                }
                if key.code == KeyCode::Char('a') && !browser.search.active {
                    browser.open_add_form();
                    dirty = true;
                    continue;
                }
                if key.code == KeyCode::Char('g') && !browser.search.active {
                    browser.open_generation(backend);
                    dirty = true;
                    continue;
                }
                if key.code == KeyCode::Char('f') && !browser.search.active {
                    let area = chrome_body(Rect::from((Position::ORIGIN, terminal.size()?)));
                    if browser.dropdown_shown(area) > 0 {
                        browser.toggle_dropdown();
                    }
                    dirty = true;
                    continue;
                }
                if key.code == KeyCode::Char('p') && !browser.search.active && browser.marked_drafts() > 0 {
                    browser.promote_marked(backend);
                    dirty = true;
                    continue;
                }
                if key.code == KeyCode::Char(' ') && !browser.search.active {
                    if let Some(row) = browser.selected_row() {
                        if !browser.marked.remove(&row.name) {
                            browser.marked.insert(row.name);
                        }
                        dirty = true;
                    }
                    continue;
                }
                if key.code == KeyCode::Esc && !search_field_shown(&browser.search) && !browser.marked.is_empty() {
                    browser.marked.clear();
                    dirty = true;
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
                    browser.reload(backend);
                } else {
                    let body = body_area(area, browser.notice.is_some());
                    let (list, _, _) = list_layout(body, selected.as_ref(), browser.marked_drafts());
                    let len = browser.visible().len();
                    browser.list.handle_key(key.code, search_area(list, &browser.search), len);
                }
                dirty = true;
            }
            Event::Mouse(mouse) => {
                let acted_on = matches!(
                    mouse.kind,
                    MouseEventKind::Down(MouseButton::Left) | MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                );
                if !acted_on {
                    continue;
                }
                let full = Rect::from((Position::ORIGIN, terminal.size()?));
                if let Some(preview) = browser.preview.as_mut() {
                    let body = body_area(chrome_body(full), browser.notice.is_some());
                    preview.handle_mouse(body, mouse);
                    dirty = true;
                    continue;
                }
                if let Some(form) = browser.form.as_mut() {
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                        let body = body_area(chrome_body(full), browser.notice.is_some());
                        match form.click(body, mouse.column, mouse.row) {
                            FormAction::Cancel => browser.form = None,
                            FormAction::Save => browser.submit_form(backend, chrome_body(full)),
                            FormAction::None => {}
                        }
                        dirty = true;
                    }
                    continue;
                }
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    let press = Position::new(mouse.column, mouse.row);
                    if browser.tab == Tab::Features
                        && browser.error.is_none()
                        && add_button_rect(full).is_some_and(|b| b.contains(press))
                    {
                        browser.open_add_form();
                        dirty = true;
                        continue;
                    }
                    let target = tab_rects(full).into_iter().find(|(_, rect)| rect.contains(press)).map(|(t, _)| t);
                    match target {
                        Some(Tab::Settings) if browser.tab != Tab::Settings => {
                            browser.open_settings(backend);
                            dirty = true;
                            continue;
                        }
                        Some(Tab::Features) if browser.tab != Tab::Features => {
                            browser.tab = Tab::Features;
                            dirty = true;
                            continue;
                        }
                        _ => {}
                    }
                }
                if browser.tab == Tab::Settings {
                    browser.settings.handle_mouse(chrome_body(full), mouse);
                    dirty = true;
                    continue;
                }
                if browser.error.is_some() {
                    let press = Position::new(mouse.column, mouse.row);
                    let (add, generate) = empty_state_rects(chrome_body(full));
                    if browser.absent && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                        if add.contains(press) {
                            browser.open_add_form();
                            dirty = true;
                        } else if generate.contains(press) {
                            browser.open_generation(backend);
                            dirty = true;
                        }
                    }
                    continue;
                }
                let area = chrome_body(full);
                let body = body_area(area, browser.notice.is_some());
                if let Some(open) = &browser.detail {
                    let row = open.row.clone();
                    let text = detail_viewport(promote_split(body, Some(&row)).0);
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
                let bulk = browser.marked_drafts();
                let (list, promote_area, _) = list_layout(body, selected.as_ref(), bulk);
                let control = filter_control_rect(list, browser.status_filter.as_deref());
                let press = Position::new(mouse.column, mouse.row);
                if browser.dropdown.is_some() && browser.dropdown_shown(area) == 0 {
                    browser.dropdown = None;
                }
                if let Some(highlight) = browser.dropdown {
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                        let options = status_options(&browser.rows);
                        let hit = control.map(|c| dropdown_rect(list, c, &options)).and_then(|rect| {
                            let inner = Block::bordered().inner(rect);
                            let row = usize::from(press.y.checked_sub(inner.y)?);
                            let index = dropdown_top(usize::from(inner.height), highlight) + row;
                            (inner.contains(press) && index < options.len()).then_some(index)
                        });
                        match hit {
                            Some(index) => browser.choose_status(index),
                            None => browser.dropdown = None,
                        }
                    } else {
                        browser.dropdown = Some(highlight);
                    }
                } else if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && control.is_some_and(|c| c.contains(press))
                    && browser.dropdown_shown(area) > 0
                {
                    browser.toggle_dropdown();
                } else if bulk > 0 && bulk_hit(promote_area, bulk, mouse) {
                    browser.promote_marked(backend);
                } else if let Some(intent) = promote_intent_for_mouse(promote_area, selected.as_ref(), mouse) {
                    browser.promote(backend, intent);
                } else {
                    let len = browser.visible().len();
                    browser.list.handle_mouse(search_area(list, &browser.search), len, mouse);
                }
                dirty = true;
            }
            Event::Resize(..) => {
                let area = chrome_body(Rect::from((Position::ORIGIN, terminal.size()?)));
                if browser.dropdown.is_some() && browser.dropdown_shown(area) == 0 {
                    browser.dropdown = None;
                }
                dirty = true;
            }
            _ => {}
        }
    }
}

/// Rows the selected-feature panel needs (borders included).
pub const PANEL_ROWS: u16 = 3;
const PANEL_MIN_WIDTH: u16 = 30;
const PANEL_MIN_BODY_HEIGHT: u16 = 14;
const PANEL_HINT: &str = " ↑↓ move · Enter open · / search · f status · r reload · Esc close ";

/// Splits the browser body into the list and, on a roomy viewport, the
/// selected-feature panel below it. Without a panel the promote button
/// keeps its place under the list.
fn list_layout(body: Rect, selected: Option<&FeatureRow>, bulk: usize) -> (Rect, Rect, Option<Rect>) {
    if body.width >= PANEL_MIN_WIDTH && body.height >= PANEL_MIN_BODY_HEIGHT {
        let list = Rect::new(body.x, body.y, body.width, body.height - PANEL_ROWS);
        let panel = Rect::new(body.x, list.bottom(), body.width, PANEL_ROWS);
        let inner = Rect::new(panel.x + 1, panel.y + 1, panel.width - 2, 1);
        (list, inner, Some(panel))
    } else {
        let (list, _) = if bulk > 0 { bulk_split(body, bulk) } else { promote_split(body, selected) };
        (list, body, None)
    }
}

const DETAIL_HINT: &str = " Esc back · ↑↓ PgUp PgDn scroll ";

/// The rounded panel the detail view is drawn in, titled with the feature.
fn detail_block(name: &str, width: u16, position: Option<&str>) -> Block<'static> {
    let room = usize::from(width.saturating_sub(2));
    let clean: String = name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let title = truncate_right(&format!(" Feature · {clean} "), room);
    let position = position.map(|p| format!(" {p} ")).unwrap_or_default();
    let hint = truncate_right(DETAIL_HINT, room.saturating_sub(display_width(&position)));
    let dim = Style::default().fg(Color::DarkGray);
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(dim)
        .title(title)
        .title_bottom(Line::from(hint).style(dim));
    if !position.is_empty() {
        block = block.title_bottom(Line::from(position).style(dim).right_aligned());
    }
    block
}

/// "first-last/total" for the lines visible in a `height`-row viewport of
/// `total` lines scrolled to `scroll`, or `None` when everything fits.
fn scroll_position(scroll: usize, height: usize, total: usize) -> Option<String> {
    if height == 0 || total <= height {
        return None;
    }
    let start = scroll.min(total - height);
    Some(format!("{}-{}/{}", start + 1, start + height, total))
}

/// The scrollable text area inside the detail panel drawn in `panel`.
fn detail_viewport(panel: Rect) -> Rect {
    Block::bordered().inner(panel)
}

fn render_selected_panel(
    frame: &mut Frame,
    panel: Rect,
    inner: Rect,
    selected: Option<&FeatureRow>,
    marked: usize,
    bulk: usize,
) {
    let title = match selected {
        Some(row) => format!(" Selected feature · {} ", row.name),
        None => " Selected feature ".to_string(),
    };
    let title: String = title.chars().take(usize::from(panel.width.saturating_sub(2))).collect();
    let hint = if marked == 0 { PANEL_HINT.to_string() } else { format!(" {marked} marked ·{PANEL_HINT}") };
    let hint: String = hint.chars().take(usize::from(panel.width.saturating_sub(2))).collect();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title)
        .title_bottom(Line::from(hint).style(Style::default().fg(Color::DarkGray)));
    frame.render_widget(block, panel);
    let Some(row) = selected else {
        if bulk > 0 {
            render_bulk_button(frame, inner, bulk);
        } else {
            frame.render_widget(
                Paragraph::new("No feature selected").style(Style::default().fg(Color::DarkGray)),
                inner,
            );
        }
        return;
    };
    if bulk > 0 {
        render_bulk_button(frame, inner, bulk);
    } else if promotable(selected).is_some() {
        render_promote_button(frame, inner, selected);
    } else {
        frame.render_widget(
            Paragraph::new("Mark pending is only for drafts").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
    }
    let status_width = u16::try_from(row.status.chars().count()).unwrap_or(u16::MAX);
    let label_width = if bulk > 0 { bulk_label(bulk).chars().count() } else { PROMOTE_LABEL.chars().count() };
    if usize::from(inner.width) > usize::from(status_width) + label_width {
        let at = Rect::new(inner.right() - status_width, inner.y, status_width, 1);
        frame.render_widget(Paragraph::new(row.status.clone()).style(status_style(&row.status)), at);
    }
}

fn body_area(area: Rect, notice: bool) -> Rect {
    Rect::new(area.x, area.y, area.width, area.height.saturating_sub(u16::from(notice)))
}
