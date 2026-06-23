//! `absolem` entrypoint and composition root.
//!
//! Per `docs/STYLE.md` §3.1.4, real capabilities are constructed only
//! here; everything else takes them by `impl Trait`.

mod decl;
mod git;
mod producer;
mod render;

use anyhow::{Context, Result, anyhow};
use clap::Parser;

use crate::git::{ChangeStatus, GitRepo, RealGit};
use crate::producer::go::GoProducer;

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

    for (i, file) in go_files.iter().enumerate() {
        if i > 0 {
            render::render_blank(&mut out)?;
        }
        match file.status {
            ChangeStatus::Deleted => render::render_deleted(&mut out, &file.path)?,
            ChangeStatus::Added | ChangeStatus::Modified => {
                let source = git
                    .read_at("HEAD", &file.path)
                    .with_context(|| format!("git show HEAD:{} failed", file.path.display()))?;
                let decls = producer
                    .parse(&source)
                    .with_context(|| format!("failed to parse {}", file.path.display()))?;
                render::render_file(&mut out, &file.path, &decls)?;
            }
        }
    }

    Ok(())
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
    use std::path::Path;

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
}
