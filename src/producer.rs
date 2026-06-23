//! Language producers: turn source text into a `Vec<Decl>`.
//!
//! v1 ships one — Go via tree-sitter — and there is no `Producer` trait
//! yet. A trait around a single impl is speculative architecture; it
//! arrives when a second language forces the seam.

pub(crate) mod go;
