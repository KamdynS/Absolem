//! End-to-end tests of the git edge: the real binary against real
//! repositories. Everything pure is unit-tested against in-memory
//! fakes; these cover the one thing fakes cannot — that the
//! assumptions baked into the real capability implementations hold
//! against actual git. Deliberately few: coverage here mirrors where
//! boundary bugs have actually appeared.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A throwaway repository in the system temp directory, deleted on
/// drop. Git is pinned per-repo (branch name, identity, no signing)
/// and cut off from user/system config, so tests behave identically
/// on any contributor's machine and in CI.
struct ScratchRepo {
    dir: PathBuf,
}

impl ScratchRepo {
    fn new() -> Self {
        let unique = format!(
            "absolem-git-edge-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = Self { dir };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.name", "absolem-tests"]);
        repo.git(&["config", "user.email", "tests@absolem.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(&self, path: &str, content: &str) {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// Runs the built absolem binary in `subdir` of the repo, asserting
    /// success and returning stdout. The same config isolation applies:
    /// absolem shells out to git itself.
    fn absolem_in(&self, subdir: &str, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_absolem"))
            .args(args)
            .current_dir(self.dir.join(subdir))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "absolem {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn absolem(&self, args: &[&str]) -> String {
        self.absolem_in("", args)
    }
}

impl Drop for ScratchRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A repo whose feature branch adds `B` to a Go file, while main moved
/// on and added `C` to the same file after the branch point — the
/// setup that distinguishes merge-base from tip-to-tip comparison.
fn diverged_repo() -> ScratchRepo {
    let repo = ScratchRepo::new();
    repo.write("f.go", "package p\n\nfunc A() {}\n");
    repo.commit("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("f.go", "package p\n\nfunc A() {}\n\nfunc B() {}\n");
    repo.commit("feature adds B");
    repo.git(&["checkout", "-q", "main"]);
    repo.write("f.go", "package p\n\nfunc A() {}\n\nfunc C() {}\n");
    repo.commit("main adds C");
    repo.git(&["checkout", "-q", "feature"]);
    repo
}

#[test]
fn three_dot_reads_base_at_the_merge_base() {
    let repo = diverged_repo();
    // Merge-base semantics: only the branch's own change appears; the
    // C that landed on main after the branch point is not attributed.
    let out = repo.absolem(&["main", "--plain"]);
    assert_eq!(out, "f.go\n  + func B()\n");
}

#[test]
fn two_dot_diffs_the_tips_directly() {
    let repo = diverged_repo();
    let out = repo.absolem(&["main..", "--plain"]);
    assert_eq!(out, "f.go\n  + func B()\n  - func C()\n");
}

#[test]
fn worktree_review_sees_dirty_and_untracked_files() {
    let repo = ScratchRepo::new();
    repo.write("f.go", "package p\n\nfunc A() {}\n");
    repo.commit("base");
    // A dirty edit and a brand-new untracked file, neither committed.
    repo.write("f.go", "package p\n\nfunc A(x int) {}\n");
    repo.write("new.go", "package p\n\nfunc New() {}\n");
    let out = repo.absolem(&["main", "--worktree", "--plain"]);
    assert_eq!(
        out,
        "f.go\n  ~ func A(x int)\n      was: func A()\n\nnew.go\n  + func New()\n"
    );
}

#[test]
fn rename_with_a_signature_change_reviews_under_the_new_path() {
    let repo = ScratchRepo::new();
    repo.write(
        "old.go",
        "package p\n\nfunc A() {}\nfunc Pad1() {}\nfunc Pad2() {}\n",
    );
    repo.commit("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    // Rename plus a small change: git reports an R entry, and the
    // review shows the tweak — not a wall of adds and removes, and no
    // phantom deletion of the old path.
    repo.git(&["mv", "old.go", "new.go"]);
    repo.write(
        "new.go",
        "package p\n\nfunc A(x int) {}\nfunc Pad1() {}\nfunc Pad2() {}\n",
    );
    repo.commit("rename and tweak");
    let out = repo.absolem(&["main", "--plain"]);
    assert_eq!(out, "new.go\n  ~ func A(x int)\n      was: func A()\n");
}

#[test]
fn runs_identically_from_a_subdirectory() {
    let repo = ScratchRepo::new();
    repo.write("src/lib.rs", "pub fn a() {}\n");
    repo.write("src/nested/deep.rs", "pub fn d() {}\n");
    repo.commit("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("src/lib.rs", "pub fn a(x: u8) {}\n");
    repo.commit("change");
    let from_root = repo.absolem(&["main", "--plain"]);
    let from_subdir = repo.absolem_in("src/nested", &["main", "--plain"]);
    assert_eq!(from_root, from_subdir);
    assert_eq!(
        from_root,
        "src/lib.rs\n  ~ pub fn a(x: u8)\n      was: pub fn a()\n"
    );
}

#[test]
fn defaults_to_local_main_when_there_is_no_remote() {
    let repo = ScratchRepo::new();
    repo.write("f.go", "package p\n\nfunc A() {}\n");
    repo.commit("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("f.go", "package p\n\nfunc A() {}\n\nfunc B() {}\n");
    repo.commit("feature");
    // No range argument and no origin: the base falls back to local main.
    let out = repo.absolem(&["--plain"]);
    assert_eq!(out, "f.go\n  + func B()\n");
}

#[test]
fn body_only_changes_report_an_untouched_surface() {
    let repo = ScratchRepo::new();
    repo.write("f.go", "package p\n\nfunc A() int { return 1 }\n");
    repo.commit("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("f.go", "package p\n\nfunc A() int { return 2 }\n");
    repo.commit("body only");
    let out = repo.absolem(&["main", "--plain"]);
    assert_eq!(
        out,
        "No structural changes — the API surface is untouched.\n"
    );
}

#[test]
fn plain_is_the_default_when_stdout_is_piped() {
    let repo = ScratchRepo::new();
    repo.write("f.go", "package p\n\nfunc A() {}\n");
    repo.commit("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("f.go", "package p\n\nfunc A() {}\n\nfunc B() {}\n");
    repo.commit("feature");
    // No --plain: a piped stdout must never start the interactive view.
    let out = repo.absolem(&["main"]);
    assert_eq!(out, "f.go\n  + func B()\n");
}

#[test]
fn json_output_is_schema_v2_with_member_grouping() {
    let repo = ScratchRepo::new();
    repo.write(
        "client.go",
        "package p\n\ntype Client struct {\n    timeout int\n}\n",
    );
    repo.commit("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(
        "client.go",
        "package p\n\ntype Client struct {\n    timeout int\n    retries int\n}\n",
    );
    repo.commit("add a field");
    let out = repo.absolem(&["main", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["version"], 2);
    let file = &doc["files"][0];
    assert_eq!(file["path"], "client.go");
    let block = &file["items"][0];
    // The unchanged struct header carries its members: the untouched
    // field as context, the new one as added.
    assert_eq!(block["status"], "unchanged");
    assert_eq!(block["kind"], "struct");
    assert_eq!(block["members"][0]["status"], "unchanged");
    assert_eq!(block["members"][1]["status"], "added");
    assert_eq!(block["members"][1]["name"], "Client.retries");
}
