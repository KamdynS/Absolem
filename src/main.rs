//! `absolem` entrypoint and composition root.

mod core;
mod git;
mod item;
mod json;
mod markdown;
mod producer;
mod render;
mod surface;
mod tui;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::Parser;

use crate::core::{FileChange, FileChangeKind, TypeIndex, diff};
use crate::git::{ChangeStatus, ChangedFile, DiffMode, GitRepo, RealGit, RefRange};
use crate::item::Line;
use crate::producer::{Producer, ProducerError, Registry};
use crate::surface::Surface;
use crate::tui::EditorLauncher;

/// The user's editor, invoked as `editor +LINE PATH` — the convention
/// the vi lineage (nvim included), nano, and emacs all accept. Strictly
/// `$VISUAL` / `$EDITOR`: absolem never guesses an editor the user
/// didn't choose. Constructed only here, at the composition root.
struct RealEditor {
    program: Option<String>,
    /// Review paths are repo-root-relative; they are joined here so the
    /// editor opens the right file however deep the invocation was.
    repo_root: PathBuf,
}

impl RealEditor {
    fn from_env(repo_root: PathBuf) -> Self {
        let program = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .ok()
            .filter(|p| !p.trim().is_empty());
        Self { program, repo_root }
    }
}

impl EditorLauncher for RealEditor {
    fn open(&self, path: &Path, line: Line) -> std::io::Result<()> {
        let Some(program) = &self.program else {
            return Err(std::io::Error::other(
                "$VISUAL / $EDITOR is not set — set one to jump into your editor",
            ));
        };
        let status = std::process::Command::new(program)
            .arg(format!("+{line}"))
            .arg(self.repo_root.join(path))
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "{program} exited with {status}"
            )))
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Surface the structural shape of a change before review."
)]
struct Cli {
    /// What to review: `base..head`, `base...head`, or a bare `base`
    /// (head defaults to HEAD). A bare base and `...` diff against the
    /// merge base, matching what forges show on a PR; `..` diffs the two
    /// trees directly. Defaults to origin/main (or origin/master)...HEAD.
    range: Option<String>,

    /// Print plain text instead of opening the interactive view. The
    /// default already falls back to plain text when stdout is not a
    /// terminal, so this is for forcing it (e.g. piping into a pager).
    #[arg(long, group = "output")]
    plain: bool,

    /// Emit the review as JSON (schema-versioned), for machine consumers
    /// like editor plugins.
    #[arg(long, group = "output")]
    json: bool,

    /// Emit the review as forge-flavored markdown, for a CI pipeline
    /// comment.
    #[arg(long, group = "output")]
    markdown: bool,
}

/// Which frontend gets the review. Decided once, at the edge, from the
/// flags and whether stdout is a terminal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Interactive,
    Plain,
    Json,
    Markdown,
}

impl Cli {
    fn output_mode(&self) -> OutputMode {
        if self.json {
            OutputMode::Json
        } else if self.markdown {
            OutputMode::Markdown
        } else if self.plain || !std::io::stdout().is_terminal() {
            OutputMode::Plain
        } else {
            OutputMode::Interactive
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // All paths in a review are repo-root-relative, so the process is
    // re-homed to the top level: running from a subdirectory then
    // behaves identically to running from the root.
    let cwd = std::env::current_dir().context("failed to read current working directory")?;
    let repo_root = RealGit::new(cwd)
        .toplevel()
        .context("not inside a git repository")?;
    let git = RealGit::new(repo_root.clone());

    let range = resolve_range(&git, cli.range.as_deref())?;
    let changed = git
        .changed_files(&range)
        .with_context(|| format!("git diff {}..{} failed", range.base, range.head))?;

    let mut registry = Registry::with_defaults().context("failed to initialize parsers")?;
    let source_files: Vec<_> = changed
        .into_iter()
        .filter(|f| registry.supports(&f.path))
        .collect();

    // Three-dot reviews compare against the merge base, so base content
    // must be read there — not at the base tip, which may have moved on.
    let base_rev = match range.mode {
        DiffMode::MergeBase => git
            .merge_base(&range.base, &range.head)
            .with_context(|| format!("git merge-base {} {} failed", range.base, range.head))?,
        DiffMode::Direct => range.base.clone(),
    };

    let review = build_review(&git, &mut registry, &base_rev, &range.head, &source_files)?;

    match cli.output_mode() {
        OutputMode::Plain => {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            render::render_review(&mut out, &review)?;
        }
        OutputMode::Json => {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            json::render_json(&mut out, &review)?;
        }
        OutputMode::Markdown => {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            markdown::render_markdown(&mut out, &review)?;
        }
        OutputMode::Interactive => {
            let index = build_type_index(&git, &mut registry, &range.head);
            tui::run(&review, index, &RealEditor::from_env(repo_root))
                .context("interactive view failed")?;
        }
    }

    Ok(())
}

/// Diffs each changed file into the review the frontends consume. Files
/// whose API shape did not move are dropped here, so every `FileChange`
/// that survives is worth rendering.
fn build_review(
    git: &impl GitRepo,
    registry: &mut Registry,
    base_rev: &str,
    head_rev: &str,
    files: &[ChangedFile],
) -> Result<Vec<FileChange>> {
    let mut review = Vec::new();
    for file in files {
        if file.status == ChangeStatus::Deleted {
            review.push(FileChange {
                path: file.path.clone(),
                kind: FileChangeKind::Deleted,
            });
            continue;
        }
        let Some(producer) = registry.for_path(&file.path) else {
            continue;
        };
        let base_surface = match file.status {
            ChangeStatus::Added => Surface::new(),
            _ => surface_at(git, producer, base_rev, &file.path)?,
        };
        let head_surface = surface_at(git, producer, head_rev, &file.path)?;
        let changeset = diff(&base_surface, &head_surface);
        if !changeset.is_empty() {
            review.push(FileChange {
                path: file.path.clone(),
                kind: FileChangeKind::Changed(changeset),
            });
        }
    }
    Ok(review)
}

/// Extracts every supported file at `head` into the index the TUI
/// resolves type expansions against. Best-effort by design: a file that
/// fails to read or parse is skipped, never fatal — the index only
/// powers an optional affordance, not the review itself.
fn build_type_index(git: &impl GitRepo, registry: &mut Registry, head: &str) -> TypeIndex {
    let Ok(files) = git.ls_files(head) else {
        return TypeIndex::default();
    };
    let supported: Vec<_> = files.into_iter().filter(|p| registry.supports(p)).collect();
    let mut surfaces = Vec::new();
    for path in &supported {
        let Some(producer) = registry.for_path(path) else {
            continue;
        };
        if let Ok(source) = git.read_at(head, path)
            && let Ok(surface) = producer.extract(path, &source)
        {
            surfaces.push(surface);
        }
    }
    TypeIndex::build(&surfaces)
}

fn surface_at(
    git: &impl GitRepo,
    producer: &mut dyn Producer,
    rev: &str,
    path: &Path,
) -> Result<Surface> {
    let source = git
        .read_at(rev, path)
        .with_context(|| format!("git show {rev}:{} failed", path.display()))?;
    producer
        .extract(path, &source)
        .map_err(|e: ProducerError| anyhow!(e))
        .with_context(|| format!("failed to parse {}@{rev}", path.display()))
}

/// Turns the optional CLI argument into a concrete `RefRange`. A missing
/// or empty side falls back to the default: origin/main (or
/// origin/master) for the base, HEAD for the head.
fn resolve_range(git: &impl GitRepo, raw: Option<&str>) -> Result<RefRange> {
    let (base, head, mode) = raw.map_or((None, None, DiffMode::MergeBase), parse_range);
    let base = match base {
        Some(b) => b.to_owned(),
        None => resolve_base(git)?,
    };
    Ok(RefRange {
        base,
        head: head.unwrap_or("HEAD").to_owned(),
        mode,
    })
}

/// A range side that was present and non-empty.
fn side(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

/// Splits `base...head`, `base..head`, or a bare `base` into its sides
/// and the diff semantics. Empty sides come back as `None`.
fn parse_range(raw: &str) -> (Option<&str>, Option<&str>, DiffMode) {
    let (base, head, mode) = if let Some((base, head)) = raw.split_once("...") {
        (base, head, DiffMode::MergeBase)
    } else if let Some((base, head)) = raw.split_once("..") {
        (base, head, DiffMode::Direct)
    } else {
        (raw, "", DiffMode::MergeBase)
    };
    (side(base), side(head), mode)
}

fn resolve_base(git: &impl GitRepo) -> Result<String> {
    if git
        .ref_exists("origin/main")
        .context("git rev-parse origin/main failed")?
    {
        Ok("origin/main".into())
    } else if git
        .ref_exists("origin/master")
        .context("git rev-parse origin/master failed")?
    {
        Ok("origin/master".into())
    } else {
        Err(anyhow!(
            "neither origin/main nor origin/master exists in this repo"
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::git::{ChangedFile, GitError};

    #[derive(Default)]
    struct FakeGit {
        main_exists: bool,
        master_exists: bool,
        files: Vec<ChangedFile>,
        contents: HashMap<String, String>,
    }

    impl GitRepo for FakeGit {
        fn ref_exists(&self, ref_name: &str) -> Result<bool, GitError> {
            Ok(match ref_name {
                "origin/main" => self.main_exists,
                "origin/master" => self.master_exists,
                _ => false,
            })
        }

        fn changed_files(&self, _range: &RefRange) -> Result<Vec<ChangedFile>, GitError> {
            Ok(self.files.clone())
        }

        /// Identity: the fake treats every base ref as its own merge base.
        fn merge_base(&self, a: &str, _b: &str) -> Result<String, GitError> {
            Ok(a.to_owned())
        }

        fn read_at(&self, rev: &str, path: &Path) -> Result<String, GitError> {
            let key = format!("{}:{}", rev, path.display());
            self.contents
                .get(&key)
                .cloned()
                .ok_or_else(|| GitError::UnexpectedOutput(format!("no content for {key}")))
        }

        fn ls_files(&self, _rev: &str) -> Result<Vec<PathBuf>, GitError> {
            Ok(self.files.iter().map(|f| f.path.clone()).collect())
        }
    }

    fn render_all(git: &FakeGit) -> String {
        let mut registry = Registry::with_defaults().unwrap();
        let review = build_review(git, &mut registry, "origin/main", "HEAD", &git.files).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        render::render_review(&mut buf, &review).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn resolves_origin_main_when_present() {
        let git = FakeGit {
            main_exists: true,
            ..Default::default()
        };
        assert_eq!(resolve_base(&git).unwrap(), "origin/main");
    }

    #[test]
    fn falls_back_to_origin_master() {
        let git = FakeGit {
            master_exists: true,
            ..Default::default()
        };
        assert_eq!(resolve_base(&git).unwrap(), "origin/master");
    }

    #[test]
    fn errors_when_neither_base_exists() {
        let git = FakeGit::default();
        assert!(resolve_base(&git).is_err());
    }

    #[test]
    fn parse_range_splits_two_and_three_dot() {
        assert_eq!(
            parse_range("a..b"),
            (Some("a"), Some("b"), DiffMode::Direct)
        );
        assert_eq!(
            parse_range("a...b"),
            (Some("a"), Some("b"), DiffMode::MergeBase)
        );
        assert_eq!(
            parse_range("main"),
            (Some("main"), None, DiffMode::MergeBase)
        );
        assert_eq!(
            parse_range("main.."),
            (Some("main"), None, DiffMode::Direct)
        );
        assert_eq!(
            parse_range("..feature"),
            (None, Some("feature"), DiffMode::Direct)
        );
        assert_eq!(
            parse_range("...feature"),
            (None, Some("feature"), DiffMode::MergeBase)
        );
    }

    #[test]
    fn resolve_range_defaults_missing_sides() {
        let git = FakeGit {
            main_exists: true,
            ..Default::default()
        };
        assert_eq!(
            resolve_range(&git, None).unwrap(),
            RefRange {
                base: "origin/main".into(),
                head: "HEAD".into(),
                mode: DiffMode::MergeBase,
            }
        );
        assert_eq!(
            resolve_range(&git, Some("v1.0..v2.0")).unwrap(),
            RefRange {
                base: "v1.0".into(),
                head: "v2.0".into(),
                mode: DiffMode::Direct,
            }
        );
        assert_eq!(
            resolve_range(&git, Some("release")).unwrap(),
            RefRange {
                base: "release".into(),
                head: "HEAD".into(),
                mode: DiffMode::MergeBase,
            }
        );
        // An empty base side falls back to the resolved default base.
        assert_eq!(
            resolve_range(&git, Some("...feature")).unwrap(),
            RefRange {
                base: "origin/main".into(),
                head: "feature".into(),
                mode: DiffMode::MergeBase,
            }
        );
    }

    #[test]
    fn prefers_origin_main_over_master() {
        let git = FakeGit {
            main_exists: true,
            master_exists: true,
            ..Default::default()
        };
        assert_eq!(resolve_base(&git).unwrap(), "origin/main");
    }

    #[test]
    fn added_file_renders_all_items_as_added() {
        let mut contents = HashMap::new();
        contents.insert("HEAD:new.go".into(), "package x\nfunc F() {}\n".into());
        let git = FakeGit {
            files: vec![ChangedFile {
                path: PathBuf::from("new.go"),
                status: ChangeStatus::Added,
            }],
            contents,
            ..Default::default()
        };
        assert_eq!(render_all(&git), "new.go\n  + func F()\n");
    }

    #[test]
    fn deleted_file_prints_deleted_marker() {
        let git = FakeGit {
            files: vec![ChangedFile {
                path: PathBuf::from("gone.go"),
                status: ChangeStatus::Deleted,
            }],
            contents: HashMap::from([(
                "origin/main:gone.go".into(),
                "package x\nfunc F() {}\n".into(),
            )]),
            ..Default::default()
        };
        assert_eq!(render_all(&git), "DELETED gone.go\n");
    }

    #[test]
    fn modified_file_emits_only_shape_changes() {
        let mut contents = HashMap::new();
        contents.insert(
            "origin/main:f.go".into(),
            "package x\nfunc F() {}\nfunc G() {}\n".into(),
        );
        contents.insert(
            "HEAD:f.go".into(),
            "package x\nfunc F(x int) {}\nfunc G() {}\nfunc H() {}\n".into(),
        );
        let git = FakeGit {
            files: vec![ChangedFile {
                path: PathBuf::from("f.go"),
                status: ChangeStatus::Modified,
            }],
            contents,
            ..Default::default()
        };
        assert_eq!(
            render_all(&git),
            "f.go\n  ~ func F(x int)\n      was: func F()\n  + func H()\n"
        );
    }

    #[test]
    fn body_only_change_renders_nothing() {
        let mut contents = HashMap::new();
        contents.insert(
            "origin/main:f.go".into(),
            "package x\nfunc F() int { return 1 }\n".into(),
        );
        contents.insert(
            "HEAD:f.go".into(),
            "package x\nfunc F() int { return 2 }\n".into(),
        );
        let git = FakeGit {
            files: vec![ChangedFile {
                path: PathBuf::from("f.go"),
                status: ChangeStatus::Modified,
            }],
            contents,
            ..Default::default()
        };
        assert_eq!(render_all(&git), "");
    }

    #[test]
    fn multiple_files_separated_by_blank_line() {
        let mut contents = HashMap::new();
        contents.insert("HEAD:a.go".into(), "package x\nfunc A() {}\n".into());
        contents.insert("HEAD:b.go".into(), "package x\nfunc B() {}\n".into());
        let git = FakeGit {
            files: vec![
                ChangedFile {
                    path: PathBuf::from("a.go"),
                    status: ChangeStatus::Added,
                },
                ChangedFile {
                    path: PathBuf::from("b.go"),
                    status: ChangeStatus::Added,
                },
            ],
            contents,
            ..Default::default()
        };
        assert_eq!(
            render_all(&git),
            "a.go\n  + func A()\n\nb.go\n  + func B()\n"
        );
    }

    #[test]
    fn unchanged_file_does_not_emit_separator() {
        let mut contents = HashMap::new();
        contents.insert(
            "origin/main:unchanged.go".into(),
            "package x\nfunc F() int { return 1 }\n".into(),
        );
        contents.insert(
            "HEAD:unchanged.go".into(),
            "package x\nfunc F() int { return 2 }\n".into(),
        );
        contents.insert("HEAD:b.go".into(), "package x\nfunc B() {}\n".into());
        let git = FakeGit {
            files: vec![
                ChangedFile {
                    path: PathBuf::from("unchanged.go"),
                    status: ChangeStatus::Modified,
                },
                ChangedFile {
                    path: PathBuf::from("b.go"),
                    status: ChangeStatus::Added,
                },
            ],
            contents,
            ..Default::default()
        };
        assert_eq!(render_all(&git), "b.go\n  + func B()\n");
    }
}
