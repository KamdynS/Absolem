//! The core IR atom: an `Item` (one API entity) and its stable `ItemId`.
//!
//! `ItemId` is what makes the diff possible — it identifies "the same item"
//! across two Surfaces so the core can tell *modified* (same id, different
//! signature) from *added + removed* (different ids).
//!
//! The disambiguator is currently just a string baked into `name`:
//! plain functions, structs, interfaces, types, consts, vars use their
//! identifier; methods use `Receiver.Method`. That covers every shape Go
//! permits at the top level today. Richer disambiguators can extend the
//! type later without breaking the matching contract.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    pub(crate) id: ItemId,
    pub(crate) signature: String,
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
}
