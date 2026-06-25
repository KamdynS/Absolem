//! The interactive TUI frontend: renders a review in a scrollable pane.
//!
//! Split in two. `review_lines` and the scroll arithmetic on `App` are
//! pure — they turn a review into styled lines and clamp the viewport, and
//! are unit-tested with no terminal. `run` is the effectful edge: it owns
//! the terminal and the keyboard event loop, the way `RealGit` owns the
//! `git` subprocess. Only the composition root calls it.
//!
//! Same convention as the plain-text frontend (`+`/`-`/`~`), here carried by
//! color: added green, removed red, modified yellow with the old signature
//! dimmed beneath. No resolution, no navigation — just shape.

use std::io;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::core::{Change, FileChange, FileChangeKind};

/// Scrollable view over a pre-rendered review. `scroll` is the index of
/// the topmost visible line; `viewport_height` is the last drawn body
/// height, recorded each frame so the scroll handlers can clamp without
/// the terminal.
struct App {
    lines: Vec<Line<'static>>,
    scroll: u16,
    viewport_height: u16,
}

impl App {
    fn new(review: &[FileChange]) -> Self {
        Self {
            lines: review_lines(review),
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

    fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_add(1).min(self.max_scroll());
    }

    const fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
    }

    fn page_down(&mut self) {
        self.scroll = self
            .scroll
            .saturating_add(self.viewport_height)
            .min(self.max_scroll());
    }

    const fn page_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(self.viewport_height);
    }

    const fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll = self.max_scroll();
    }
}

/// Turns a review into the styled lines the pane scrolls over. Files are
/// separated by a blank line, mirroring the plain-text frontend.
fn review_lines(review: &[FileChange]) -> Vec<Line<'static>> {
    if review.is_empty() {
        return vec![Line::from("No structural changes.").dim()];
    }
    let mut lines = Vec::new();
    for (i, file) in review.iter().enumerate() {
        if i > 0 {
            lines.push(Line::default());
        }
        match &file.kind {
            FileChangeKind::Deleted => {
                lines.push(
                    Line::from(format!("DELETED {}", file.path.display()))
                        .red()
                        .bold(),
                );
            }
            FileChangeKind::Changed(changeset) => {
                lines.push(Line::from(file.path.display().to_string()).bold());
                for change in &changeset.changes {
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
    lines
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

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('j') | KeyCode::Down => app.scroll_down(),
            KeyCode::Char('k') | KeyCode::Up => app.scroll_up(),
            KeyCode::Char('f') | KeyCode::PageDown => app.page_down(),
            KeyCode::Char('b') | KeyCode::PageUp => app.page_up(),
            KeyCode::Char('g') | KeyCode::Home => app.scroll_to_top(),
            KeyCode::Char('G') | KeyCode::End => app.scroll_to_bottom(),
            _ => {}
        }
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &mut App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" absolem — shape of the change ")
        .title_bottom(" j/k scroll · f/b page · g/G top/bottom · q quit ");
    // Record the inner height so scroll clamping matches what's on screen.
    app.viewport_height = block.inner(frame.area()).height;
    app.scroll = app.scroll.min(app.max_scroll());
    let paragraph = Paragraph::new(app.lines.clone())
        .block(block)
        .scroll((app.scroll, 0));
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

    #[test]
    fn empty_review_shows_placeholder() {
        let lines = review_lines(&[]);
        assert_eq!(lines.len(), 1);
        assert_eq!(text(&lines[0]), "No structural changes.");
    }

    #[test]
    fn changed_file_renders_header_then_prefixed_changes() {
        let lines = review_lines(&[changed(
            "a.go",
            vec![
                Change::Added(item("A", "func A()")),
                Change::Modified {
                    before: item("B", "func B()"),
                    after: item("B", "func B(x int)"),
                },
            ],
        )]);
        let rendered: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(
            rendered,
            vec![
                "a.go",
                "  + func A()",
                "  ~ func B(x int)",
                "      was: func B()",
            ]
        );
    }

    #[test]
    fn change_lines_are_colored_by_kind() {
        let lines = review_lines(&[changed(
            "a.go",
            vec![
                Change::Added(item("A", "func A()")),
                Change::Removed(item("C", "func C()")),
            ],
        )]);
        assert_eq!(lines[1].style.fg, Some(Color::Green));
        assert_eq!(lines[2].style.fg, Some(Color::Red));
    }

    #[test]
    fn files_separated_by_blank_line() {
        let lines = review_lines(&[
            changed("a.go", vec![Change::Added(item("A", "func A()"))]),
            FileChange {
                path: PathBuf::from("b.go"),
                kind: FileChangeKind::Deleted,
            },
        ]);
        let rendered: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(rendered, vec!["a.go", "  + func A()", "", "DELETED b.go"]);
    }

    #[test]
    fn scroll_clamps_to_last_line() {
        let mut app = App::new(&[changed(
            "a.go",
            vec![
                Change::Added(item("A", "func A()")),
                Change::Added(item("B", "func B()")),
                Change::Added(item("C", "func C()")),
            ],
        )]);
        app.viewport_height = 2; // 4 lines total, 2 visible → max scroll 2.
        for _ in 0..10 {
            app.scroll_down();
        }
        assert_eq!(app.scroll, 2);
        app.scroll_to_top();
        assert_eq!(app.scroll, 0);
        app.scroll_up();
        assert_eq!(app.scroll, 0);
        app.scroll_to_bottom();
        assert_eq!(app.scroll, 2);
    }

    #[test]
    fn scroll_max_is_zero_when_everything_fits() {
        let mut app = App::new(&[changed("a.go", vec![Change::Added(item("A", "func A()"))])]);
        app.viewport_height = 20;
        app.scroll_down();
        assert_eq!(app.scroll, 0);
    }
}
