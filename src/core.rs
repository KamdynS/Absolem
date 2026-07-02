//! Pure diff between two Surfaces, grouped the way a reviewer reads.
//!
//! Matching is by `ItemId`: same id → compare signatures (modified if
//! they differ); id in head but not base → added; id in base but not
//! head → removed. Items are then grouped into *blocks*: a top-level
//! item plus all of its members. A block whose composite changed
//! anywhere carries **all** of its members — unchanged ones as context —
//! so a frontend can show the entire enum/struct/interface with the
//! changes in place, not orphaned member rows.
//!
//! Output order is deterministic: head order drives the blocks (and the
//! members within them), removed members trail their block, and removed
//! top-level items trail the file. A removed composite folds its
//! members into the one removal row. Blocks with no change anywhere are
//! dropped. No IO, no language knowledge — this layer never imports
//! tree-sitter.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::item::Item;
use crate::surface::Surface;

/// How one item moved between the two Surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ItemStatus {
    Added,
    Removed,
    Modified {
        before: Item,
    },
    /// Identical on both sides — carried so composites render whole.
    Unchanged,
}

impl ItemStatus {
    pub(crate) const fn is_changed(&self) -> bool {
        !matches!(self, Self::Unchanged)
    }

    /// The status as a lowercase word, for serialized output.
    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Modified { .. } => "modified",
            Self::Unchanged => "unchanged",
        }
    }
}

/// One rendered block: an item and its members (members never nest
/// further today).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ItemView {
    pub(crate) status: ItemStatus,
    pub(crate) item: Item,
    pub(crate) members: Vec<Self>,
}

impl ItemView {
    const fn leaf(status: ItemStatus, item: Item) -> Self {
        Self {
            status,
            item,
            members: Vec::new(),
        }
    }

    pub(crate) fn is_changed(&self) -> bool {
        self.status.is_changed() || self.members.iter().any(|m| m.status.is_changed())
    }
}

/// The diff of two Surfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ChangeSet {
    pub(crate) blocks: Vec<ItemView>,
}

impl ChangeSet {
    pub(crate) const fn is_empty(&self) -> bool {
        self.blocks.is_empty()
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

/// A resolved type reference: the definition chosen, and how contested
/// its name is. `candidates` of one means the name is unambiguous;
/// anything higher means resolution had to guess and frontends should
/// say so.
#[derive(Debug)]
pub(crate) struct Resolved<'a> {
    pub(crate) def: &'a ItemView,
    pub(crate) candidates: usize,
}

/// A head-wide index of item definitions by name, for resolving
/// `TypeRef`s: exact name match, with a fallback to the final segment
/// of a qualified name (`pkg.Client` → `Client`). Members attach to
/// their parent's entry across files, so an `impl` in another file
/// lands on the type it extends. Every definition of a colliding name
/// is kept; resolution prefers one in the referencing item's own file,
/// then falls back to the first seen.
#[derive(Debug, Default)]
pub(crate) struct TypeIndex {
    by_name: HashMap<String, Vec<ItemView>>,
    /// The inverse edge: type name → every item whose signature
    /// references it. Keyed by the final segment of the reference as
    /// written, matching how `lookup` normalizes.
    users: HashMap<String, Vec<ItemView>>,
}

impl TypeIndex {
    pub(crate) fn build(surfaces: &[Surface]) -> Self {
        let mut by_name: HashMap<String, Vec<ItemView>> = HashMap::new();
        for surface in surfaces {
            for item in surface.iter().filter(|i| i.parent.is_none()) {
                by_name
                    .entry(item.id.name.clone())
                    .or_default()
                    .push(ItemView::leaf(ItemStatus::Unchanged, item.clone()));
            }
        }
        for surface in surfaces {
            for item in surface.iter() {
                let Some(parent) = &item.parent else {
                    continue;
                };
                let Some(defs) = by_name.get_mut(parent) else {
                    continue;
                };
                // A member joins the definition in its own file when one
                // exists — an impl block next to its type — and the
                // first definition otherwise.
                let at = defs
                    .iter()
                    .position(|d| d.item.id.path == item.id.path)
                    .unwrap_or(0);
                defs[at]
                    .members
                    .push(ItemView::leaf(ItemStatus::Unchanged, item.clone()));
            }
        }
        let mut users: HashMap<String, Vec<ItemView>> = HashMap::new();
        for surface in surfaces {
            for item in surface.iter() {
                for reference in &item.refs {
                    let name = final_segment(&reference.0);
                    users
                        .entry(name.to_owned())
                        .or_default()
                        .push(ItemView::leaf(ItemStatus::Unchanged, item.clone()));
                }
            }
        }
        Self { by_name, users }
    }

    /// Every item whose signature references `name` — the reverse of
    /// `lookup`. Name-based, like everything at this tier: two types
    /// sharing a name share a user list.
    pub(crate) fn users_of(&self, name: &str) -> &[ItemView] {
        self.users.get(name).map_or(&[], Vec::as_slice)
    }

    /// Resolves `name` as seen from `from` (the referencing item's
    /// file): a definition in the same file wins over one elsewhere.
    pub(crate) fn lookup(&self, name: &str, from: &Path) -> Option<Resolved<'_>> {
        let defs = self.candidates(name)?;
        let def = defs
            .iter()
            .find(|d| d.item.id.path == from)
            .unwrap_or(defs.first()?);
        Some(Resolved {
            def,
            candidates: defs.len(),
        })
    }

    fn candidates(&self, name: &str) -> Option<&Vec<ItemView>> {
        self.by_name.get(name).or_else(|| {
            let last = final_segment(name);
            (last != name).then(|| self.by_name.get(last))?
        })
    }
}

/// `pkg.Client` → `Client`, `Foo::Bar` → `Bar`; a bare name unchanged.
fn final_segment(name: &str) -> &str {
    name.rsplit(['.', ':']).next().unwrap_or(name)
}

pub(crate) fn diff(base: &Surface, head: &Surface) -> ChangeSet {
    let base_by_id = base.by_id();
    let head_by_id = head.by_id();

    let status_of = |item: &Item| match base_by_id.get(&item.id) {
        None => ItemStatus::Added,
        Some(prev) if prev.signature != item.signature => ItemStatus::Modified {
            before: (*prev).clone(),
        },
        Some(_) => ItemStatus::Unchanged,
    };

    // Pass 1: a block per head top-level item, members attached to the
    // block whose name their `parent` names. A member whose parent isn't
    // in the file (an impl for a type declared elsewhere, or a member
    // seen before its parent) degrades to its own block.
    let mut blocks: Vec<ItemView> = Vec::new();
    let mut block_by_name: HashMap<&str, usize> = HashMap::new();
    for item in head.iter() {
        let attached = item
            .parent
            .as_ref()
            .and_then(|p| block_by_name.get(p.as_str()).copied());
        if let Some(index) = attached {
            blocks[index]
                .members
                .push(ItemView::leaf(status_of(item), item.clone()));
        } else {
            if item.parent.is_none() {
                block_by_name.insert(item.id.name.as_str(), blocks.len());
            }
            blocks.push(ItemView::leaf(status_of(item), item.clone()));
        }
    }

    // Pass 2: removed items from base. Members trail their block; a
    // member whose parent was itself removed folds into the parent's
    // removal row. Removed top-level items trail the file.
    let removed_parents: HashSet<&str> = base
        .iter()
        .filter(|i| i.parent.is_none() && !head_by_id.contains_key(&i.id))
        .map(|i| i.id.name.as_str())
        .collect();
    let mut removed_tops: Vec<ItemView> = Vec::new();
    for item in base.iter() {
        if head_by_id.contains_key(&item.id) {
            continue;
        }
        let view = ItemView::leaf(ItemStatus::Removed, item.clone());
        match item.parent.as_deref() {
            Some(parent) if removed_parents.contains(parent) => {}
            Some(parent) => match block_by_name.get(parent) {
                Some(&index) => blocks[index].members.push(view),
                None => removed_tops.push(view),
            },
            None => removed_tops.push(view),
        }
    }
    blocks.extend(removed_tops);

    blocks.retain(ItemView::is_changed);
    ChangeSet { blocks }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::item::{ItemId, Kind, Line};

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

    fn member(parent: &str, name: &str, kind: Kind, sig: &str) -> Item {
        let mut i = item(name, kind, sig);
        i.parent = Some(parent.into());
        i
    }

    fn surface(items: Vec<Item>) -> Surface {
        let mut s = Surface::new();
        for i in items {
            s.push(i);
        }
        s
    }

    fn statuses(cs: &ChangeSet) -> Vec<(&str, &'static str)> {
        fn tag(s: &ItemStatus) -> &'static str {
            match s {
                ItemStatus::Added => "added",
                ItemStatus::Removed => "removed",
                ItemStatus::Modified { .. } => "modified",
                ItemStatus::Unchanged => "unchanged",
            }
        }
        cs.blocks
            .iter()
            .map(|b| (b.item.id.name.as_str(), tag(&b.status)))
            .collect()
    }

    #[test]
    fn empty_vs_empty_yields_no_changes() {
        assert!(diff(&Surface::new(), &Surface::new()).is_empty());
    }

    #[test]
    fn added_item_in_head_only() {
        let head = surface(vec![item("A", Kind::Function, "func A()")]);
        let cs = diff(&Surface::new(), &head);
        assert_eq!(statuses(&cs), vec![("A", "added")]);
    }

    #[test]
    fn removed_item_in_base_only() {
        let base = surface(vec![item("A", Kind::Function, "func A()")]);
        let cs = diff(&base, &Surface::new());
        assert_eq!(statuses(&cs), vec![("A", "removed")]);
    }

    #[test]
    fn identical_signatures_yield_no_blocks() {
        let s = surface(vec![item("A", Kind::Function, "func A()")]);
        assert!(diff(&s, &s).is_empty());
    }

    #[test]
    fn same_id_different_signature_is_modified() {
        let base = surface(vec![item("A", Kind::Function, "func A()")]);
        let head = surface(vec![item("A", Kind::Function, "func A(x int)")]);
        let cs = diff(&base, &head);
        match &cs.blocks[0].status {
            ItemStatus::Modified { before } => assert_eq!(before.signature, "func A()"),
            other => panic!("expected Modified, got {other:?}"),
        }
    }

    #[test]
    fn different_kind_with_same_name_is_add_plus_remove() {
        let base = surface(vec![item("Foo", Kind::Struct, "type Foo struct")]);
        let head = surface(vec![item("Foo", Kind::Function, "func Foo()")]);
        let cs = diff(&base, &head);
        assert_eq!(statuses(&cs), vec![("Foo", "added"), ("Foo", "removed")]);
    }

    #[test]
    fn head_order_drives_blocks_removals_last() {
        let base = surface(vec![
            item("Old", Kind::Function, "func Old()"),
            item("B", Kind::Function, "func B()"),
        ]);
        let head = surface(vec![
            item("A", Kind::Function, "func A()"),
            item("B", Kind::Function, "func B(x int)"),
        ]);
        let cs = diff(&base, &head);
        assert_eq!(
            statuses(&cs),
            vec![("A", "added"), ("B", "modified"), ("Old", "removed")]
        );
    }

    #[test]
    fn changed_member_pulls_whole_block_with_context() {
        let base = surface(vec![
            item("Kind", Kind::Enum, "pub enum Kind"),
            member("Kind", "Kind::Function", Kind::Variant, "Kind::Function"),
        ]);
        let head = surface(vec![
            item("Kind", Kind::Enum, "pub enum Kind"),
            member("Kind", "Kind::Function", Kind::Variant, "Kind::Function"),
            member("Kind", "Kind::Field", Kind::Variant, "Kind::Field"),
        ]);
        let cs = diff(&base, &head);
        assert_eq!(cs.blocks.len(), 1);
        let block = &cs.blocks[0];
        // The unchanged parent is kept as the block header…
        assert_eq!(block.status, ItemStatus::Unchanged);
        assert_eq!(block.item.id.name, "Kind");
        // …and the unchanged sibling variant is kept as context.
        assert_eq!(block.members.len(), 2);
        assert_eq!(block.members[0].status, ItemStatus::Unchanged);
        assert_eq!(block.members[0].item.id.name, "Kind::Function");
        assert_eq!(block.members[1].status, ItemStatus::Added);
        assert_eq!(block.members[1].item.id.name, "Kind::Field");
    }

    #[test]
    fn fully_unchanged_block_is_dropped() {
        let s = surface(vec![
            item("Kind", Kind::Enum, "pub enum Kind"),
            member("Kind", "Kind::Function", Kind::Variant, "Kind::Function"),
            item("F", Kind::Function, "fn F()"),
        ]);
        let mut head_items: Vec<Item> = s.iter().cloned().collect();
        head_items[2].signature = "fn F(x: u8)".into();
        let cs = diff(&s, &surface(head_items));
        // Only the function block survives; the untouched enum is gone.
        assert_eq!(statuses(&cs), vec![("F", "modified")]);
    }

    #[test]
    fn removed_member_trails_its_block() {
        let base = surface(vec![
            item("S", Kind::Struct, "struct S"),
            member("S", "S::gone", Kind::Field, "S::gone: u8"),
            member("S", "S::kept", Kind::Field, "S::kept: u8"),
        ]);
        let head = surface(vec![
            item("S", Kind::Struct, "struct S"),
            member("S", "S::kept", Kind::Field, "S::kept: u8"),
        ]);
        let cs = diff(&base, &head);
        assert_eq!(cs.blocks.len(), 1);
        let names: Vec<_> = cs.blocks[0]
            .members
            .iter()
            .map(|m| m.item.id.name.as_str())
            .collect();
        assert_eq!(names, vec!["S::kept", "S::gone"]);
        assert_eq!(cs.blocks[0].members[1].status, ItemStatus::Removed);
    }

    #[test]
    fn removed_composite_folds_its_members() {
        let base = surface(vec![
            item("S", Kind::Struct, "struct S"),
            member("S", "S::x", Kind::Field, "S::x: u8"),
        ]);
        let cs = diff(&base, &Surface::new());
        // One removal row for the struct; the field is folded in.
        assert_eq!(statuses(&cs), vec![("S", "removed")]);
        assert!(cs.blocks[0].members.is_empty());
    }

    #[test]
    fn orphan_member_degrades_to_its_own_block() {
        // An impl member for a type declared in another file.
        let head = surface(vec![member(
            "Elsewhere",
            "Elsewhere::m",
            Kind::Method,
            "fn Elsewhere::m()",
        )]);
        let cs = diff(&Surface::new(), &head);
        assert_eq!(statuses(&cs), vec![("Elsewhere::m", "added")]);
    }

    #[test]
    fn moving_an_item_without_changing_its_signature_is_not_a_change() {
        let base = surface(vec![item("A", Kind::Function, "func A()")]);
        let mut moved = item("A", Kind::Function, "func A()");
        moved.line = Line(40);
        let head = surface(vec![moved]);
        assert!(diff(&base, &head).is_empty());
    }

    #[test]
    fn type_index_resolves_names_and_attaches_cross_file_members() {
        let mut client = item("Client", Kind::Struct, "type Client struct");
        client.id.path = PathBuf::from("client.go");
        let mut field = member(
            "Client",
            "Client.timeout",
            Kind::Field,
            "Client.timeout int",
        );
        field.id.path = PathBuf::from("client.go");
        // A method on Client declared in a different file.
        let mut method = member(
            "Client",
            "Client.Close",
            Kind::Method,
            "func (c *Client) Close() error",
        );
        method.id.path = PathBuf::from("client_ext.go");

        let index = TypeIndex::build(&[surface(vec![client, field]), surface(vec![method])]);
        let from = PathBuf::from("elsewhere.go");
        let resolved = index.lookup("Client", &from).unwrap();
        assert_eq!(resolved.def.item.id.name, "Client");
        assert_eq!(resolved.candidates, 1);
        let members: Vec<_> = resolved
            .def
            .members
            .iter()
            .map(|m| m.item.id.name.as_str())
            .collect();
        assert_eq!(members, vec!["Client.timeout", "Client.Close"]);
        // Qualified names fall back to their final segment.
        assert!(index.lookup("pkg.Client", &from).is_some());
        assert!(index.lookup("Missing", &from).is_none());
    }

    #[test]
    fn users_of_inverts_references() {
        let client = item("Client", Kind::Struct, "type Client struct");
        let mut connect = item("Connect", Kind::Function, "func Connect() *Client");
        connect.refs = vec![crate::item::TypeRef("Client".into())];
        let mut field = member("Pool", "Pool.client", Kind::Field, "Pool.client sdk.Client");
        field.refs = vec![crate::item::TypeRef("sdk.Client".into())];
        let index = TypeIndex::build(&[surface(vec![client, connect, field])]);

        let users: Vec<_> = index
            .users_of("Client")
            .iter()
            .map(|u| u.item.id.name.as_str())
            .collect();
        // Qualified references count by their final segment.
        assert_eq!(users, vec!["Connect", "Pool.client"]);
        assert!(index.users_of("Nobody").is_empty());
    }

    #[test]
    fn colliding_names_prefer_the_referencing_file() {
        let mut a = item("Config", Kind::Struct, "type Config struct");
        a.id.path = PathBuf::from("server/config.go");
        let mut b = item("Config", Kind::Struct, "type Config struct");
        b.id.path = PathBuf::from("client/config.go");
        let index = TypeIndex::build(&[surface(vec![a]), surface(vec![b])]);

        let near = index
            .lookup("Config", Path::new("client/config.go"))
            .unwrap();
        assert_eq!(near.def.item.id.path, PathBuf::from("client/config.go"));
        assert_eq!(near.candidates, 2);

        // From an unrelated file the pick is arbitrary but the contest
        // is reported.
        let far = index.lookup("Config", Path::new("other.go")).unwrap();
        assert_eq!(far.candidates, 2);
    }

    #[test]
    fn path_is_part_of_identity() {
        let a = item("F", Kind::Function, "func F()");
        let mut b = item("F", Kind::Function, "func F()");
        b.id.path = PathBuf::from("b.go");
        let cs = diff(&surface(vec![a]), &surface(vec![b]));
        assert_eq!(cs.blocks.len(), 2);
    }
}
