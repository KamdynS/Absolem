//! The `GitRepo` capability and its real shell-out-to-`git` implementation.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub(crate) enum GitError {
    #[error("failed to spawn git: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("git exited with status {status}: {stderr}")]
    GitFailed { status: i32, stderr: String },
    #[error("unexpected git output: {0}")]
    UnexpectedOutput(String),
    #[error("failed to read {path} from the working tree: {reason}")]
    WorktreeRead { path: String, reason: String },
}

/// A reviewable state of the tree: a committed ref, or the working tree
/// as it sits on disk, uncommitted changes and all. Only the head side
/// of a review may be the working tree — the base is always a ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Rev {
    Ref(String),
    WorkingTree,
}

impl fmt::Display for Rev {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ref(name) => name.fmt(f),
            Self::WorkingTree => "worktree".fmt(f),
        }
    }
}

/// How the two refs of a review are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffMode {
    /// Three-dot: changes since the merge base — what GitHub/GitLab show
    /// on a PR, and the default.
    MergeBase,
    /// Two-dot: a direct tree-to-tree diff between the refs.
    Direct,
}

/// The two sides under review, plus the semantics for comparing them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefRange {
    pub(crate) base: String,
    pub(crate) head: Rev,
    pub(crate) mode: DiffMode,
}

impl RefRange {
    /// The argument `git diff` expects. Git's own grammar already covers
    /// the working tree: a bare `base` diffs it directly, `base...`
    /// diffs it against the merge base.
    fn to_git_range(&self) -> String {
        let dots = match self.mode {
            DiffMode::MergeBase => "...",
            DiffMode::Direct => "..",
        };
        match &self.head {
            Rev::Ref(head) => format!("{}{}{}", self.base, dots, head),
            Rev::WorkingTree => match self.mode {
                DiffMode::MergeBase => format!("{}...", self.base),
                DiffMode::Direct => self.base.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedFile {
    pub(crate) path: PathBuf,
    pub(crate) status: ChangeStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChangeStatus {
    Added,
    Modified,
    Deleted,
}

pub(crate) trait GitRepo {
    /// Returns true if `ref_name` resolves; used to choose between
    /// `origin/main` and `origin/master`.
    fn ref_exists(&self, ref_name: &str) -> Result<bool, GitError>;

    /// Files changed across `range`, honoring its two-dot/three-dot
    /// semantics.
    fn changed_files(&self, range: &RefRange) -> Result<Vec<ChangedFile>, GitError>;

    /// The merge base of two refs. Three-dot reviews read base content
    /// here, so work that landed on base after the branch point is not
    /// misattributed to the branch.
    fn merge_base(&self, a: &str, b: &str) -> Result<String, GitError>;

    /// Read the contents of `path` at `rev` — a committed ref or the
    /// working tree.
    fn read_at(&self, rev: &Rev, path: &Path) -> Result<String, GitError>;

    /// Every file in the tree at `rev`, repo-relative. Feeds the
    /// head-wide type index that resolves `TypeRef`s by name.
    fn ls_files(&self, rev: &Rev) -> Result<Vec<PathBuf>, GitError>;

    /// Fetch the head of forge pull/merge request `number` from origin
    /// and return a rev that resolves to it.
    fn fetch_pr_head(&self, number: u32) -> Result<String, GitError>;
}

/// The ref a forge publishes a PR/MR head under, chosen from the origin
/// URL: GitLab uses `refs/merge-requests/`, GitHub (and the Gitea
/// family) use `refs/pull/`.
fn pr_refspec(origin_url: &str, number: u32) -> String {
    if origin_url.contains("gitlab") {
        format!("refs/merge-requests/{number}/head")
    } else {
        format!("refs/pull/{number}/head")
    }
}

pub(crate) struct RealGit {
    repo_dir: PathBuf,
}

impl RealGit {
    pub(crate) fn new(repo_dir: impl Into<PathBuf>) -> Self {
        Self {
            repo_dir: repo_dir.into(),
        }
    }

    /// The repository's top-level directory. Everything downstream —
    /// diff paths, file reads, editor jumps — is repo-root-relative, so
    /// the composition root re-homes here before any other git call.
    pub(crate) fn toplevel(&self) -> Result<PathBuf, GitError> {
        Ok(PathBuf::from(
            self.run_checked(&["rev-parse", "--show-toplevel"])?.trim(),
        ))
    }

    fn run(&self, args: &[&str]) -> Result<std::process::Output, GitError> {
        Ok(Command::new("git")
            .args(args)
            .current_dir(&self.repo_dir)
            .output()?)
    }

    fn run_checked(&self, args: &[&str]) -> Result<String, GitError> {
        let output = self.run(args)?;
        if !output.status.success() {
            return Err(GitError::GitFailed {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

impl GitRepo for RealGit {
    fn ref_exists(&self, ref_name: &str) -> Result<bool, GitError> {
        Ok(self
            .run(&["rev-parse", "--verify", "--quiet", ref_name])?
            .status
            .success())
    }

    fn changed_files(&self, range: &RefRange) -> Result<Vec<ChangedFile>, GitError> {
        let raw = self.run_checked(&["diff", "--name-status", "-z", &range.to_git_range()])?;
        let mut files = parse_name_status(&raw)?;
        // `git diff` never reports untracked files, but a brand-new
        // uncommitted file is exactly what a working-tree review is for.
        if range.head == Rev::WorkingTree {
            let untracked =
                self.run_checked(&["ls-files", "--others", "--exclude-standard", "-z"])?;
            files.extend(
                untracked
                    .split('\0')
                    .filter(|s| !s.is_empty())
                    .map(|path| ChangedFile {
                        path: PathBuf::from(path),
                        status: ChangeStatus::Added,
                    }),
            );
        }
        Ok(files)
    }

    fn merge_base(&self, a: &str, b: &str) -> Result<String, GitError> {
        Ok(self.run_checked(&["merge-base", a, b])?.trim().to_owned())
    }

    fn read_at(&self, rev: &Rev, path: &Path) -> Result<String, GitError> {
        match rev {
            Rev::Ref(name) => {
                let spec = format!("{}:{}", name, path.display());
                self.run_checked(&["show", &spec])
            }
            Rev::WorkingTree => std::fs::read_to_string(self.repo_dir.join(path)).map_err(|e| {
                GitError::WorktreeRead {
                    path: path.display().to_string(),
                    reason: e.to_string(),
                }
            }),
        }
    }

    fn ls_files(&self, rev: &Rev) -> Result<Vec<PathBuf>, GitError> {
        let raw = match rev {
            Rev::Ref(name) => self.run_checked(&["ls-tree", "-r", "--name-only", "-z", name])?,
            Rev::WorkingTree => self.run_checked(&[
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
            ])?,
        };
        Ok(raw
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect())
    }

    fn fetch_pr_head(&self, number: u32) -> Result<String, GitError> {
        let origin = self.run_checked(&["remote", "get-url", "origin"])?;
        let refspec = pr_refspec(origin.trim(), number);
        self.run_checked(&["fetch", "origin", &refspec])?;
        Ok("FETCH_HEAD".to_owned())
    }
}

/// Parses `git diff --name-status -z` output. With `-z`, fields are
/// NUL-separated and unquoted. For `A`/`M`/`D`/`T`: `STATUS\0PATH\0`.
/// For `R`/`C`: `R<score>\0OLD_PATH\0NEW_PATH\0` — we keep the new path
/// and treat the entry as `Modified`.
fn parse_name_status(output: &str) -> Result<Vec<ChangedFile>, GitError> {
    let mut result = Vec::new();
    let mut fields = output.split('\0').filter(|s| !s.is_empty());
    while let Some(status_field) = fields.next() {
        let first = status_field
            .chars()
            .next()
            .ok_or_else(|| GitError::UnexpectedOutput("empty status field".into()))?;

        let (status, path) = match first {
            'A' => (ChangeStatus::Added, fields.next()),
            'M' | 'T' => (ChangeStatus::Modified, fields.next()),
            'D' => (ChangeStatus::Deleted, fields.next()),
            'R' | 'C' => {
                let _old = fields.next();
                (ChangeStatus::Modified, fields.next())
            }
            _ => {
                let _ = fields.next();
                continue;
            }
        };

        let path = path.ok_or_else(|| {
            GitError::UnexpectedOutput(format!("missing path after status {status_field:?}"))
        })?;
        result.push(ChangedFile {
            path: PathBuf::from(path),
            status,
        });
    }
    Ok(result)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_added_modified_deleted() {
        let input = "M\0foo.go\0A\0bar.go\0D\0baz.go\0";
        let files = parse_name_status(input).unwrap();
        assert_eq!(
            files,
            vec![
                ChangedFile {
                    path: "foo.go".into(),
                    status: ChangeStatus::Modified
                },
                ChangedFile {
                    path: "bar.go".into(),
                    status: ChangeStatus::Added
                },
                ChangedFile {
                    path: "baz.go".into(),
                    status: ChangeStatus::Deleted
                },
            ]
        );
    }

    #[test]
    fn parses_rename_keeps_new_path_as_modified() {
        let input = "R100\0old.go\0new.go\0M\0other.go\0";
        let files = parse_name_status(input).unwrap();
        assert_eq!(
            files,
            vec![
                ChangedFile {
                    path: "new.go".into(),
                    status: ChangeStatus::Modified
                },
                ChangedFile {
                    path: "other.go".into(),
                    status: ChangeStatus::Modified
                },
            ]
        );
    }

    #[test]
    fn empty_input_yields_empty_list() {
        assert!(parse_name_status("").unwrap().is_empty());
    }

    #[test]
    fn ref_range_formats_dot_count_from_semantics() {
        let three = RefRange {
            base: "main".into(),
            head: Rev::Ref("HEAD".into()),
            mode: DiffMode::MergeBase,
        };
        assert_eq!(three.to_git_range(), "main...HEAD");
        let two = RefRange {
            base: "v1".into(),
            head: Rev::Ref("v2".into()),
            mode: DiffMode::Direct,
        };
        assert_eq!(two.to_git_range(), "v1..v2");
    }

    #[test]
    fn pr_refspec_follows_the_forge() {
        assert_eq!(
            pr_refspec("https://github.com/acme/widgets.git", 7),
            "refs/pull/7/head"
        );
        assert_eq!(
            pr_refspec("git@gitlab.com:acme/widgets.git", 7),
            "refs/merge-requests/7/head"
        );
        assert_eq!(
            pr_refspec("https://gitlab.example.com/acme/widgets.git", 12),
            "refs/merge-requests/12/head"
        );
    }

    #[test]
    fn ref_range_uses_git_worktree_grammar() {
        let merge_base = RefRange {
            base: "main".into(),
            head: Rev::WorkingTree,
            mode: DiffMode::MergeBase,
        };
        assert_eq!(merge_base.to_git_range(), "main...");
        let direct = RefRange {
            base: "main".into(),
            head: Rev::WorkingTree,
            mode: DiffMode::Direct,
        };
        assert_eq!(direct.to_git_range(), "main");
    }
}
