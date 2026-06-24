//! The `GitRepo` capability and its real shell-out-to-`git` implementation.

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

    /// Files changed in the three-dot range `base...head` (i.e. against
    /// the merge-base, matching what GitHub/GitLab show on a PR).
    fn changed_files(&self, base: &str, head: &str) -> Result<Vec<ChangedFile>, GitError>;

    /// Read the contents of `path` at revision `rev`.
    fn read_at(&self, rev: &str, path: &Path) -> Result<String, GitError>;
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

    fn changed_files(&self, base: &str, head: &str) -> Result<Vec<ChangedFile>, GitError> {
        let range = format!("{base}...{head}");
        let raw = self.run_checked(&["diff", "--name-status", "-z", &range])?;
        parse_name_status(&raw)
    }

    fn read_at(&self, rev: &str, path: &Path) -> Result<String, GitError> {
        let spec = format!("{}:{}", rev, path.display());
        self.run_checked(&["show", &spec])
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
}
