//! `absolem` entrypoint and composition root.

mod core;
mod git;
mod item;
mod producer;
mod render;
mod surface;

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use clap::Parser;

use crate::core::diff;
use crate::git::{ChangeStatus, ChangedFile, GitRepo, RealGit};
use crate::producer::go::{GoError, GoProducer};
use crate::surface::Surface;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Surface the structural shape of a change before review."
)]
struct Cli {}

fn main() -> Result<()> {
    let _cli = Cli::parse();

    let repo_dir = std::env::current_dir().context("failed to read current working directory")?;
    let git = RealGit::new(repo_dir);

    let base = resolve_base(&git)?;
    let changed = git
        .changed_files(&base, "HEAD")
        .with_context(|| format!("git diff {base}...HEAD failed"))?;

    let go_files: Vec<_> = changed
        .into_iter()
        .filter(|f| f.path.extension().is_some_and(|e| e == "go"))
        .collect();

    let mut producer = GoProducer::new().context("failed to initialize Go parser")?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut first_rendered = true;

    for file in &go_files {
        render_file_diff(
            &git,
            &mut producer,
            &base,
            file,
            &mut out,
            &mut first_rendered,
        )?;
    }

    Ok(())
}

fn render_file_diff(
    git: &impl GitRepo,
    producer: &mut GoProducer,
    base: &str,
    file: &ChangedFile,
    out: &mut impl std::io::Write,
    first_rendered: &mut bool,
) -> Result<()> {
    let (base_surface, head_surface) = match file.status {
        ChangeStatus::Added => (
            Surface::new(),
            surface_at(git, producer, "HEAD", &file.path)?,
        ),
        ChangeStatus::Deleted => (surface_at(git, producer, base, &file.path)?, Surface::new()),
        ChangeStatus::Modified => (
            surface_at(git, producer, base, &file.path)?,
            surface_at(git, producer, "HEAD", &file.path)?,
        ),
    };

    let changeset = diff(&base_surface, &head_surface);
    if file.status == ChangeStatus::Deleted {
        if !*first_rendered {
            render::render_blank(out)?;
        }
        render::render_deleted(out, &file.path)?;
        *first_rendered = false;
    } else if !changeset.is_empty() {
        if !*first_rendered {
            render::render_blank(out)?;
        }
        render::render_changeset(out, &file.path, &changeset)?;
        *first_rendered = false;
    }
    Ok(())
}

fn surface_at(
    git: &impl GitRepo,
    producer: &mut GoProducer,
    rev: &str,
    path: &Path,
) -> Result<Surface> {
    let source = git
        .read_at(rev, path)
        .with_context(|| format!("git show {rev}:{} failed", path.display()))?;
    producer
        .extract(path, &source)
        .map_err(|e: GoError| anyhow!(e))
        .with_context(|| format!("failed to parse {}@{rev}", path.display()))
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

        fn changed_files(&self, _base: &str, _head: &str) -> Result<Vec<ChangedFile>, GitError> {
            Ok(self.files.clone())
        }

        fn read_at(&self, rev: &str, path: &Path) -> Result<String, GitError> {
            let key = format!("{}:{}", rev, path.display());
            self.contents
                .get(&key)
                .cloned()
                .ok_or_else(|| GitError::UnexpectedOutput(format!("no content for {key}")))
        }
    }

    fn render_all(git: &FakeGit) -> String {
        let mut producer = GoProducer::new().unwrap();
        let mut buf: Vec<u8> = Vec::new();
        let mut first = true;
        for file in &git.files {
            render_file_diff(
                git,
                &mut producer,
                "origin/main",
                file,
                &mut buf,
                &mut first,
            )
            .unwrap();
        }
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
