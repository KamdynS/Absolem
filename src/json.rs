//! The JSON frontend: serializes a review for machine consumers such
//! as editor plugins. Pure — takes `&mut impl Write` like the
//! plain-text frontend.
//!
//! The IR types stay serde-free; this frontend maps them to
//! `serde_json::Value` by hand so the pure core keeps zero dependencies.
//! The document carries a `version` so consumers can detect shape drift.
//! Schema v2: files carry `items` — blocks with a `status` (including
//! `unchanged` context members), nested `members`, and the `refs` a
//! signature mentions.

use std::io::{self, Write};

use serde_json::{Value, json};

use crate::core::{FileChange, FileChangeKind, ItemStatus, ItemView};

/// Bump when the emitted shape changes incompatibly.
const SCHEMA_VERSION: u32 = 2;

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
            "items": [],
        }),
        FileChangeKind::Changed(changeset) => {
            let items: Vec<Value> = changeset.blocks.iter().map(view_json).collect();
            json!({
                "path": file.path.display().to_string(),
                "status": "changed",
                "items": items,
            })
        }
    }
}

/// The item's path is omitted: it always equals the enclosing file
/// entry's. A modified item carries its base-side signature and line
/// under `before`.
fn view_json(view: &ItemView) -> Value {
    let item = &view.item;
    let refs: Vec<&str> = item.refs.iter().map(|r| r.0.as_str()).collect();
    let members: Vec<Value> = view.members.iter().map(view_json).collect();
    let mut value = json!({
        "status": view.status.as_str(),
        "kind": item.id.kind.as_str(),
        "name": item.id.name,
        "signature": item.signature,
        "line": item.line.0,
        "refs": refs,
        "members": members,
    });
    if let ItemStatus::Modified { before } = &view.status
        && let Some(map) = value.as_object_mut()
    {
        map.insert(
            "before".to_owned(),
            json!({ "signature": before.signature, "line": before.line.0 }),
        );
    }
    value
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::core::ChangeSet;
    use crate::item::{Item, ItemId, Kind, Line, TypeRef};

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
        assert_eq!(render(&[]), json!({ "version": 2, "files": [] }));
    }

    #[test]
    fn output_ends_with_a_newline() {
        let mut buf = Vec::new();
        render_json(&mut buf, &[]).unwrap();
        assert_eq!(buf.last(), Some(&b'\n'));
    }

    #[test]
    fn blocks_nest_members_with_statuses_and_refs() {
        let mut func = item("F", Kind::Function, "func F() *Client", 3);
        func.refs = vec![TypeRef("Client".into())];
        let block = ItemView {
            status: ItemStatus::Unchanged,
            item: item("Kind", Kind::Enum, "pub enum Kind", 10),
            members: vec![ItemView {
                status: ItemStatus::Modified {
                    before: item("Kind::A", Kind::Variant, "Kind::A", 11),
                },
                item: item("Kind::A", Kind::Variant, "Kind::A(u8)", 12),
                members: Vec::new(),
            }],
        };
        let review = vec![FileChange {
            path: PathBuf::from("f.go"),
            kind: FileChangeKind::Changed(ChangeSet {
                blocks: vec![
                    ItemView {
                        status: ItemStatus::Added,
                        item: func,
                        members: Vec::new(),
                    },
                    block,
                ],
            }),
        }];
        assert_eq!(
            render(&review),
            json!({
                "version": 2,
                "files": [{
                    "path": "f.go",
                    "status": "changed",
                    "items": [
                        {
                            "status": "added",
                            "kind": "function",
                            "name": "F",
                            "signature": "func F() *Client",
                            "line": 3,
                            "refs": ["Client"],
                            "members": [],
                        },
                        {
                            "status": "unchanged",
                            "kind": "enum",
                            "name": "Kind",
                            "signature": "pub enum Kind",
                            "line": 10,
                            "refs": [],
                            "members": [{
                                "status": "modified",
                                "kind": "variant",
                                "name": "Kind::A",
                                "signature": "Kind::A(u8)",
                                "line": 12,
                                "refs": [],
                                "members": [],
                                "before": { "signature": "Kind::A", "line": 11 },
                            }],
                        },
                    ],
                }],
            })
        );
    }

    #[test]
    fn deleted_file_has_deleted_status_and_no_items() {
        let review = vec![FileChange {
            path: PathBuf::from("gone.rs"),
            kind: FileChangeKind::Deleted,
        }];
        assert_eq!(
            render(&review),
            json!({
                "version": 2,
                "files": [{ "path": "gone.rs", "status": "deleted", "items": [] }],
            })
        );
    }
}
