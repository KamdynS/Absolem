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
//! dimmed beneath. The cursor's row is shown reversed. All colors are
//! ANSI palette entries, never RGB, so the view absorbs the terminal
//! emulator's theme instead of fighting it. No resolution and no
//! navigation across refs — just shape, plus a jump out to `$EDITOR`.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::core::{FileChange, FileChangeKind, ItemStatus, ItemView, Resolved, TypeIndex};
use crate::item::{ItemId, Line as SourceLine};

/// The authority to open the user's editor at a location. The TUI can
/// spawn a process no other way: only the composition root constructs
/// the real, `$EDITOR`-backed implementation, and tests substitute an
/// in-memory fake.
pub(crate) trait EditorLauncher {
    fn open(&self, path: &Path, line: SourceLine) -> io::Result<()>;
}

/// Where a cursor stop leads when opened: the file and line of the item
/// at the ref under review. Removed items have nowhere to go — they no
/// longer exist on the head side.
#[derive(Debug, Clone, PartialEq, Eq)]
struct JumpTarget {
    path: PathBuf,
    line: SourceLine,
}

/// The stable identity of an item row: the chain of `ItemId`s from the
/// review row down through each expansion that produced it. Expansion
/// state is keyed by this rather than by stop position — positions
/// shift whenever rows unfold, identities don't. Doubles as the cycle
/// guard: a definition already on the path is not expanded again.
#[derive(Debug, Clone, Default, Hash, PartialEq, Eq)]
struct RowPath(Vec<ItemId>);

impl RowPath {
    fn child(&self, id: &ItemId) -> Self {
        let mut ids = self.0.clone();
        ids.push(id.clone());
        Self(ids)
    }

    fn contains(&self, id: &ItemId) -> bool {
        self.0.contains(id)
    }
}

/// A review turned into display lines plus the indices of the lines the
/// cursor may land on. Blank separators and `was:` continuation rows are
/// rendered but are not stops.
struct Rendered {
    lines: Vec<Line<'static>>,
    stops: Vec<u16>,
    /// Parallel to `stops`: where each stop jumps when opened.
    jumps: Vec<Option<JumpTarget>>,
    /// Parallel to `stops`: whether the row references a type the index
    /// can resolve, i.e. whether `Tab` will do anything.
    expandable: Vec<bool>,
    /// Parallel to `stops`: whether anything references the row's item,
    /// i.e. whether `gr` will do anything.
    referenced: Vec<bool>,
    /// Parallel to `stops`: the row's identity, `None` for file headers.
    paths: Vec<Option<RowPath>>,
    /// Indices into `stops` that are file rows — the `{` / `}` waypoints.
    headers: Vec<usize>,
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
    /// `{` / `}` — move the cursor to the previous / next file header.
    PrevFile(usize),
    NextFile(usize),
    /// `Tab` — expand or collapse the types the cursor's item references.
    Expand,
    /// `gr` — show or hide what references the cursor's item.
    Usages,
    /// `Enter` — open the cursor's item in the user's editor.
    Open,
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

        // `gr`: the one non-`gg` completion of a pending `g`.
        if self.pending_g && !ctrl && code == KeyCode::Char('r') {
            self.pending_g = false;
            self.count = None;
            return Some(Motion::Usages);
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
            (false, KeyCode::Char('{')) => Motion::PrevFile(times),
            (false, KeyCode::Char('}')) => Motion::NextFile(times),
            (false, KeyCode::Tab) => Motion::Expand,
            (false, KeyCode::Enter) => Motion::Open,
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
    /// The review being displayed — kept so the pane can re-render when
    /// an expansion toggles.
    review: Vec<FileChange>,
    /// The head-wide index expansions resolve against.
    index: TypeIndex,
    /// Rows whose expansion is currently open, by identity — positions
    /// shift as rows unfold, identities don't.
    expanded: HashSet<RowPath>,
    /// Rows whose used-by list is currently open, same keying.
    usages_open: HashSet<RowPath>,
    lines: Vec<Line<'static>>,
    stops: Vec<u16>,
    /// Parallel to `stops`: where each stop jumps when opened.
    jumps: Vec<Option<JumpTarget>>,
    /// Parallel to `stops`: whether the row has at least one reference
    /// that resolves in the index.
    expandable: Vec<bool>,
    /// Parallel to `stops`: whether anything references the row's item.
    referenced: Vec<bool>,
    /// Parallel to `stops`: the row's identity, `None` for file headers.
    paths: Vec<Option<RowPath>>,
    /// Indices into `stops` that are file rows — the `{` / `}` waypoints.
    headers: Vec<usize>,
    /// Stop indices whose row matched the last committed search.
    matches: Vec<usize>,
    /// The title-bar summary: file and change counts.
    summary: String,
    cursor: usize,
    scroll: u16,
    viewport_height: u16,
}

impl App {
    fn new(review: &[FileChange], index: TypeIndex) -> Self {
        let mut app = Self {
            review: review.to_vec(),
            index,
            expanded: HashSet::new(),
            usages_open: HashSet::new(),
            lines: Vec::new(),
            stops: Vec::new(),
            jumps: Vec::new(),
            expandable: Vec::new(),
            referenced: Vec::new(),
            paths: Vec::new(),
            headers: Vec::new(),
            matches: Vec::new(),
            summary: summarize(review),
            cursor: 0,
            scroll: 0,
            viewport_height: 0,
        };
        app.rerender();
        app
    }

    /// Rebuilds the display lines from the review and the current
    /// expansion state, then puts the cursor back on the row it was on:
    /// rows are found again by identity, since unfolding shifts every
    /// position below it.
    fn rerender(&mut self) {
        let on = self.paths.get(self.cursor).cloned().flatten();
        let Rendered {
            lines,
            stops,
            jumps,
            expandable,
            referenced,
            paths,
            headers,
        } = render_lines(
            &self.review,
            &RenderCtx {
                expanded: &self.expanded,
                usages_open: &self.usages_open,
                index: &self.index,
            },
        );
        self.lines = lines;
        self.stops = stops;
        self.jumps = jumps;
        self.expandable = expandable;
        self.referenced = referenced;
        self.paths = paths;
        self.headers = headers;
        if let Some(on) = on
            && let Some(found) = self.paths.iter().position(|p| p.as_ref() == Some(&on))
        {
            self.cursor = found;
        } else if !self.stops.is_empty() {
            self.cursor = self.cursor.min(self.stops.len() - 1);
        }
        self.scroll_to_cursor();
    }

    /// `Tab`: toggles the cursor row's expansion. Returns a footer
    /// notice when there is nothing to expand.
    fn toggle_expand(&mut self) -> Option<String> {
        if !self.expandable.get(self.cursor).copied().unwrap_or(false) {
            return Some(" nothing to expand here ".to_owned());
        }
        let Some(path) = self.paths.get(self.cursor).cloned().flatten() else {
            return Some(" nothing to expand here ".to_owned());
        };
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        self.rerender();
        None
    }

    /// `gr`: toggles the cursor row's used-by list. Returns a footer
    /// notice when nothing references the item.
    fn toggle_usages(&mut self) -> Option<String> {
        if !self.referenced.get(self.cursor).copied().unwrap_or(false) {
            return Some(" nothing references this item ".to_owned());
        }
        let Some(path) = self.paths.get(self.cursor).cloned().flatten() else {
            return Some(" nothing references this item ".to_owned());
        };
        if !self.usages_open.remove(&path) {
            self.usages_open.insert(path);
        }
        self.rerender();
        None
    }

    /// Where the cursor's stop leads, if anywhere.
    fn current_jump(&self) -> Option<&JumpTarget> {
        self.jumps.get(self.cursor).and_then(Option::as_ref)
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
            Motion::PrevFile(n) => self.prev_file(n),
            Motion::NextFile(n) => self.next_file(n),
            // Unfolds, Open, and Quit are handled by the event loop:
            // they either re-render or act outside the pane.
            Motion::Expand | Motion::Usages | Motion::Open | Motion::Quit => {}
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

    /// `{`: the `n`-th file header strictly before the cursor, saturating
    /// at the first.
    fn prev_file(&mut self, n: usize) {
        let before = self.headers.iter().filter(|&&h| h < self.cursor).count();
        let Some(&target) = before
            .checked_sub(n)
            .or(if before > 0 { Some(0) } else { None })
            .and_then(|i| self.headers.get(i))
        else {
            return;
        };
        self.cursor = target;
        self.scroll_to_cursor();
    }

    /// `}`: the `n`-th file header strictly after the cursor, saturating
    /// at the last.
    fn next_file(&mut self, n: usize) {
        let mut after = self.headers.iter().filter(|&&h| h > self.cursor);
        let Some(&target) = after
            .nth(n.saturating_sub(1))
            .or_else(|| self.headers.last())
        else {
            return;
        };
        if target > self.cursor {
            self.cursor = target;
            self.scroll_to_cursor();
        }
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

/// The two sides of a modified signature as spans, with the tokens that
/// differ marked: struck out on the old side, bold and underlined on
/// the new. A hand-rolled token LCS — signatures are one line, so the
/// quadratic table is nothing.
fn modified_spans(before: &str, after: &str) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let before_tokens = tokenize(before);
    let after_tokens = tokenize(after);
    let (before_common, after_common) = lcs_membership(&before_tokens, &after_tokens);
    (
        marked_spans(&before_tokens, &before_common, Modifier::CROSSED_OUT),
        marked_spans(
            &after_tokens,
            &after_common,
            Modifier::BOLD | Modifier::UNDERLINED,
        ),
    )
}

/// Splits into word runs (`[A-Za-z0-9_]+`) and single non-word
/// characters, so a renamed parameter or a changed type marks as one
/// token, not per character.
fn tokenize(s: &str) -> Vec<&str> {
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if is_word(bytes[i]) {
            let start = i;
            while i < bytes.len() && is_word(bytes[i]) {
                i += 1;
            }
            out.push(&s[start..i]);
        } else {
            let len = s[i..].chars().next().map_or(1, char::len_utf8);
            out.push(&s[i..i + len]);
            i += len;
        }
    }
    out
}

/// For each side, whether the token is part of the longest common
/// subsequence — `false` means the token changed.
fn lcs_membership(left: &[&str], right: &[&str]) -> (Vec<bool>, Vec<bool>) {
    let (rows, cols) = (left.len(), right.len());
    // table[row][col] = LCS length of left[row..] and right[col..].
    let mut table = vec![vec![0u16; cols + 1]; rows + 1];
    for row in (0..rows).rev() {
        for col in (0..cols).rev() {
            table[row][col] = if left[row] == right[col] {
                table[row + 1][col + 1] + 1
            } else {
                table[row + 1][col].max(table[row][col + 1])
            };
        }
    }
    let (mut in_left, mut in_right) = (vec![false; rows], vec![false; cols]);
    let (mut row, mut col) = (0, 0);
    while row < rows && col < cols {
        if left[row] == right[col] {
            in_left[row] = true;
            in_right[col] = true;
            row += 1;
            col += 1;
        } else if table[row + 1][col] >= table[row][col + 1] {
            row += 1;
        } else {
            col += 1;
        }
    }
    (in_left, in_right)
}

/// Tokens back into spans, adjacent same-state tokens merged; tokens
/// outside the common subsequence carry `marker`.
fn marked_spans(tokens: &[&str], common: &[bool], marker: Modifier) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_common = true;
    for (token, &is_common) in tokens.iter().zip(common) {
        if is_common != run_common && !run.is_empty() {
            out.push(span_for(std::mem::take(&mut run), run_common, marker));
        }
        run_common = is_common;
        run.push_str(token);
    }
    if !run.is_empty() {
        out.push(span_for(run, run_common, marker));
    }
    out
}

fn span_for(text: String, common: bool, marker: Modifier) -> Span<'static> {
    if common {
        Span::raw(text)
    } else {
        Span::styled(text, Style::default().add_modifier(marker))
    }
}

/// Accumulates the rendered lines and, in parallel, the cursor stops
/// with their jump targets. Blank separators and `was:` continuation
/// rows are lines but not stops.
#[derive(Default)]
struct LineBuilder {
    lines: Vec<Line<'static>>,
    stops: Vec<u16>,
    jumps: Vec<Option<JumpTarget>>,
    expandable: Vec<bool>,
    referenced: Vec<bool>,
    paths: Vec<Option<RowPath>>,
    headers: Vec<usize>,
}

/// Where an item's row jumps: its declaration site, straight off the
/// item itself.
fn jump(view: &ItemView) -> JumpTarget {
    JumpTarget {
        path: view.item.id.path.clone(),
        line: view.item.line,
    }
}

/// Whether `Tab` on this row would unfold anything: at least one of its
/// references resolves to a definition not already on the row's path
/// (expanding a cycle would loop forever).
fn can_expand(view: &ItemView, path: &RowPath, index: &TypeIndex) -> bool {
    view.item.refs.iter().any(|r| {
        index
            .lookup(&r.0, &view.item.id.path)
            .is_some_and(|res| !path.contains(&res.def.item.id))
    })
}

/// Whether `gr` on this row would unfold anything: something other than
/// the item itself (or a row already on the path) references its name.
fn has_users(view: &ItemView, path: &RowPath, index: &TypeIndex) -> bool {
    index
        .users_of(&view.item.id.name)
        .iter()
        .any(|u| u.item.id != view.item.id && !path.contains(&u.item.id))
}

/// What the row builders consult: which rows are unfolded in each
/// direction, and the index that resolves both.
struct RenderCtx<'a> {
    expanded: &'a HashSet<RowPath>,
    usages_open: &'a HashSet<RowPath>,
    index: &'a TypeIndex,
}

impl LineBuilder {
    fn into_rendered(self) -> Rendered {
        Rendered {
            lines: self.lines,
            stops: self.stops,
            jumps: self.jumps,
            expandable: self.expandable,
            referenced: self.referenced,
            paths: self.paths,
            headers: self.headers,
        }
    }

    /// Pushes a line the cursor can land on.
    fn stop(
        &mut self,
        line: Line<'static>,
        jump: Option<JumpTarget>,
        expandable: bool,
        referenced: bool,
        path: Option<RowPath>,
    ) {
        self.stops
            .push(u16::try_from(self.lines.len()).unwrap_or(u16::MAX));
        self.jumps.push(jump);
        self.expandable.push(expandable);
        self.referenced.push(referenced);
        self.paths.push(path);
        self.lines.push(line);
    }

    /// Whatever the row's toggles have unfolded, rendered beneath it:
    /// referenced definitions (`Tab`) and referencing items (`gr`).
    fn unfolded(&mut self, view: &ItemView, indent: usize, row_path: &RowPath, ctx: &RenderCtx) {
        if ctx.expanded.contains(row_path) {
            self.expansion(view, indent, row_path, ctx);
        }
        if ctx.usages_open.contains(row_path) {
            self.usage_rows(view, indent, row_path, ctx);
        }
    }

    /// One review row at `indent`: status marker and color by status,
    /// unchanged context dimmed, a modified row's old signature beneath,
    /// and whatever the row has unfolded below it.
    fn item_row(&mut self, view: &ItemView, indent: usize, parent: &RowPath, ctx: &RenderCtx) {
        let pad = " ".repeat(indent);
        let sig = &view.item.signature;
        let row_path = parent.child(&view.item.id);
        let expandable = can_expand(view, &row_path, ctx.index);
        let referenced = has_users(view, &row_path, ctx.index);
        match &view.status {
            ItemStatus::Added => {
                self.stop(
                    Line::from(format!("{pad}+ {sig}")).green(),
                    Some(jump(view)),
                    expandable,
                    referenced,
                    Some(row_path.clone()),
                );
            }
            ItemStatus::Removed => {
                // A removed item no longer exists on the head side:
                // nowhere to jump.
                self.stop(
                    Line::from(format!("{pad}- {sig}")).red(),
                    None,
                    expandable,
                    referenced,
                    Some(row_path.clone()),
                );
            }
            ItemStatus::Modified { before } => {
                // Word-level diff: what changed within the signature is
                // marked, so the eye needn't compare the two lines.
                let (before_spans, after_spans) = modified_spans(&before.signature, sig);
                let mut row = vec![Span::raw(format!("{pad}~ "))];
                row.extend(after_spans);
                self.stop(
                    Line::from(row).yellow(),
                    Some(jump(view)),
                    expandable,
                    referenced,
                    Some(row_path.clone()),
                );
                let mut was = vec![Span::raw(format!("{pad}    was: "))];
                was.extend(before_spans);
                self.lines.push(Line::from(was).dim());
            }
            ItemStatus::Unchanged => {
                self.stop(
                    Line::from(format!("{pad}  {sig}")).dim(),
                    Some(jump(view)),
                    expandable,
                    referenced,
                    Some(row_path.clone()),
                );
            }
        }
        self.unfolded(view, indent, &row_path, ctx);
    }

    /// The definitions of the types `view` references, unfolded beneath
    /// its row. Every unfolded row is a real stop — jumpable,
    /// searchable, and expandable in turn; a reference back to a type
    /// already on the path renders as a cycle note instead of looping.
    fn expansion(&mut self, view: &ItemView, indent: usize, row_path: &RowPath, ctx: &RenderCtx) {
        let pad = " ".repeat(indent + 4);
        for r in &view.item.refs {
            let Some(resolved) = ctx.index.lookup(&r.0, &view.item.id.path) else {
                continue;
            };
            if row_path.contains(&resolved.def.item.id) {
                self.lines
                    .push(Line::from(format!("{pad}↺ {} (cycle)", r.0)).cyan().dim());
                continue;
            }
            self.definition_rows(&resolved, indent + 4, row_path, ctx);
        }
    }

    /// A resolved definition: its header row (`▸ signature · where`,
    /// noting contested names), collapsed by default — one `Tab` per
    /// level. Expanding the header reveals its members and then
    /// whatever the definition itself references.
    fn definition_rows(
        &mut self,
        resolved: &Resolved<'_>,
        indent: usize,
        parent: &RowPath,
        ctx: &RenderCtx,
    ) {
        let def = resolved.def;
        let pad = " ".repeat(indent);
        let def_path = parent.child(&def.item.id);
        let at = if resolved.candidates > 1 {
            format!(
                "  · {}:{} (1 of {} definitions)",
                def.item.id.path.display(),
                def.item.line,
                resolved.candidates
            )
        } else {
            format!("  · {}:{}", def.item.id.path.display(), def.item.line)
        };
        // The header unfolds members as well as references, so it is
        // expandable whenever it has either.
        let expandable = !def.members.is_empty() || can_expand(def, &def_path, ctx.index);
        self.stop(
            Line::from(vec![
                Span::raw(format!("{pad}▸ {}", def.item.signature)),
                Span::raw(at).dim(),
            ])
            .cyan(),
            Some(jump(def)),
            expandable,
            has_users(def, &def_path, ctx.index),
            Some(def_path.clone()),
        );
        if ctx.expanded.contains(&def_path) {
            for member in &def.members {
                let member_path = def_path.child(&member.item.id);
                self.stop(
                    Line::from(format!("{pad}  {}", member.item.signature))
                        .cyan()
                        .dim(),
                    Some(jump(member)),
                    can_expand(member, &member_path, ctx.index),
                    has_users(member, &member_path, ctx.index),
                    Some(member_path.clone()),
                );
                self.unfolded(member, indent + 2, &member_path, ctx);
            }
            self.expansion(def, indent, &def_path, ctx);
        }
        if ctx.usages_open.contains(&def_path) {
            self.usage_rows(def, indent, &def_path, ctx);
        }
    }

    /// The items whose signatures reference `view`'s item — the reverse
    /// direction — unfolded beneath its row. Same contract as expansion
    /// rows: each is a stop, jumpable and unfoldable in turn, with the
    /// row path as the cycle guard.
    fn usage_rows(&mut self, view: &ItemView, indent: usize, row_path: &RowPath, ctx: &RenderCtx) {
        let pad = " ".repeat(indent + 4);
        for user in ctx.index.users_of(&view.item.id.name) {
            if user.item.id == view.item.id || row_path.contains(&user.item.id) {
                continue;
            }
            let user_path = row_path.child(&user.item.id);
            let at = format!("  · {}:{}", user.item.id.path.display(), user.item.line);
            self.stop(
                Line::from(vec![
                    Span::raw(format!("{pad}◂ {}", user.item.signature)),
                    Span::raw(at).dim(),
                ])
                .magenta(),
                Some(jump(user)),
                can_expand(user, &user_path, ctx.index),
                has_users(user, &user_path, ctx.index),
                Some(user_path.clone()),
            );
            self.unfolded(user, indent + 4, &user_path, ctx);
        }
    }
}

/// Turns a review into the styled lines the pane scrolls over, the set of
/// cursor stops, and each stop's jump target. Layout mirrors the plain
/// frontend: members indented under their block header, composites set
/// off as paragraphs, files separated by a blank line. Every item row is
/// a stop (unchanged context included — it can still be expanded or
/// jumped to); removed rows and deleted files have nowhere to go.
fn render_lines(review: &[FileChange], ctx: &RenderCtx) -> Rendered {
    let mut b = LineBuilder::default();
    if review.is_empty() {
        b.lines
            .push(Line::from("No structural changes — the API surface is untouched.").dim());
        return b.into_rendered();
    }
    for (i, file) in review.iter().enumerate() {
        if i > 0 {
            b.lines.push(Line::default());
        }
        b.headers.push(b.stops.len());
        match &file.kind {
            FileChangeKind::Deleted => {
                b.stop(
                    Line::from(format!("DELETED {}", file.path.display()))
                        .red()
                        .bold(),
                    None,
                    false,
                    false,
                    None,
                );
            }
            FileChangeKind::Changed(changeset) => {
                b.stop(
                    Line::from(file.path.display().to_string()).bold(),
                    Some(JumpTarget {
                        path: file.path.clone(),
                        line: SourceLine(1),
                    }),
                    false,
                    false,
                    None,
                );
                let root = RowPath::default();
                let mut prev_was_composite = false;
                for block in &changeset.blocks {
                    let composite = !block.members.is_empty();
                    if composite || prev_was_composite {
                        b.lines.push(Line::default());
                    }
                    b.item_row(block, 2, &root, ctx);
                    for member in &block.members {
                        b.item_row(member, 6, &root, ctx);
                    }
                    prev_was_composite = composite;
                }
            }
        }
    }
    b.into_rendered()
}

/// The title-bar summary: `5 files · +12 ~3 -4`, with deleted files
/// counted among the files but not the change tallies.
fn summarize(review: &[FileChange]) -> String {
    let (mut added, mut modified, mut removed) = (0usize, 0usize, 0usize);
    let mut tally = |view: &ItemView| match view.status {
        ItemStatus::Added => added += 1,
        ItemStatus::Modified { .. } => modified += 1,
        ItemStatus::Removed => removed += 1,
        ItemStatus::Unchanged => {}
    };
    for file in review {
        if let FileChangeKind::Changed(changeset) = &file.kind {
            for block in &changeset.blocks {
                tally(block);
                for member in &block.members {
                    tally(member);
                }
            }
        }
    }
    let files = review.len();
    let plural = if files == 1 { "file" } else { "files" };
    format!("{files} {plural} · +{added} ~{modified} -{removed}")
}

/// Runs the interactive view until the user quits. The effectful edge:
/// sets up the terminal (raw mode + alternate screen via `ratatui::init`),
/// pumps key events, and restores the terminal on the way out.
pub(crate) fn run(
    review: &[FileChange],
    index: TypeIndex,
    editor: &impl EditorLauncher,
) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut App::new(review, index), editor);
    ratatui::restore();
    result
}

const HELP: &str =
    " j/k move · {/} files · / search · n/N matches · ⇥ expand · gr references · ↵ edit · q quit ";

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    editor: &impl EditorLauncher,
) -> io::Result<()> {
    let mut input = InputState::default();
    let mut mode = Mode::Normal;
    let mut query = String::new();
    let mut notice: Option<String> = None;
    loop {
        let footer = if mode == Mode::Search {
            format!(" /{query} ")
        } else {
            notice.take().unwrap_or_else(|| HELP.to_owned())
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
                    match motion {
                        Motion::Quit => return Ok(()),
                        Motion::Open => notice = open_in_editor(terminal, app, editor)?,
                        Motion::Expand => notice = app.toggle_expand(),
                        Motion::Usages => notice = app.toggle_usages(),
                        _ => app.apply(motion),
                    }
                }
            }
        }
    }
}

/// Opens the cursor's item in the user's editor, suspending the TUI
/// around the child process: the terminal is restored before the editor
/// takes the tty and re-initialized after it exits. Returns a footer
/// notice when there is nothing to open or the editor failed; editor
/// failure is a notice rather than an error because the review is still
/// usable.
fn open_in_editor(
    terminal: &mut DefaultTerminal,
    app: &App,
    editor: &impl EditorLauncher,
) -> io::Result<Option<String>> {
    let Some(target) = app.current_jump() else {
        return Ok(Some(
            " nothing to open here (removed items have no location) ".to_owned(),
        ));
    };
    ratatui::restore();
    let opened = editor.open(&target.path, target.line);
    *terminal = ratatui::init();
    terminal.clear()?;
    Ok(opened.err().map(|e| format!(" editor failed: {e} ")))
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &mut App, footer: &str) {
    let position = if app.stops.is_empty() {
        String::new()
    } else {
        format!(" {}/{} ", app.cursor + 1, app.stops.len())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" absolem · {} ", app.summary))
        .title_bottom(footer)
        .title_bottom(Line::from(position).right_aligned());
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
            parent: None,
            refs: Vec::new(),
        }
    }

    fn leaf(status: ItemStatus, item: Item) -> ItemView {
        ItemView {
            status,
            item,
            members: Vec::new(),
        }
    }

    fn added(item: Item) -> ItemView {
        leaf(ItemStatus::Added, item)
    }

    fn removed(item: Item) -> ItemView {
        leaf(ItemStatus::Removed, item)
    }

    fn modified(before: Item, after: Item) -> ItemView {
        leaf(ItemStatus::Modified { before }, after)
    }

    fn changed(path: &str, blocks: Vec<ItemView>) -> FileChange {
        FileChange {
            path: PathBuf::from(path),
            kind: FileChangeKind::Changed(ChangeSet { blocks }),
        }
    }

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// `App` with an empty type index — expansion is exercised separately.
    fn app(review: &[FileChange]) -> App {
        App::new(review, TypeIndex::default())
    }

    /// `render_lines` with no expansions and an empty index.
    fn render(review: &[FileChange]) -> Rendered {
        let (expanded, usages_open) = (HashSet::new(), HashSet::new());
        let index = TypeIndex::default();
        render_lines(
            review,
            &RenderCtx {
                expanded: &expanded,
                usages_open: &usages_open,
                index: &index,
            },
        )
    }

    /// Three added items in one file: 1 header + 3 change rows = 4 stops over
    /// 4 lines. Used by the cursor/scroll tests.
    fn three_items() -> App {
        app(&[changed(
            "a.go",
            vec![
                added(item("A", "func A()")),
                added(item("B", "func B()")),
                added(item("C", "func C()")),
            ],
        )])
    }

    /// One file with `n` added items: 1 header + n change rows = n+1 stops
    /// over n+1 lines. Used by the viewport-positioning tests.
    fn many_items(n: usize) -> App {
        let changes = (0..n)
            .map(|i| added(item(&format!("F{i}"), &format!("func F{i}()"))))
            .collect();
        app(&[changed("a.go", changes)])
    }

    #[test]
    fn empty_review_shows_placeholder() {
        let rendered = render(&[]);
        assert_eq!(rendered.lines.len(), 1);
        assert_eq!(
            text(&rendered.lines[0]),
            "No structural changes — the API surface is untouched."
        );
        assert!(rendered.stops.is_empty());
    }

    #[test]
    fn changed_file_renders_header_then_prefixed_changes() {
        let rendered = render(&[changed(
            "a.go",
            vec![
                added(item("A", "func A()")),
                modified(item("B", "func B()"), item("B", "func B(x int)")),
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
        let rendered = render(&[changed(
            "a.go",
            vec![modified(item("B", "func B()"), item("B", "func B(x int)"))],
        )]);
        // Lines: 0 header, 1 "~", 2 "was:". The continuation row is not a stop.
        assert_eq!(rendered.stops, vec![0, 1]);
    }

    #[test]
    fn change_lines_are_colored_by_kind() {
        let rendered = render(&[changed(
            "a.go",
            vec![added(item("A", "func A()")), removed(item("C", "func C()"))],
        )]);
        assert_eq!(rendered.lines[1].style.fg, Some(Color::Green));
        assert_eq!(rendered.lines[2].style.fg, Some(Color::Red));
    }

    #[test]
    fn files_separated_by_blank_line() {
        let rendered = render(&[
            changed("a.go", vec![added(item("A", "func A()"))]),
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
        let mut app = app(&[]);
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
    fn input_decodes_file_hops_with_counts() {
        let mut input = InputState::default();
        assert_eq!(
            input.feed(KeyCode::Char('{'), false),
            Some(Motion::PrevFile(1))
        );
        assert_eq!(input.feed(KeyCode::Char('2'), false), None);
        assert_eq!(
            input.feed(KeyCode::Char('}'), false),
            Some(Motion::NextFile(2))
        );
    }

    #[test]
    fn file_hops_move_between_headers_and_saturate() {
        let mut app = three_files(); // headers at stops 0, 2, 4
        app.viewport_height = 20;
        assert_eq!(app.headers, vec![0, 2, 4]);
        app.next_file(1);
        assert_eq!(app.cursor, 2);
        app.next_file(5); // saturates at the last header
        assert_eq!(app.cursor, 4);
        app.cursor_down(1); // onto c.go's change row
        app.prev_file(1);
        assert_eq!(app.cursor, 4);
        app.prev_file(2);
        assert_eq!(app.cursor, 0);
        app.prev_file(1); // already at the first header: stays
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn summary_counts_files_and_changes() {
        let review = vec![
            changed(
                "a.go",
                vec![
                    added(item("A", "func A()")),
                    modified(item("B", "func B()"), item("B", "func B(x int)")),
                    removed(item("C", "func C()")),
                ],
            ),
            FileChange {
                path: PathBuf::from("gone.go"),
                kind: FileChangeKind::Deleted,
            },
        ];
        assert_eq!(summarize(&review), "2 files · +1 ~1 -1");
        assert_eq!(summarize(&review[..1]), "1 file · +1 ~1 -1");
    }

    #[test]
    fn input_decodes_enter_as_open() {
        let mut input = InputState::default();
        assert_eq!(input.feed(KeyCode::Enter, false), Some(Motion::Open));
    }

    #[test]
    fn input_decodes_tab_as_expand() {
        let mut input = InputState::default();
        assert_eq!(input.feed(KeyCode::Tab, false), Some(Motion::Expand));
    }

    #[test]
    fn tokenize_keeps_word_runs_whole() {
        assert_eq!(
            tokenize("func F(x int)"),
            vec!["func", " ", "F", "(", "x", " ", "int", ")"]
        );
    }

    #[test]
    fn modified_rows_mark_only_the_changed_tokens() {
        let rendered = render(&[changed(
            "a.go",
            vec![modified(item("B", "func B()"), item("B", "func B(x int)"))],
        )]);
        // Row text is unchanged by the span split…
        assert_eq!(text(&rendered.lines[1]), "  ~ func B(x int)");
        // …and the inserted parameter is the marked region.
        let marked: String = rendered.lines[1]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::UNDERLINED))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(marked, "x int");
        // The old side has nothing struck out — nothing was removed.
        assert!(
            rendered.lines[2]
                .spans
                .iter()
                .all(|s| !s.style.add_modifier.contains(Modifier::CROSSED_OUT))
        );
    }

    #[test]
    fn modified_rows_strike_removed_tokens_on_the_was_line() {
        let rendered = render(&[changed(
            "a.go",
            vec![modified(
                item("B", "func B(retries int)"),
                item("B", "func B()"),
            )],
        )]);
        let struck: String = rendered.lines[2]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::CROSSED_OUT))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(struck, "retries int");
    }

    /// An index defining `Client` with one field, and a review whose one
    /// item references `Client`.
    fn expandable_app() -> App {
        let mut client = item("Client", "type Client struct");
        client.id.kind = Kind::Struct;
        let mut field = item("Client.timeout", "Client.timeout int");
        field.id.kind = Kind::Field;
        field.parent = Some("Client".into());
        let mut client_surface = crate::surface::Surface::new();
        client_surface.push(client);
        client_surface.push(field);
        let index = TypeIndex::build(&[client_surface]);

        let mut connect = item("Connect", "func Connect() *Client");
        connect.refs = vec![crate::item::TypeRef("Client".into())];
        App::new(&[changed("a.go", vec![added(connect)])], index)
    }

    #[test]
    fn tab_expands_referenced_types_inline_and_collapses_again() {
        let mut app = expandable_app();
        app.viewport_height = 20;
        assert_eq!(app.lines.len(), 2); // header + row
        assert_eq!(app.expandable, vec![false, true]);

        app.cursor = 1;
        assert_eq!(app.toggle_expand(), None);
        let texts: Vec<String> = app.lines.iter().map(text).collect();
        // One Tab, one level: the definition header only, members
        // collapsed behind their own Tab.
        assert_eq!(texts[2], "      ▸ type Client struct  · f.go:1");
        assert_eq!(app.lines.len(), 3);
        assert_eq!(app.stops.len(), 3);
        // The header row is a stop with a jump of its own; the cursor
        // stayed on its row.
        assert_eq!(app.cursor, 1);
        assert_eq!(app.jumps[2].as_ref().unwrap().path, PathBuf::from("f.go"));
        assert!(app.expandable[2]); // it has members to unfold

        // Tab on the header unfolds the members.
        app.cursor = 2;
        assert_eq!(app.toggle_expand(), None);
        let texts: Vec<String> = app.lines.iter().map(text).collect();
        assert_eq!(texts[3], "        Client.timeout int");
        assert_eq!(app.stops.len(), 4);
        assert_eq!(app.jumps[3].as_ref().unwrap().path, PathBuf::from("f.go"));

        // Collapse both levels again.
        assert_eq!(app.toggle_expand(), None);
        assert_eq!(app.stops.len(), 3);
        app.cursor = 1;
        assert_eq!(app.toggle_expand(), None);
        assert_eq!(app.lines.len(), 2);
        assert_eq!(app.stops.len(), 2);
    }

    #[test]
    fn expansion_recurses_and_guards_cycles() {
        // Node refs Edge; Edge refs Node (a cycle) and Weight (not).
        let mut node = item("Node", "type Node struct");
        node.id.kind = Kind::Struct;
        node.refs = vec![crate::item::TypeRef("Edge".into())];
        let mut edge = item("Edge", "type Edge struct");
        edge.id.kind = Kind::Struct;
        edge.refs = vec![
            crate::item::TypeRef("Node".into()),
            crate::item::TypeRef("Weight".into()),
        ];
        let mut weight = item("Weight", "type Weight float64");
        weight.id.kind = Kind::Type;
        let mut s = crate::surface::Surface::new();
        s.push(node);
        s.push(edge);
        s.push(weight);
        let index = TypeIndex::build(&[s]);

        let mut f = item("Walk", "func Walk(n Node)");
        f.refs = vec![crate::item::TypeRef("Node".into())];
        let mut app = App::new(&[changed("a.go", vec![added(f)])], index);
        app.viewport_height = 30;

        // Expand the review row: Node's definition unfolds.
        app.cursor = 1;
        assert_eq!(app.toggle_expand(), None);
        assert_eq!(text(&app.lines[2]), "      ▸ type Node struct  · f.go:1");
        // The unfolded Node row is itself expandable (it refs Edge).
        app.cursor = 2;
        assert!(app.expandable[2]);
        assert_eq!(app.toggle_expand(), None);
        assert_eq!(
            text(&app.lines[3]),
            "          ▸ type Edge struct  · f.go:1"
        );
        // Expanding Edge: the ref back to Node is already on this row's
        // path, so it renders as a cycle note; Weight unfolds normally.
        app.cursor = 3;
        assert_eq!(app.toggle_expand(), None);
        assert_eq!(text(&app.lines[4]), "              ↺ Node (cycle)");
        assert_eq!(
            text(&app.lines[5]),
            "              ▸ type Weight float64  · f.go:1"
        );
        // The cursor stayed on the Edge row through the re-render.
        assert_eq!(app.cursor, 3);
        // The Weight row's only surroundings are cyclic or ref-free: it
        // has no refs of its own, so it is not expandable.
        assert!(!app.expandable[4]);
    }

    #[test]
    fn input_decodes_gr_as_usages_and_keeps_gg_working() {
        let mut input = InputState::default();
        assert_eq!(input.feed(KeyCode::Char('g'), false), None);
        assert_eq!(input.feed(KeyCode::Char('r'), false), Some(Motion::Usages));
        assert_eq!(input.feed(KeyCode::Char('g'), false), None);
        assert_eq!(
            input.feed(KeyCode::Char('g'), false),
            Some(Motion::First(None))
        );
    }

    #[test]
    fn gr_unfolds_the_used_by_list_and_collapses_again() {
        // The index knows Client and a function referencing it; the
        // review shows Client itself.
        let mut client = item("Client", "type Client struct");
        client.id.kind = Kind::Struct;
        let mut connect = item("Connect", "func Connect() *Client");
        connect.refs = vec![crate::item::TypeRef("Client".into())];
        let mut s = crate::surface::Surface::new();
        s.push(client.clone());
        s.push(connect);
        let index = TypeIndex::build(&[s]);

        let mut app = App::new(
            &[changed(
                "a.go",
                vec![modified(item("Client", "type Client struct"), client)],
            )],
            index,
        );
        app.viewport_height = 20;

        app.cursor = 1;
        assert!(app.referenced[1]);
        assert_eq!(app.toggle_usages(), None);
        // Row 0 header, 1 the ~ row, 2 its was: line, 3 the user.
        assert_eq!(
            text(&app.lines[3]),
            "      ◂ func Connect() *Client  · f.go:1"
        );
        // The usage row is a stop with a jump of its own.
        assert_eq!(app.stops.len(), 3);
        assert_eq!(app.jumps[2].as_ref().unwrap().path, PathBuf::from("f.go"));

        assert_eq!(app.toggle_usages(), None);
        assert_eq!(app.stops.len(), 2);
    }

    #[test]
    fn gr_on_an_unreferenced_row_notices() {
        let mut app = expandable_app();
        app.viewport_height = 20;
        app.cursor = 1; // Connect: nothing references a function's name
        assert!(app.toggle_usages().is_some());
    }

    #[test]
    fn tab_on_an_unexpandable_row_notices() {
        let mut app = expandable_app();
        app.viewport_height = 20;
        app.cursor = 0; // the file header
        assert!(app.toggle_expand().is_some());
        assert_eq!(app.lines.len(), 2);
    }

    #[test]
    fn stops_jump_to_head_side_lines() {
        let with_lines = |name: &str, sig: &str, line: u32| {
            let mut i = item(name, sig);
            i.line = crate::item::Line(line);
            i
        };
        let app = app(&[
            changed(
                "a.go",
                vec![
                    added(with_lines("A", "func A()", 10)),
                    modified(
                        with_lines("B", "func B()", 12),
                        with_lines("B", "func B(x int)", 20),
                    ),
                    removed(with_lines("C", "func C()", 30)),
                ],
            ),
            FileChange {
                path: PathBuf::from("gone.go"),
                kind: FileChangeKind::Deleted,
            },
        ]);
        let jump = |i: usize| app.jumps[i].clone();
        // File header jumps to the top of the file.
        assert_eq!(
            jump(0),
            Some(JumpTarget {
                path: PathBuf::from("a.go"),
                line: crate::item::Line(1),
            })
        );
        // Added jumps to its line; modified to the *after* line.
        assert_eq!(jump(1).unwrap().line, crate::item::Line(10));
        assert_eq!(jump(2).unwrap().line, crate::item::Line(20));
        // Removed items and deleted files have nowhere to go.
        assert_eq!(jump(3), None);
        assert_eq!(jump(4), None);
    }

    #[test]
    fn current_jump_follows_the_cursor() {
        let mut app = app(&[changed("a.go", vec![added(item("A", "func A()"))])]);
        app.viewport_height = 10;
        assert_eq!(app.current_jump().unwrap().line, crate::item::Line(1));
        app.cursor_down(1);
        // Item rows jump to the item's own declaration site.
        assert_eq!(app.current_jump().unwrap().path, PathBuf::from("f.go"));
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
        app(&[
            changed("a.go", vec![added(item("Alpha", "func Alpha()"))]),
            changed("b.go", vec![added(item("Beta", "func Beta()"))]),
            changed("c.go", vec![added(item("Gamma", "func Gamma()"))]),
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
