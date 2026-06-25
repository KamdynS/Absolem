//! Renders a per-file `ChangeSet` to a writer. Pure: takes
//! `&mut impl Write` so tests render to a `Vec<u8>` and assert on the
//! bytes.
//!
//! Convention: `+` added, `-` removed, `~` modified (with the new
//! signature on the same line and the old one indented beneath). Files
//! with no API-shape changes do not render at all — suppressing them is
//! the whole point of the shape view.

use std::io::{self, Write};
use std::path::Path;

use crate::core::{Change, ChangeSet};

pub(crate) fn render_blank(out: &mut impl Write) -> io::Result<()> {
    writeln!(out)
}

pub(crate) fn render_deleted(out: &mut impl Write, path: &Path) -> io::Result<()> {
    writeln!(out, "DELETED {}", path.display())
}

pub(crate) fn render_changeset(
    out: &mut impl Write,
    path: &Path,
    changeset: &ChangeSet,
) -> io::Result<()> {
    if changeset.is_empty() {
        return Ok(());
    }
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
    use crate::item::{Item, ItemId, Kind};

    fn item(name: &str, kind: Kind, sig: &str) -> Item {
        Item {
            id: ItemId {
                path: PathBuf::from("f.go"),
                kind,
                name: name.into(),
            },
            signature: sig.into(),
        }
    }

    fn capture(f: impl FnOnce(&mut Vec<u8>) -> io::Result<()>) -> String {
        let mut buf = Vec::new();
        f(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn deleted_prints_marker() {
        let s = capture(|out| render_deleted(out, &PathBuf::from("foo.go")));
        assert_eq!(s, "DELETED foo.go\n");
    }

    #[test]
    fn empty_changeset_renders_nothing() {
        let cs = ChangeSet::default();
        let s = capture(|out| render_changeset(out, &PathBuf::from("foo.go"), &cs));
        assert_eq!(s, "");
    }

    #[test]
    fn added_uses_plus_prefix() {
        let cs = ChangeSet {
            changes: vec![Change::Added(item("F", Kind::Function, "func F()"))],
        };
        let s = capture(|out| render_changeset(out, &PathBuf::from("a.go"), &cs));
        assert_eq!(s, "a.go\n  + func F()\n");
    }

    #[test]
    fn removed_uses_minus_prefix() {
        let cs = ChangeSet {
            changes: vec![Change::Removed(item("F", Kind::Function, "func F()"))],
        };
        let s = capture(|out| render_changeset(out, &PathBuf::from("a.go"), &cs));
        assert_eq!(s, "a.go\n  - func F()\n");
    }

    #[test]
    fn modified_shows_new_then_was_old() {
        let cs = ChangeSet {
            changes: vec![Change::Modified {
                before: item("F", Kind::Function, "func F()"),
                after: item("F", Kind::Function, "func F(x int)"),
            }],
        };
        let s = capture(|out| render_changeset(out, &PathBuf::from("a.go"), &cs));
        assert_eq!(s, "a.go\n  ~ func F(x int)\n      was: func F()\n");
    }

    #[test]
    fn mixed_changes_render_in_changeset_order() {
        let cs = ChangeSet {
            changes: vec![
                Change::Added(item("A", Kind::Function, "func A()")),
                Change::Modified {
                    before: item("B", Kind::Function, "func B()"),
                    after: item("B", Kind::Function, "func B(x int)"),
                },
                Change::Removed(item("C", Kind::Function, "func C()")),
            ],
        };
        let s = capture(|out| render_changeset(out, &PathBuf::from("m.go"), &cs));
        assert_eq!(
            s,
            "m.go\n  + func A()\n  ~ func B(x int)\n      was: func B()\n  - func C()\n"
        );
    }
}
