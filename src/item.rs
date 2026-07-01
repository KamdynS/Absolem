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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    pub(crate) id: ItemId,
    pub(crate) signature: String,
    /// Where the item's declaration starts at the ref it was extracted
    /// from.
    pub(crate) line: Line,
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
}
