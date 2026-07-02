//! The markdown frontend: renders a review as forge-flavored markdown
//! for a CI pipeline comment. Pure — takes `&mut impl Write` like the
//! other text frontends.
//!
//! Each file is a bold heading over a diff-language code fence, so
//! GitHub/GitLab color added lines green and removed lines red. A
//! modified item renders as a remove/add pair — the diff idiom readers
//! already know — and unchanged context rows carry the leading space of
//! a diff context line. Members are indented under their block header.

use std::io::{self, Write};

use crate::core::{ChangeSet, FileChange, FileChangeKind, ItemStatus, ItemView};

pub(crate) fn render_markdown(out: &mut impl Write, review: &[FileChange]) -> io::Result<()> {
    writeln!(out, "### absolem — shape of the change")?;
    if review.is_empty() {
        writeln!(out)?;
        writeln!(
            out,
            "_No structural changes — the API surface is untouched._"
        )?;
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
    let mut prev_was_composite = false;
    for block in &changeset.blocks {
        let composite = !block.members.is_empty();
        if composite || prev_was_composite {
            writeln!(out)?;
        }
        render_row(out, block, "")?;
        for member in &block.members {
            render_row(out, member, "  ")?;
        }
        prev_was_composite = composite;
    }
    writeln!(out, "```")
}

/// One row inside the fence: the diff marker column (`+`, `-`, or the
/// context space), a space, the member indent, and the signature.
fn render_row(out: &mut impl Write, view: &ItemView, indent: &str) -> io::Result<()> {
    match &view.status {
        ItemStatus::Added => writeln!(out, "+ {indent}{}", view.item.signature),
        ItemStatus::Removed => writeln!(out, "- {indent}{}", view.item.signature),
        ItemStatus::Modified { before } => {
            writeln!(out, "- {indent}{}", before.signature)?;
            writeln!(out, "+ {indent}{}", view.item.signature)
        }
        ItemStatus::Unchanged => writeln!(out, "  {indent}{}", view.item.signature),
    }
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

    fn leaf(status: ItemStatus, name: &str, sig: &str) -> ItemView {
        ItemView {
            status,
            item: item(name, sig),
            members: Vec::new(),
        }
    }

    fn changed(path: &str, blocks: Vec<ItemView>) -> FileChange {
        FileChange {
            path: PathBuf::from(path),
            kind: FileChangeKind::Changed(ChangeSet { blocks }),
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
            "### absolem — shape of the change\n\n_No structural changes — the API surface is untouched._\n"
        );
    }

    #[test]
    fn leaf_changes_render_as_diff_lines() {
        let review = vec![changed(
            "a.go",
            vec![
                leaf(ItemStatus::Added, "A", "func A()"),
                leaf(
                    ItemStatus::Modified {
                        before: item("B", "func B()"),
                    },
                    "B",
                    "func B(x int)",
                ),
                leaf(ItemStatus::Removed, "C", "func C()"),
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
    fn composite_renders_with_context_rows() {
        let block = ItemView {
            status: ItemStatus::Unchanged,
            item: item("Kind", "pub enum Kind"),
            members: vec![
                leaf(ItemStatus::Unchanged, "Kind::Function", "Kind::Function"),
                leaf(ItemStatus::Added, "Kind::Field", "Kind::Field"),
            ],
        };
        let review = vec![changed(
            "item.rs",
            vec![leaf(ItemStatus::Added, "f", "fn f()"), block],
        )];
        assert_eq!(
            render(&review),
            concat!(
                "### absolem — shape of the change\n",
                "\n",
                "**`item.rs`**\n",
                "\n",
                "```diff\n",
                "+ fn f()\n",
                "\n",
                "  pub enum Kind\n",
                "    Kind::Function\n",
                "+   Kind::Field\n",
                "```\n",
            )
        );
    }

    #[test]
    fn deleted_file_renders_without_a_fence() {
        let review = vec![
            changed("a.go", vec![leaf(ItemStatus::Added, "A", "func A()")]),
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
