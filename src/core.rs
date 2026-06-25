//! Pure diff between two Surfaces.
//!
//! Matching is by `ItemId`: same id → compare signatures (modified if
//! they differ, omitted if identical); id in head but not base → added;
//! id in base but not head → removed.
//!
//! Output order is deterministic and aimed at readability: head-order
//! drives adds/modifies (so they appear roughly where the reviewer
//! expects in the new file), then base-order drives removals at the
//! end. No IO, no language knowledge — this layer never imports
//! tree-sitter.

use std::path::PathBuf;

use crate::item::Item;
use crate::surface::Surface;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    Added(Item),
    Removed(Item),
    Modified { before: Item, after: Item },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ChangeSet {
    pub(crate) changes: Vec<Change>,
}

impl ChangeSet {
    pub(crate) const fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// One file's contribution to a review: either it was deleted outright, or
/// its shape changed in a way worth showing. Built by the composition root
/// and consumed by frontends — the multi-file aggregate that pairs a
/// `ChangeSet` with the path it came from. Files whose API shape did not
/// move are dropped before this stage, so every `FileChange` renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileChange {
    pub(crate) path: PathBuf,
    pub(crate) kind: FileChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileChangeKind {
    Deleted,
    Changed(ChangeSet),
}

pub(crate) fn diff(base: &Surface, head: &Surface) -> ChangeSet {
    let base_by_id = base.by_id();
    let head_by_id = head.by_id();
    let mut changes = Vec::new();

    for item in head.iter() {
        match base_by_id.get(&item.id) {
            None => changes.push(Change::Added(item.clone())),
            Some(prev) if prev.signature != item.signature => changes.push(Change::Modified {
                before: (*prev).clone(),
                after: item.clone(),
            }),
            Some(_) => {}
        }
    }

    for item in base.iter() {
        if !head_by_id.contains_key(&item.id) {
            changes.push(Change::Removed(item.clone()));
        }
    }

    ChangeSet { changes }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::item::{ItemId, Kind};

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

    fn surface(items: Vec<Item>) -> Surface {
        let mut s = Surface::new();
        for i in items {
            s.push(i);
        }
        s
    }

    #[test]
    fn empty_vs_empty_yields_no_changes() {
        assert!(diff(&Surface::new(), &Surface::new()).is_empty());
    }

    #[test]
    fn added_item_in_head_only() {
        let base = Surface::new();
        let head = surface(vec![item("A", Kind::Function, "func A()")]);
        let cs = diff(&base, &head);
        assert_eq!(cs.changes.len(), 1);
        assert!(matches!(&cs.changes[0], Change::Added(i) if i.id.name == "A"));
    }

    #[test]
    fn removed_item_in_base_only() {
        let base = surface(vec![item("A", Kind::Function, "func A()")]);
        let head = Surface::new();
        let cs = diff(&base, &head);
        assert_eq!(cs.changes.len(), 1);
        assert!(matches!(&cs.changes[0], Change::Removed(i) if i.id.name == "A"));
    }

    #[test]
    fn identical_signatures_are_unchanged() {
        let s = surface(vec![item("A", Kind::Function, "func A()")]);
        assert!(diff(&s, &s).is_empty());
    }

    #[test]
    fn same_id_different_signature_is_modified() {
        let base = surface(vec![item("A", Kind::Function, "func A()")]);
        let head = surface(vec![item("A", Kind::Function, "func A(x int)")]);
        let cs = diff(&base, &head);
        assert_eq!(cs.changes.len(), 1);
        match &cs.changes[0] {
            Change::Modified { before, after } => {
                assert_eq!(before.signature, "func A()");
                assert_eq!(after.signature, "func A(x int)");
            }
            other => panic!("expected Modified, got {other:?}"),
        }
    }

    #[test]
    fn different_kind_with_same_name_is_add_plus_remove() {
        let base = surface(vec![item("Foo", Kind::Struct, "type Foo struct")]);
        let head = surface(vec![item("Foo", Kind::Function, "func Foo()")]);
        let cs = diff(&base, &head);
        assert_eq!(cs.changes.len(), 2);
        assert!(matches!(&cs.changes[0], Change::Added(i) if i.id.kind == Kind::Function));
        assert!(matches!(&cs.changes[1], Change::Removed(i) if i.id.kind == Kind::Struct));
    }

    #[test]
    fn head_order_drives_adds_and_modifies() {
        let base = surface(vec![item("B", Kind::Function, "func B()")]);
        let head = surface(vec![
            item("A", Kind::Function, "func A()"),
            item("B", Kind::Function, "func B(x int)"),
            item("C", Kind::Function, "func C()"),
        ]);
        let cs = diff(&base, &head);
        let names: Vec<_> = cs
            .changes
            .iter()
            .map(|c| match c {
                Change::Added(i) | Change::Removed(i) => i.id.name.clone(),
                Change::Modified { after, .. } => after.id.name.clone(),
            })
            .collect();
        assert_eq!(names, vec!["A", "B", "C"]);
    }

    #[test]
    fn removals_appear_after_adds_and_modifies() {
        let base = surface(vec![
            item("Old1", Kind::Function, "func Old1()"),
            item("Keep", Kind::Function, "func Keep()"),
            item("Old2", Kind::Function, "func Old2()"),
        ]);
        let head = surface(vec![
            item("New", Kind::Function, "func New()"),
            item("Keep", Kind::Function, "func Keep(x int)"),
        ]);
        let cs = diff(&base, &head);
        let kinds: Vec<_> = cs
            .changes
            .iter()
            .map(|c| match c {
                Change::Added(_) => "added",
                Change::Modified { .. } => "modified",
                Change::Removed(_) => "removed",
            })
            .collect();
        assert_eq!(kinds, vec!["added", "modified", "removed", "removed"]);
    }

    #[test]
    fn path_is_part_of_identity() {
        let a = Item {
            id: ItemId {
                path: PathBuf::from("a.go"),
                kind: Kind::Function,
                name: "F".into(),
            },
            signature: "func F()".into(),
        };
        let b = Item {
            id: ItemId {
                path: PathBuf::from("b.go"),
                kind: Kind::Function,
                name: "F".into(),
            },
            signature: "func F()".into(),
        };
        let cs = diff(&surface(vec![a]), &surface(vec![b]));
        assert_eq!(cs.changes.len(), 2);
    }
}
