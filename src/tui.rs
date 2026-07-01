//! The interactive TUI frontend: renders a review in a scrollable pane
//! navigated like vim's normal mode.
//!
//! Split in three pure layers and one effectful edge. `render_lines` turns a
//! review into styled lines plus the set of cursor stops. `InputState`
//! decodes keystrokes into `Motion`s — it owns the count prefix (`5j`) and
//! the pending-`g` of `gg`, and is a pure state machine. `App` applies a
//! `Motion`: it moves the cursor and clamps the viewport so the cursor stays
//! on screen. All three are unit-tested with no terminal. `run` is the edge:
//! it owns the terminal and the keyboard loop, the way `RealGit` owns the
//! `git` subprocess. Only the composition root calls it.
//!
//! Same convention as the plain-text frontend (`+`/`-`/`~`), here carried by
//! color: added green, removed red, modified yellow with the old signature
//! dimmed beneath. The cursor's row is shown reversed. No resolution, no
//! navigation across refs — just shape.

use std::io;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::core::{Change, FileChange, FileChangeKind};

/// A review turned into display lines plus the indices of the lines the
/// cursor may land on. Blank separators and `was:` continuation rows are
/// rendered but are not stops.
struct Rendered {
    lines: Vec<Line<'static>>,
    stops: Vec<u16>,
}

/// A decoded normal-mode command. `usize` payloads are repeat counts; the
/// `Option<usize>` on `First`/`Last` is the optional explicit count of
/// `{n}gg` / `{n}G` (a 1-based item index), `None` meaning the bare motion.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Motion {
    Down(usize),
    Up(usize),
    First(Option<usize>),
    Last(Option<usize>),
    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,
    /// `H` / `M` / `L` — move the cursor to the top / middle / bottom item
    /// of the current viewport, leaving the view put.
    CursorHigh,
    CursorMiddle,
    CursorLow,
    /// `zt` / `zz` / `zb` — scroll the view so the cursor's item sits at the
    /// top / center / bottom, leaving the cursor on its item.
    ViewTop,
    ViewCenter,
    ViewBottom,
    NextMatch,
    PrevMatch,
    Quit,
}

/// A spot within the viewport: the top, the vertical middle, or the bottom.
/// Shared by `H`/`M`/`L` (move cursor there) and `zt`/`zz`/`zb` (scroll the
/// cursor there).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ViewSpot {
    Top,
    Middle,
    Bottom,
}

/// Which keymap is live. `Normal` decodes motions; `Search` accumulates a
/// query in the bottom border until `Enter` commits or `Esc` cancels.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Search,
}

/// The pending state between keystrokes: a half-typed count and whether a
/// lone `g` (of `gg`) or `z` (of `zt`/`zz`/`zb`) is waiting for its partner.
/// Pure — `feed` is the whole decoder.
#[derive(Default)]
struct InputState {
    count: Option<usize>,
    pending_g: bool,
    pending_z: bool,
}

impl InputState {
    /// Folds one keypress into the pending state, returning a `Motion` once
    /// one is complete. Returns `None` for the intermediate keys — a count
    /// digit, or the first `g` of `gg`.
    fn feed(&mut self, code: KeyCode, ctrl: bool) -> Option<Motion> {
        // A pending `z` prefix consumes exactly the next key.
        if self.pending_z {
            self.pending_z = false;
            self.count = None;
            return match (ctrl, code) {
                (false, KeyCode::Char('t')) => Some(Motion::ViewTop),
                (false, KeyCode::Char('z')) => Some(Motion::ViewCenter),
                (false, KeyCode::Char('b')) => Some(Motion::ViewBottom),
                _ => None,
            };
        }

        if let KeyCode::Char(c) = code {
            // A leading `0` is not a count (vim reserves it for column 0,
            // which has no meaning here); `0` only extends a count already
            // under way.
            if !ctrl && c.is_ascii_digit() && !(c == '0' && self.count.is_none()) {
                let digit = usize::from(c as u8 - b'0');
                self.count = Some(
                    self.count
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(digit),
                );
                return None;
            }
        }

        if !ctrl && code == KeyCode::Char('z') {
            self.pending_g = false;
            self.pending_z = true;
            return None;
        }

        if !ctrl && code == KeyCode::Char('g') {
            if self.pending_g {
                self.pending_g = false;
                return Some(Motion::First(self.count.take()));
            }
            self.pending_g = true;
            return None;
        }

        // Any other key ends a dangling `g` and consumes the pending count.
        self.pending_g = false;
        let count = self.count.take();
        let times = count.unwrap_or(1);
        let motion = match (ctrl, code) {
            (false, KeyCode::Char('q') | KeyCode::Esc) => Motion::Quit,
            (false, KeyCode::Char('j') | KeyCode::Down) => Motion::Down(times),
            (false, KeyCode::Char('k') | KeyCode::Up) => Motion::Up(times),
            (false, KeyCode::Char('G')) => Motion::Last(count),
            (true, KeyCode::Char('d')) => Motion::HalfPageDown,
            (true, KeyCode::Char('u')) => Motion::HalfPageUp,
            (true, KeyCode::Char('f')) | (false, KeyCode::PageDown) => Motion::PageDown,
            (true, KeyCode::Char('b')) | (false, KeyCode::PageUp) => Motion::PageUp,
            (false, KeyCode::Char('H')) => Motion::CursorHigh,
            (false, KeyCode::Char('M')) => Motion::CursorMiddle,
            (false, KeyCode::Char('L')) => Motion::CursorLow,
            (false, KeyCode::Char('n')) => Motion::NextMatch,
            (false, KeyCode::Char('N')) => Motion::PrevMatch,
            _ => return None,
        };
        Some(motion)
    }
}

/// Scrollable view over a pre-rendered review. `cursor` indexes `stops`;
/// `scroll` is the topmost visible line, derived to keep the cursor on
/// screen. `viewport_height` is the last drawn body height, recorded each
/// frame so the motion handlers can clamp without the terminal.
struct App {
    lines: Vec<Line<'static>>,
    stops: Vec<u16>,
    /// Stop indices whose row matched the last committed search.
    matches: Vec<usize>,
    cursor: usize,
    scroll: u16,
    viewport_height: u16,
}

impl App {
    fn new(review: &[FileChange]) -> Self {
        let Rendered { lines, stops } = render_lines(review);
        Self {
            lines,
            stops,
            matches: Vec::new(),
            cursor: 0,
            scroll: 0,
            viewport_height: 0,
        }
    }

    /// Largest valid top-line index: enough to bring the final line into
    /// view, and no further. Zero when everything already fits.
    fn max_scroll(&self) -> u16 {
        let total = u16::try_from(self.lines.len()).unwrap_or(u16::MAX);
        total.saturating_sub(self.viewport_height)
    }

    /// The display line the cursor currently sits on.
    fn cursor_line(&self) -> u16 {
        self.stops.get(self.cursor).copied().unwrap_or(0)
    }

    fn half_page(&self) -> usize {
        usize::from((self.viewport_height / 2).max(1))
    }

    fn page(&self) -> usize {
        usize::from(self.viewport_height.max(1))
    }

    fn apply(&mut self, motion: Motion) {
        match motion {
            Motion::Down(n) => self.cursor_down(n),
            Motion::Up(n) => self.cursor_up(n),
            Motion::First(n) => self.goto_item(n, false),
            Motion::Last(n) => self.goto_item(n, true),
            Motion::HalfPageDown => self.cursor_down(self.half_page()),
            Motion::HalfPageUp => self.cursor_up(self.half_page()),
            Motion::PageDown => self.cursor_down(self.page()),
            Motion::PageUp => self.cursor_up(self.page()),
            Motion::CursorHigh => self.cursor_to_viewport(ViewSpot::Top),
            Motion::CursorMiddle => self.cursor_to_viewport(ViewSpot::Middle),
            Motion::CursorLow => self.cursor_to_viewport(ViewSpot::Bottom),
            Motion::ViewTop => self.scroll_cursor_to(ViewSpot::Top),
            Motion::ViewCenter => self.scroll_cursor_to(ViewSpot::Middle),
            Motion::ViewBottom => self.scroll_cursor_to(ViewSpot::Bottom),
            Motion::NextMatch => self.next_match(),
            Motion::PrevMatch => self.prev_match(),
            Motion::Quit => {}
        }
    }

    fn cursor_down(&mut self, n: usize) {
        if self.stops.is_empty() {
            return;
        }
        self.cursor = self.cursor.saturating_add(n).min(self.stops.len() - 1);
        self.scroll_to_cursor();
    }

    fn cursor_up(&mut self, n: usize) {
        self.cursor = self.cursor.saturating_sub(n);
        self.scroll_to_cursor();
    }

    /// `gg` / `G`, with or without an explicit 1-based item count. With no
    /// count, `last_if_none` picks the end (`G`) over the start (`gg`).
    fn goto_item(&mut self, n: Option<usize>, last_if_none: bool) {
        if self.stops.is_empty() {
            return;
        }
        let last = self.stops.len() - 1;
        self.cursor = match n {
            Some(k) => k.saturating_sub(1).min(last),
            None if last_if_none => last,
            None => 0,
        };
        self.scroll_to_cursor();
    }

    /// Slides the viewport the minimum needed to keep the cursor visible,
    /// then clamps so it never scrolls past the last line.
    fn scroll_to_cursor(&mut self) {
        let line = self.cursor_line();
        if line < self.scroll {
            self.scroll = line;
        } else if self.viewport_height > 0 && line >= self.scroll + self.viewport_height {
            self.scroll = line.saturating_sub(self.viewport_height).saturating_add(1);
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// The first and last stop indices currently visible in the viewport.
    /// `None` only when there are no stops at all (the cursor's own stop is
    /// always visible otherwise).
    fn visible_stops(&self) -> Option<(usize, usize)> {
        let bottom = self.scroll.saturating_add(self.viewport_height);
        let mut visible = self
            .stops
            .iter()
            .enumerate()
            .filter(|&(_, &line)| line >= self.scroll && line < bottom)
            .map(|(i, _)| i);
        let first = visible.next()?;
        Some((first, visible.next_back().unwrap_or(first)))
    }

    /// `H` / `M` / `L`: move the cursor to the top, middle, or bottom item of
    /// the current viewport without moving the view.
    fn cursor_to_viewport(&mut self, spot: ViewSpot) {
        if let Some((first, last)) = self.visible_stops() {
            self.cursor = match spot {
                ViewSpot::Top => first,
                ViewSpot::Middle => first + (last - first) / 2,
                ViewSpot::Bottom => last,
            };
            self.scroll_to_cursor();
        }
    }

    /// Recomputes the set of matching stops for `query` (case-insensitive
    /// substring over the rendered row text) and jumps the cursor to the
    /// first match at or after its current position, wrapping if needed. An
    /// empty query clears any existing matches.
    fn search(&mut self, query: &str) {
        self.matches.clear();
        if query.is_empty() {
            return;
        }
        let needle = query.to_lowercase();
        for (i, &line) in self.stops.iter().enumerate() {
            if line_text(&self.lines[usize::from(line)])
                .to_lowercase()
                .contains(&needle)
            {
                self.matches.push(i);
            }
        }
        if let Some(&target) = self
            .matches
            .iter()
            .find(|&&m| m >= self.cursor)
            .or_else(|| self.matches.first())
        {
            self.cursor = target;
            self.scroll_to_cursor();
        }
    }

    /// `zt` / `zz` / `zb`: scroll so the cursor's item sits at the top,
    /// center, or bottom of the viewport, without moving the cursor. Clamped
    /// so the view never runs past either end.
    fn scroll_cursor_to(&mut self, spot: ViewSpot) {
        let line = self.cursor_line();
        let offset = match spot {
            ViewSpot::Top => 0,
            ViewSpot::Middle => self.viewport_height / 2,
            ViewSpot::Bottom => self.viewport_height.saturating_sub(1),
        };
        self.scroll = line.saturating_sub(offset).min(self.max_scroll());
    }

    fn next_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.cursor = self
            .matches
            .iter()
            .find(|&&m| m > self.cursor)
            .or_else(|| self.matches.first())
            .copied()
            .unwrap_or(self.cursor);
        self.scroll_to_cursor();
    }

    fn prev_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.cursor = self
            .matches
            .iter()
            .rev()
            .find(|&&m| m < self.cursor)
            .or_else(|| self.matches.last())
            .copied()
            .unwrap_or(self.cursor);
        self.scroll_to_cursor();
    }
}

/// Concatenates a line's spans back into plain text, for substring search.
fn line_text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Turns a review into the styled lines the pane scrolls over and the set of
/// cursor stops. Files are separated by a blank line, mirroring the
/// plain-text frontend. Stops are the file headers and the change rows; the
/// blank separators and the `was:` continuation lines are skipped.
fn render_lines(review: &[FileChange]) -> Rendered {
    let mut lines = Vec::new();
    let mut stops = Vec::new();
    if review.is_empty() {
        lines.push(Line::from("No structural changes.").dim());
        return Rendered { lines, stops };
    }
    let stop = |lines: &[Line<'static>], stops: &mut Vec<u16>| {
        stops.push(u16::try_from(lines.len()).unwrap_or(u16::MAX));
    };
    for (i, file) in review.iter().enumerate() {
        if i > 0 {
            lines.push(Line::default());
        }
        match &file.kind {
            FileChangeKind::Deleted => {
                stop(&lines, &mut stops);
                lines.push(
                    Line::from(format!("DELETED {}", file.path.display()))
                        .red()
                        .bold(),
                );
            }
            FileChangeKind::Changed(changeset) => {
                stop(&lines, &mut stops);
                lines.push(Line::from(file.path.display().to_string()).bold());
                for change in &changeset.changes {
                    stop(&lines, &mut stops);
                    match change {
                        Change::Added(item) => {
                            lines.push(Line::from(format!("  + {}", item.signature)).green());
                        }
                        Change::Removed(item) => {
                            lines.push(Line::from(format!("  - {}", item.signature)).red());
                        }
                        Change::Modified { before, after } => {
                            lines.push(Line::from(format!("  ~ {}", after.signature)).yellow());
                            lines
                                .push(Line::from(format!("      was: {}", before.signature)).dim());
                        }
                    }
                }
            }
        }
    }
    Rendered { lines, stops }
}

/// Runs the interactive view until the user quits. The effectful edge:
/// sets up the terminal (raw mode + alternate screen via `ratatui::init`),
/// pumps key events, and restores the terminal on the way out.
pub(crate) fn run(review: &[FileChange]) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut App::new(review));
    ratatui::restore();
    result
}

const HELP: &str = " j/k move · ^d/^u/^f/^b scroll · gg/G ends · H/M/L view · zt/zz/zb center · / search · n/N matches · q quit ";

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let mut input = InputState::default();
    let mut mode = Mode::Normal;
    let mut query = String::new();
    loop {
        let footer = if mode == Mode::Search {
            format!(" /{query} ")
        } else {
            HELP.to_owned()
        };
        terminal.draw(|frame| draw(frame, app, &footer))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match mode {
            Mode::Search => match key.code {
                KeyCode::Esc => {
                    mode = Mode::Normal;
                    query.clear();
                }
                KeyCode::Enter => {
                    app.search(&query);
                    mode = Mode::Normal;
                }
                KeyCode::Backspace => {
                    query.pop();
                }
                KeyCode::Char(c) => query.push(c),
                _ => {}
            },
            Mode::Normal => {
                if !ctrl && key.code == KeyCode::Char('/') {
                    mode = Mode::Search;
                    query.clear();
                } else if let Some(motion) = input.feed(key.code, ctrl) {
                    if motion == Motion::Quit {
                        return Ok(());
                    }
                    app.apply(motion);
                }
            }
        }
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &mut App, footer: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" absolem — shape of the change ")
        .title_bottom(footer);
    // Record the inner height so scroll clamping matches what's on screen,
    // and re-follow the cursor in case a resize moved it out of view.
    app.viewport_height = block.inner(frame.area()).height;
    app.scroll_to_cursor();

    let mut lines = app.lines.clone();
    // Search matches are underlined; the cursor's row reverses on top.
    for &m in &app.matches {
        if let Some(&line) = app.stops.get(m)
            && let Some(rendered) = lines.get_mut(usize::from(line))
        {
            rendered.style = rendered.style.add_modifier(Modifier::UNDERLINED);
        }
    }
    if !app.stops.is_empty()
        && let Some(line) = lines.get_mut(usize::from(app.cursor_line()))
    {
        line.style = line.style.add_modifier(Modifier::REVERSED);
    }
    let paragraph = Paragraph::new(lines).block(block).scroll((app.scroll, 0));
    frame.render_widget(paragraph, frame.area());
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use ratatui::style::Color;

    use super::*;
    use crate::core::ChangeSet;
    use crate::item::{Item, ItemId, Kind};

    fn item(name: &str, sig: &str) -> Item {
        Item {
            id: ItemId {
                path: PathBuf::from("f.go"),
                kind: Kind::Function,
                name: name.into(),
            },
            signature: sig.into(),
            // Qualified: `Line` unqualified is ratatui's in this module.
            line: crate::item::Line(1),
        }
    }

    fn changed(path: &str, changes: Vec<Change>) -> FileChange {
        FileChange {
            path: PathBuf::from(path),
            kind: FileChangeKind::Changed(ChangeSet { changes }),
        }
    }

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Three added items in one file: 1 header + 3 change rows = 4 stops over
    /// 4 lines. Used by the cursor/scroll tests.
    fn three_items() -> App {
        App::new(&[changed(
            "a.go",
            vec![
                Change::Added(item("A", "func A()")),
                Change::Added(item("B", "func B()")),
                Change::Added(item("C", "func C()")),
            ],
        )])
    }

    /// One file with `n` added items: 1 header + n change rows = n+1 stops
    /// over n+1 lines. Used by the viewport-positioning tests.
    fn many_items(n: usize) -> App {
        let changes = (0..n)
            .map(|i| Change::Added(item(&format!("F{i}"), &format!("func F{i}()"))))
            .collect();
        App::new(&[changed("a.go", changes)])
    }

    #[test]
    fn empty_review_shows_placeholder() {
        let rendered = render_lines(&[]);
        assert_eq!(rendered.lines.len(), 1);
        assert_eq!(text(&rendered.lines[0]), "No structural changes.");
        assert!(rendered.stops.is_empty());
    }

    #[test]
    fn changed_file_renders_header_then_prefixed_changes() {
        let rendered = render_lines(&[changed(
            "a.go",
            vec![
                Change::Added(item("A", "func A()")),
                Change::Modified {
                    before: item("B", "func B()"),
                    after: item("B", "func B(x int)"),
                },
            ],
        )]);
        let lines: Vec<String> = rendered.lines.iter().map(text).collect();
        assert_eq!(
            lines,
            vec![
                "a.go",
                "  + func A()",
                "  ~ func B(x int)",
                "      was: func B()",
            ]
        );
    }

    #[test]
    fn stops_are_headers_and_change_rows_only() {
        let rendered = render_lines(&[changed(
            "a.go",
            vec![Change::Modified {
                before: item("B", "func B()"),
                after: item("B", "func B(x int)"),
            }],
        )]);
        // Lines: 0 header, 1 "~", 2 "was:". The continuation row is not a stop.
        assert_eq!(rendered.stops, vec![0, 1]);
    }

    #[test]
    fn change_lines_are_colored_by_kind() {
        let rendered = render_lines(&[changed(
            "a.go",
            vec![
                Change::Added(item("A", "func A()")),
                Change::Removed(item("C", "func C()")),
            ],
        )]);
        assert_eq!(rendered.lines[1].style.fg, Some(Color::Green));
        assert_eq!(rendered.lines[2].style.fg, Some(Color::Red));
    }

    #[test]
    fn files_separated_by_blank_line() {
        let rendered = render_lines(&[
            changed("a.go", vec![Change::Added(item("A", "func A()"))]),
            FileChange {
                path: PathBuf::from("b.go"),
                kind: FileChangeKind::Deleted,
            },
        ]);
        let lines: Vec<String> = rendered.lines.iter().map(text).collect();
        assert_eq!(lines, vec!["a.go", "  + func A()", "", "DELETED b.go"]);
        // The blank separator at index 2 is skipped; both file rows are stops.
        assert_eq!(rendered.stops, vec![0, 1, 3]);
    }

    #[test]
    fn cursor_moves_and_scroll_follows() {
        let mut app = three_items();
        app.viewport_height = 2; // 4 lines, 2 visible → max scroll 2.

        app.cursor_down(1);
        assert_eq!((app.cursor, app.scroll), (1, 0));
        app.cursor_down(1);
        assert_eq!((app.cursor, app.scroll), (2, 1)); // line 2 pulled into view
        app.cursor_down(1);
        assert_eq!((app.cursor, app.scroll), (3, 2));
        app.cursor_down(10); // clamps at the last stop
        assert_eq!((app.cursor, app.scroll), (3, 2));

        app.cursor_up(1);
        assert_eq!((app.cursor, app.scroll), (2, 2)); // still visible, no scroll
        app.goto_item(None, false); // gg
        assert_eq!((app.cursor, app.scroll), (0, 0));
    }

    #[test]
    fn count_motion_moves_many_at_once() {
        let mut app = three_items();
        app.viewport_height = 4;
        app.cursor_down(2);
        assert_eq!(app.cursor, 2);
        app.cursor_up(5); // saturates at the top
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn goto_item_honors_explicit_count() {
        let mut app = three_items();
        app.viewport_height = 4;
        app.goto_item(Some(3), false); // 3G → third stop (1-based)
        assert_eq!(app.cursor, 2);
        app.goto_item(Some(99), true); // clamps to last
        assert_eq!(app.cursor, 3);
        app.goto_item(None, true); // G → last
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn cursor_to_viewport_targets_visible_extremes() {
        let mut app = many_items(6); // 7 lines (0..=6)
        app.viewport_height = 4;
        app.scroll = 2; // visible lines 2..=5 → stops 2,3,4,5
        app.cursor_to_viewport(ViewSpot::Top);
        assert_eq!(app.cursor, 2);
        app.cursor_to_viewport(ViewSpot::Bottom);
        assert_eq!(app.cursor, 5);
        app.cursor_to_viewport(ViewSpot::Middle);
        assert_eq!(app.cursor, 3);
        assert_eq!(app.scroll, 2); // the view never moved
    }

    #[test]
    fn scroll_cursor_to_repositions_view_not_cursor() {
        let mut app = many_items(6); // 7 lines → max scroll 3 at height 4
        app.viewport_height = 4;
        app.cursor = 3; // line 3
        app.scroll_cursor_to(ViewSpot::Top);
        assert_eq!((app.cursor, app.scroll), (3, 3));
        app.scroll_cursor_to(ViewSpot::Middle);
        assert_eq!((app.cursor, app.scroll), (3, 1));
        app.scroll_cursor_to(ViewSpot::Bottom);
        assert_eq!((app.cursor, app.scroll), (3, 0));
    }

    #[test]
    fn motions_are_noops_on_empty_review() {
        let mut app = App::new(&[]);
        app.viewport_height = 10;
        app.cursor_down(1);
        app.goto_item(Some(2), true);
        app.cursor_to_viewport(ViewSpot::Top);
        app.scroll_cursor_to(ViewSpot::Middle);
        assert_eq!((app.cursor, app.scroll), (0, 0));
    }

    #[test]
    fn input_accumulates_count_then_emits_motion() {
        let mut input = InputState::default();
        assert_eq!(input.feed(KeyCode::Char('1'), false), None);
        assert_eq!(input.feed(KeyCode::Char('2'), false), None);
        assert_eq!(
            input.feed(KeyCode::Char('j'), false),
            Some(Motion::Down(12))
        );
        // Count is consumed; the next bare motion is a single step.
        assert_eq!(input.feed(KeyCode::Char('j'), false), Some(Motion::Down(1)));
    }

    #[test]
    fn input_zero_is_a_count_digit_only_mid_count() {
        let mut input = InputState::default();
        // Leading 0 is not a count and maps to nothing here.
        assert_eq!(input.feed(KeyCode::Char('0'), false), None);
        assert_eq!(input.feed(KeyCode::Char('j'), false), Some(Motion::Down(1)));
        // But 0 extends a count under way: 1,0 → 10.
        assert_eq!(input.feed(KeyCode::Char('1'), false), None);
        assert_eq!(input.feed(KeyCode::Char('0'), false), None);
        assert_eq!(input.feed(KeyCode::Char('k'), false), Some(Motion::Up(10)));
    }

    #[test]
    fn input_decodes_gg_and_counted_gg() {
        let mut input = InputState::default();
        assert_eq!(input.feed(KeyCode::Char('g'), false), None);
        assert_eq!(
            input.feed(KeyCode::Char('g'), false),
            Some(Motion::First(None))
        );
        assert_eq!(input.feed(KeyCode::Char('5'), false), None);
        assert_eq!(input.feed(KeyCode::Char('g'), false), None);
        assert_eq!(
            input.feed(KeyCode::Char('g'), false),
            Some(Motion::First(Some(5)))
        );
    }

    #[test]
    fn input_drops_dangling_g_before_other_key() {
        let mut input = InputState::default();
        assert_eq!(input.feed(KeyCode::Char('g'), false), None);
        assert_eq!(input.feed(KeyCode::Char('j'), false), Some(Motion::Down(1)));
    }

    #[test]
    fn input_decodes_ctrl_paging_and_g_uppercase() {
        let mut input = InputState::default();
        assert_eq!(
            input.feed(KeyCode::Char('d'), true),
            Some(Motion::HalfPageDown)
        );
        assert_eq!(
            input.feed(KeyCode::Char('u'), true),
            Some(Motion::HalfPageUp)
        );
        assert_eq!(input.feed(KeyCode::Char('f'), true), Some(Motion::PageDown));
        assert_eq!(input.feed(KeyCode::Char('b'), true), Some(Motion::PageUp));
        assert_eq!(
            input.feed(KeyCode::Char('G'), false),
            Some(Motion::Last(None))
        );
        assert_eq!(input.feed(KeyCode::Char('5'), false), None);
        assert_eq!(
            input.feed(KeyCode::Char('G'), false),
            Some(Motion::Last(Some(5)))
        );
    }

    #[test]
    fn input_decodes_quit() {
        let mut input = InputState::default();
        assert_eq!(input.feed(KeyCode::Char('q'), false), Some(Motion::Quit));
        assert_eq!(input.feed(KeyCode::Esc, false), Some(Motion::Quit));
    }

    #[test]
    fn input_decodes_hml_view_positioning() {
        let mut input = InputState::default();
        assert_eq!(
            input.feed(KeyCode::Char('H'), false),
            Some(Motion::CursorHigh)
        );
        assert_eq!(
            input.feed(KeyCode::Char('M'), false),
            Some(Motion::CursorMiddle)
        );
        assert_eq!(
            input.feed(KeyCode::Char('L'), false),
            Some(Motion::CursorLow)
        );
    }

    #[test]
    fn input_decodes_z_prefixed_view_scrolls() {
        let mut input = InputState::default();
        assert_eq!(input.feed(KeyCode::Char('z'), false), None);
        assert_eq!(input.feed(KeyCode::Char('t'), false), Some(Motion::ViewTop));
        assert_eq!(input.feed(KeyCode::Char('z'), false), None);
        assert_eq!(
            input.feed(KeyCode::Char('z'), false),
            Some(Motion::ViewCenter)
        );
        assert_eq!(input.feed(KeyCode::Char('z'), false), None);
        assert_eq!(
            input.feed(KeyCode::Char('b'), false),
            Some(Motion::ViewBottom)
        );
    }

    #[test]
    fn input_drops_unknown_z_command() {
        let mut input = InputState::default();
        assert_eq!(input.feed(KeyCode::Char('z'), false), None);
        assert_eq!(input.feed(KeyCode::Char('x'), false), None); // zx → nothing
        // State cleared: a following motion decodes normally.
        assert_eq!(input.feed(KeyCode::Char('j'), false), Some(Motion::Down(1)));
    }

    #[test]
    fn input_decodes_match_navigation() {
        let mut input = InputState::default();
        assert_eq!(
            input.feed(KeyCode::Char('n'), false),
            Some(Motion::NextMatch)
        );
        assert_eq!(
            input.feed(KeyCode::Char('N'), false),
            Some(Motion::PrevMatch)
        );
    }

    /// Three files, one item each, so every stop carries a distinct name.
    fn three_files() -> App {
        App::new(&[
            changed("a.go", vec![Change::Added(item("Alpha", "func Alpha()"))]),
            changed("b.go", vec![Change::Added(item("Beta", "func Beta()"))]),
            changed("c.go", vec![Change::Added(item("Gamma", "func Gamma()"))]),
        ])
    }

    #[test]
    fn search_collects_matches_and_jumps_to_first() {
        let mut app = three_files();
        app.viewport_height = 20;
        // "func" matches every change row (stops 1, 3, 5); headers don't.
        app.search("func");
        assert_eq!(app.matches, vec![1, 3, 5]);
        assert_eq!(app.cursor, 1); // first match at or after cursor 0
    }

    #[test]
    fn search_is_case_insensitive_and_jumps_forward_from_cursor() {
        let mut app = three_files();
        app.viewport_height = 20;
        app.cursor = 4; // sitting on c.go's header
        app.search("BETA");
        assert_eq!(app.matches, vec![3]);
        // No match at/after 4, so it wraps to the first (and only) match.
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn next_and_prev_match_wrap_around() {
        let mut app = three_files();
        app.viewport_height = 20;
        app.search("func"); // matches [1, 3, 5], cursor → 1
        app.next_match();
        assert_eq!(app.cursor, 3);
        app.next_match();
        assert_eq!(app.cursor, 5);
        app.next_match(); // wraps to the first
        assert_eq!(app.cursor, 1);
        app.prev_match(); // wraps back to the last
        assert_eq!(app.cursor, 5);
    }

    #[test]
    fn search_with_no_hits_clears_matches_and_holds_cursor() {
        let mut app = three_files();
        app.viewport_height = 20;
        app.cursor = 2;
        app.search("func"); // matches [1,3,5]; first at/after 2 is stop 3
        assert_eq!(app.cursor, 3);
        app.search("nonexistent");
        assert!(app.matches.is_empty());
        // No matches → cursor stays where the last search left it.
        assert_eq!(app.cursor, 3);
        app.next_match(); // no-op with no matches
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn empty_query_clears_matches() {
        let mut app = three_files();
        app.viewport_height = 20;
        app.search("func");
        app.search("");
        assert!(app.matches.is_empty());
    }
}
