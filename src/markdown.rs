//! The markdown frontend: renders a review as forge-flavored markdown
//! for a CI pipeline comment (DESIGN §2's third frontend). Pure — takes
//! `&mut impl Write` like the other text frontends.
//!
//! Each file is a bold heading over a diff-language code fence, so
//! GitHub/GitLab color added lines green and removed lines red. A
//! modified item renders as a remove/add pair — the diff idiom readers
//! already know.

use std::io::{self, Write};

use crate::core::{Change, ChangeSet, FileChange, FileChangeKind};

pub(crate) fn render_markdown(out: &mut impl Write, review: &[FileChange]) -> io::Result<()> {
    writeln!(out, "### absolem — shape of the change")?;
    if review.is_empty() {
        writeln!(out)?;
        writeln!(out, "_No structural changes._")?;
        return Ok(());
    }
    for file in review {
        writeln!(out)?;
        match &file.kind {
            FileChangeKind::Deleted => {
                writeln!(out, "**`{}`** — deleted", file.path.display())?;
            }
            FileChangeKind::Changed(changeset) => {
                writeln!(out, "**`{}`**", file.path.display())?;
                writeln!(out)?;
                render_fence(out, changeset)?;
            }
        }
    }
    Ok(())
}

fn render_fence(out: &mut impl Write, changeset: &ChangeSet) -> io::Result<()> {
    writeln!(out, "```diff")?;
    for change in &changeset.changes {
        match change {
            Change::Added(item) => writeln!(out, "+ {}", item.signature)?,
            Change::Removed(item) => writeln!(out, "- {}", item.signature)?,
            Change::Modified { before, after } => {
                writeln!(out, "- {}", before.signature)?;
                writeln!(out, "+ {}", after.signature)?;
            }
        }
    }
    writeln!(out, "```")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::item::{Item, ItemId, Kind, Line};

    fn item(name: &str, sig: &str) -> Item {
        Item {
            id: ItemId {
                path: PathBuf::from("f.go"),
                kind: Kind::Function,
                name: name.into(),
            },
            signature: sig.into(),
            line: Line(1),
            parent: None,
            refs: Vec::new(),
        }
    }

    fn changed(path: &str, changes: Vec<Change>) -> FileChange {
        FileChange {
            path: PathBuf::from(path),
            kind: FileChangeKind::Changed(ChangeSet { changes }),
        }
    }

    fn render(review: &[FileChange]) -> String {
        let mut buf = Vec::new();
        render_markdown(&mut buf, review).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn empty_review_says_so() {
        assert_eq!(
            render(&[]),
            "### absolem — shape of the change\n\n_No structural changes._\n"
        );
    }

    #[test]
    fn changed_file_renders_a_diff_fence() {
        let review = vec![changed(
            "a.go",
            vec![
                Change::Added(item("A", "func A()")),
                Change::Modified {
                    before: item("B", "func B()"),
                    after: item("B", "func B(x int)"),
                },
                Change::Removed(item("C", "func C()")),
            ],
        )];
        assert_eq!(
            render(&review),
            concat!(
                "### absolem — shape of the change\n",
                "\n",
                "**`a.go`**\n",
                "\n",
                "```diff\n",
                "+ func A()\n",
                "- func B()\n",
                "+ func B(x int)\n",
                "- func C()\n",
                "```\n",
            )
        );
    }

    #[test]
    fn deleted_file_renders_without_a_fence() {
        let review = vec![
            changed("a.go", vec![Change::Added(item("A", "func A()"))]),
            FileChange {
                path: PathBuf::from("gone.go"),
                kind: FileChangeKind::Deleted,
            },
        ];
        assert_eq!(
            render(&review),
            concat!(
                "### absolem — shape of the change\n",
                "\n",
                "**`a.go`**\n",
                "\n",
                "```diff\n",
                "+ func A()\n",
                "```\n",
                "\n",
                "**`gone.go`** — deleted\n",
            )
        );
    }
}
