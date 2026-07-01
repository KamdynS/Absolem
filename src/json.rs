//! The JSON frontend: serializes a review for machine consumers — the
//! seam the Neovim plugin will read the IR over (DESIGN §2). Pure —
//! takes `&mut impl Write` like the plain-text frontend.
//!
//! The IR types stay serde-free; this frontend maps them to
//! `serde_json::Value` by hand so the pure core keeps zero dependencies.
//! The document carries a `version` so consumers can detect shape drift.

use std::io::{self, Write};

use serde_json::{Value, json};

use crate::core::{Change, FileChange, FileChangeKind};
use crate::item::{Item, Kind};

/// Bump when the emitted shape changes incompatibly.
const SCHEMA_VERSION: u32 = 1;

pub(crate) fn render_json(out: &mut impl Write, review: &[FileChange]) -> io::Result<()> {
    let files: Vec<Value> = review.iter().map(file_json).collect();
    let doc = json!({
        "version": SCHEMA_VERSION,
        "files": files,
    });
    serde_json::to_writer(&mut *out, &doc).map_err(io::Error::other)?;
    writeln!(out)
}

fn file_json(file: &FileChange) -> Value {
    match &file.kind {
        FileChangeKind::Deleted => json!({
            "path": file.path.display().to_string(),
            "status": "deleted",
            "changes": [],
        }),
        FileChangeKind::Changed(changeset) => {
            let changes: Vec<Value> = changeset.changes.iter().map(change_json).collect();
            json!({
                "path": file.path.display().to_string(),
                "status": "changed",
                "changes": changes,
            })
        }
    }
}

fn change_json(change: &Change) -> Value {
    match change {
        Change::Added(item) => json!({ "change": "added", "item": item_json(item) }),
        Change::Removed(item) => json!({ "change": "removed", "item": item_json(item) }),
        Change::Modified { before, after } => json!({
            "change": "modified",
            "before": item_json(before),
            "after": item_json(after),
        }),
    }
}

/// The item's path is omitted: it always equals the enclosing file entry's.
fn item_json(item: &Item) -> Value {
    json!({
        "kind": kind_str(item.id.kind),
        "name": item.id.name,
        "signature": item.signature,
        "line": item.line.0,
    })
}

const fn kind_str(kind: Kind) -> &'static str {
    match kind {
        Kind::Function => "function",
        Kind::Method => "method",
        Kind::Struct => "struct",
        Kind::Interface => "interface",
        Kind::Type => "type",
        Kind::TypeAlias => "type_alias",
        Kind::Const => "const",
        Kind::Var => "var",
        Kind::Enum => "enum",
        Kind::Trait => "trait",
        Kind::Static => "static",
        Kind::Field => "field",
        Kind::InterfaceMethod => "interface_method",
        Kind::Variant => "variant",
        Kind::TraitMethod => "trait_method",
        Kind::AssocType => "assoc_type",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::core::ChangeSet;
    use crate::item::{ItemId, Line};

    fn item(name: &str, kind: Kind, sig: &str, line: u32) -> Item {
        Item {
            id: ItemId {
                path: PathBuf::from("f.go"),
                kind,
                name: name.into(),
            },
            signature: sig.into(),
            line: Line(line),
            parent: None,
            refs: Vec::new(),
        }
    }

    fn render(review: &[FileChange]) -> Value {
        let mut buf = Vec::new();
        render_json(&mut buf, review).unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    #[test]
    fn empty_review_is_versioned_with_no_files() {
        assert_eq!(render(&[]), json!({ "version": 1, "files": [] }));
    }

    #[test]
    fn output_ends_with_a_newline() {
        let mut buf = Vec::new();
        render_json(&mut buf, &[]).unwrap();
        assert_eq!(buf.last(), Some(&b'\n'));
    }

    #[test]
    fn changes_carry_kind_name_signature_and_line() {
        let review = vec![FileChange {
            path: PathBuf::from("f.go"),
            kind: FileChangeKind::Changed(ChangeSet {
                changes: vec![
                    Change::Added(item("F", Kind::Function, "func F()", 3)),
                    Change::Modified {
                        before: item("Client.timeout", Kind::Field, "Client.timeout int", 8),
                        after: item("Client.timeout", Kind::Field, "Client.timeout int64", 9),
                    },
                    Change::Removed(item("Gone", Kind::Struct, "type Gone struct", 20)),
                ],
            }),
        }];
        assert_eq!(
            render(&review),
            json!({
                "version": 1,
                "files": [{
                    "path": "f.go",
                    "status": "changed",
                    "changes": [
                        {
                            "change": "added",
                            "item": {"kind": "function", "name": "F", "signature": "func F()", "line": 3},
                        },
                        {
                            "change": "modified",
                            "before": {"kind": "field", "name": "Client.timeout", "signature": "Client.timeout int", "line": 8},
                            "after": {"kind": "field", "name": "Client.timeout", "signature": "Client.timeout int64", "line": 9},
                        },
                        {
                            "change": "removed",
                            "item": {"kind": "struct", "name": "Gone", "signature": "type Gone struct", "line": 20},
                        },
                    ],
                }],
            })
        );
    }

    #[test]
    fn deleted_file_has_deleted_status_and_no_changes() {
        let review = vec![FileChange {
            path: PathBuf::from("gone.rs"),
            kind: FileChangeKind::Deleted,
        }];
        assert_eq!(
            render(&review),
            json!({
                "version": 1,
                "files": [{ "path": "gone.rs", "status": "deleted", "changes": [] }],
            })
        );
    }
}
