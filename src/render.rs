//! The plain-text frontend: renders a review to a writer. Pure — takes
//! `&mut impl Write` so tests render to a `Vec<u8>` and assert on the
//! bytes, and so the composition root can point it at stdout or a pipe.
//!
//! Convention: `+` added, `-` removed, `~` modified (with the new
//! signature on the same line and the old one indented beneath). Files
//! are separated by a blank line. The review is pre-filtered, so every
//! `FileChange` handed in renders — suppressing shapeless files is the
//! composition root's job, not the frontend's.

use std::io::{self, Write};
use std::path::Path;

use crate::core::{Change, ChangeSet, FileChange, FileChangeKind};

pub(crate) fn render_review(out: &mut impl Write, review: &[FileChange]) -> io::Result<()> {
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
    for change in &changeset.changes {
        match change {
            Change::Added(item) => writeln!(out, "  + {}", item.signature)?,
            Change::Removed(item) => writeln!(out, "  - {}", item.signature)?,
            Change::Modified { before, after } => {
                writeln!(out, "  ~ {}", after.signature)?;
                writeln!(out, "      was: {}", before.signature)?;
            }
        }
    }
    Ok(())
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

    fn changed(path: &str, changes: Vec<Change>) -> FileChange {
        FileChange {
            path: PathBuf::from(path),
            kind: FileChangeKind::Changed(ChangeSet { changes }),
        }
    }

    fn render(review: &[FileChange]) -> String {
        let mut buf = Vec::new();
        render_review(&mut buf, review).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn empty_review_renders_nothing() {
        assert_eq!(render(&[]), "");
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
    fn added_uses_plus_prefix() {
        let review = vec![changed(
            "a.go",
            vec![Change::Added(item("F", Kind::Function, "func F()"))],
        )];
        assert_eq!(render(&review), "a.go\n  + func F()\n");
    }

    #[test]
    fn removed_uses_minus_prefix() {
        let review = vec![changed(
            "a.go",
            vec![Change::Removed(item("F", Kind::Function, "func F()"))],
        )];
        assert_eq!(render(&review), "a.go\n  - func F()\n");
    }

    #[test]
    fn modified_shows_new_then_was_old() {
        let review = vec![changed(
            "a.go",
            vec![Change::Modified {
                before: item("F", Kind::Function, "func F()"),
                after: item("F", Kind::Function, "func F(x int)"),
            }],
        )];
        assert_eq!(
            render(&review),
            "a.go\n  ~ func F(x int)\n      was: func F()\n"
        );
    }

    #[test]
    fn mixed_changes_render_in_changeset_order() {
        let review = vec![changed(
            "m.go",
            vec![
                Change::Added(item("A", Kind::Function, "func A()")),
                Change::Modified {
                    before: item("B", Kind::Function, "func B()"),
                    after: item("B", Kind::Function, "func B(x int)"),
                },
                Change::Removed(item("C", Kind::Function, "func C()")),
            ],
        )];
        assert_eq!(
            render(&review),
            "m.go\n  + func A()\n  ~ func B(x int)\n      was: func B()\n  - func C()\n"
        );
    }

    #[test]
    fn files_separated_by_blank_line() {
        let review = vec![
            changed(
                "a.go",
                vec![Change::Added(item("A", Kind::Function, "func A()"))],
            ),
            FileChange {
                path: PathBuf::from("b.go"),
                kind: FileChangeKind::Deleted,
            },
        ];
        assert_eq!(render(&review), "a.go\n  + func A()\n\nDELETED b.go\n");
    }
}
