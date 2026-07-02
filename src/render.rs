//! The plain-text frontend: renders a review to a writer. Pure — takes
//! `&mut impl Write` so tests render to a `Vec<u8>` and assert on the
//! bytes, and so the composition root can point it at stdout or a pipe.
//!
//! Convention: `+` added, `-` removed, `~` modified (with the new
//! signature on the same line and the old one indented beneath),
//! no marker for unchanged context rows. Members are indented under
//! their block header; a block with members is set off as its own
//! paragraph. Files are separated by a blank line. The review is
//! pre-filtered, so every `FileChange` handed in renders.

use std::io::{self, Write};
use std::path::Path;

use crate::core::{ChangeSet, FileChange, FileChangeKind, ItemStatus, ItemView};

pub(crate) fn render_review(out: &mut impl Write, review: &[FileChange]) -> io::Result<()> {
    // An empty review is a finding, not a failure: the change touched
    // no API surface. Say so rather than printing nothing.
    if review.is_empty() {
        return writeln!(out, "No structural changes — the API surface is untouched.");
    }
    for (i, file) in review.iter().enumerate() {
        if i > 0 {
            writeln!(out)?;
        }
        match &file.kind {
            FileChangeKind::Deleted => render_deleted(out, &file.path)?,
            FileChangeKind::Changed(changeset) => render_changeset(out, &file.path, changeset)?,
        }
    }
    Ok(())
}

fn render_deleted(out: &mut impl Write, path: &Path) -> io::Result<()> {
    writeln!(out, "DELETED {}", path.display())
}

fn render_changeset(out: &mut impl Write, path: &Path, changeset: &ChangeSet) -> io::Result<()> {
    writeln!(out, "{}", path.display())?;
    let mut prev_was_composite = false;
    for block in &changeset.blocks {
        let composite = !block.members.is_empty();
        // Composites are paragraphs: air above and below, so a run of
        // leaf rows stays tight but a struct/enum block stands apart.
        if composite || prev_was_composite {
            writeln!(out)?;
        }
        render_row(out, block, 2)?;
        for member in &block.members {
            render_row(out, member, 6)?;
        }
        prev_was_composite = composite;
    }
    Ok(())
}

/// One item row at the given indent: a status marker (or none, for
/// context rows) and the signature, with a modified row's old signature
/// beneath.
fn render_row(out: &mut impl Write, view: &ItemView, indent: usize) -> io::Result<()> {
    let pad = " ".repeat(indent);
    match &view.status {
        ItemStatus::Added => writeln!(out, "{pad}+ {}", view.item.signature),
        ItemStatus::Removed => writeln!(out, "{pad}- {}", view.item.signature),
        ItemStatus::Modified { before } => {
            writeln!(out, "{pad}~ {}", view.item.signature)?;
            writeln!(out, "{pad}    was: {}", before.signature)
        }
        ItemStatus::Unchanged => writeln!(out, "{pad}  {}", view.item.signature),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::item::{Item, ItemId, Kind, Line};

    fn item(name: &str, kind: Kind, sig: &str) -> Item {
        Item {
            id: ItemId {
                path: PathBuf::from("f.go"),
                kind,
                name: name.into(),
            },
            signature: sig.into(),
            line: Line(1),
            parent: None,
            refs: Vec::new(),
        }
    }

    fn leaf(status: ItemStatus, name: &str, kind: Kind, sig: &str) -> ItemView {
        ItemView {
            status,
            item: item(name, kind, sig),
            members: Vec::new(),
        }
    }

    fn added(name: &str, sig: &str) -> ItemView {
        leaf(ItemStatus::Added, name, Kind::Function, sig)
    }

    fn changed(path: &str, blocks: Vec<ItemView>) -> FileChange {
        FileChange {
            path: PathBuf::from(path),
            kind: FileChangeKind::Changed(ChangeSet { blocks }),
        }
    }

    fn render(review: &[FileChange]) -> String {
        let mut buf = Vec::new();
        render_review(&mut buf, review).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn empty_review_says_the_surface_is_untouched() {
        assert_eq!(
            render(&[]),
            "No structural changes — the API surface is untouched.\n"
        );
    }

    #[test]
    fn deleted_prints_marker() {
        let review = vec![FileChange {
            path: PathBuf::from("foo.go"),
            kind: FileChangeKind::Deleted,
        }];
        assert_eq!(render(&review), "DELETED foo.go\n");
    }

    #[test]
    fn leaf_blocks_render_tight() {
        let review = vec![changed(
            "a.go",
            vec![
                added("A", "func A()"),
                leaf(
                    ItemStatus::Modified {
                        before: item("B", Kind::Function, "func B()"),
                    },
                    "B",
                    Kind::Function,
                    "func B(x int)",
                ),
                leaf(ItemStatus::Removed, "C", Kind::Function, "func C()"),
            ],
        )];
        assert_eq!(
            render(&review),
            concat!(
                "a.go\n",
                "  + func A()\n",
                "  ~ func B(x int)\n",
                "      was: func B()\n",
                "  - func C()\n",
            )
        );
    }

    #[test]
    fn composite_renders_whole_as_a_paragraph() {
        let members = vec![
            leaf(
                ItemStatus::Unchanged,
                "Kind::Function",
                Kind::Variant,
                "Kind::Function",
            ),
            leaf(
                ItemStatus::Added,
                "Kind::Field",
                Kind::Variant,
                "Kind::Field",
            ),
            leaf(
                ItemStatus::Removed,
                "Kind::Gone",
                Kind::Variant,
                "Kind::Gone",
            ),
        ];
        let block = ItemView {
            status: ItemStatus::Unchanged,
            item: item("Kind", Kind::Enum, "pub enum Kind"),
            members,
        };
        let review = vec![changed(
            "item.rs",
            vec![added("f", "fn f()"), block, added("g", "fn g()")],
        )];
        assert_eq!(
            render(&review),
            concat!(
                "item.rs\n",
                "  + fn f()\n",
                "\n",
                "    pub enum Kind\n",
                "        Kind::Function\n",
                "      + Kind::Field\n",
                "      - Kind::Gone\n",
                "\n",
                "  + fn g()\n",
            )
        );
    }

    #[test]
    fn files_separated_by_blank_line() {
        let review = vec![
            changed("a.go", vec![added("A", "func A()")]),
            FileChange {
                path: PathBuf::from("b.go"),
                kind: FileChangeKind::Deleted,
            },
        ];
        assert_eq!(render(&review), "a.go\n  + func A()\n\nDELETED b.go\n");
    }
}
