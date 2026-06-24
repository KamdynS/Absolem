//! A top-level declaration extracted from a source file.
//!
//! v1 deliberately keeps this minimal: just the rendered signature and a
//! kind. `Span`, `ItemId`, resolution, and the rest of the design-doc IR
//! arrive when the next feature (Surface diffing between two refs) forces
//! them to exist.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decl {
    pub(crate) signature: String,
    pub(crate) kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Function,
    Method,
    Struct,
    Interface,
    TypeAlias,
    Const,
    Var,
}
