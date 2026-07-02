//! The core IR atom: an `Item` (one API entity) and its stable `ItemId`.
//!
//! `ItemId` is what makes the diff possible — it identifies "the same item"
//! across two Surfaces so the core can tell *modified* (same id, different
//! signature) from *added + removed* (different ids).
//!
//! The disambiguator is currently just a string baked into `name`:
//! plain functions, structs, interfaces, types, consts, vars use their
//! identifier; methods use `Receiver.Method` (Go) or `Type::method` /
//! `<Type as Trait>::method` (Rust). That covers every shape Go and Rust
//! permit at the top level today. Richer disambiguators can extend the
//! type later without breaking the matching contract.

use std::fmt;
use std::path::PathBuf;

/// A 1-based source line, counted the way an editor's `+N` flag counts
/// them. Carried for navigation and serialization; never part of item
/// identity, and never what makes an item read as modified.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Line(pub(crate) u32);

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A reference from an item's signature to a type it uses, as written
/// in the source (pointer/reference sugar stripped). A display name,
/// not a resolved identity: consumers resolve it by name lookup where
/// Surfaces aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeRef(pub(crate) String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    pub(crate) id: ItemId,
    pub(crate) signature: String,
    /// Where the item's declaration starts at the ref it was extracted
    /// from.
    pub(crate) line: Line,
    /// The composite this item is a member of — the *name* of the
    /// enclosing struct/interface/enum/trait (or a method's receiver
    /// type). A name, not an `ItemId`: a producer sees one file, and the
    /// parent of an `impl` member may be declared in another. Grouping
    /// resolves it against the same file's items and degrades to a flat
    /// row when nothing matches.
    pub(crate) parent: Option<String>,
    /// Types this item's signature references, in source order, deduped.
    pub(crate) refs: Vec<TypeRef>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct ItemId {
    pub(crate) path: PathBuf,
    pub(crate) kind: Kind,
    pub(crate) name: String,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) enum Kind {
    Function,
    Method,
    Struct,
    Interface,
    Type,
    TypeAlias,
    Const,
    Var,
    // Rust adds these to the shared shapes above. `Enum` and `Trait` are
    // Rust's analogues to a Go struct/interface; `Static` to a top-level
    // `var`. They stay distinct so a struct and an enum (or a const and a
    // static) of the same name never collide on identity.
    Enum,
    Trait,
    Static,
    // Members of a composite item, named `Parent.child` (Go) or
    // `Parent::child` (Rust). Extracted as their own items so a field or
    // variant change diffs to exactly that member, not a whole-type blur.
    // Distinct from `Method` so `Client.Close` the interface requirement
    // and `Client.Close` the concrete method never collide on identity.
    Field,
    InterfaceMethod,
    // Rust-side members: enum variants, trait-declared methods (required
    // or default), and associated types in traits and impls. Associated
    // consts reuse `Const` — their `Parent::NAME` id cannot collide with
    // a top-level const.
    Variant,
    TraitMethod,
    AssocType,
}

impl Kind {
    /// The kind as a `snake_case` word, for serialized output.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Interface => "interface",
            Self::Type => "type",
            Self::TypeAlias => "type_alias",
            Self::Const => "const",
            Self::Var => "var",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Static => "static",
            Self::Field => "field",
            Self::InterfaceMethod => "interface_method",
            Self::Variant => "variant",
            Self::TraitMethod => "trait_method",
            Self::AssocType => "assoc_type",
        }
    }
}
