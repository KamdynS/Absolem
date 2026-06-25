//! Tree-sitter-based extraction of top-level Rust items.
//!
//! Walks the root's direct children, mirroring the Go producer. Bodies and
//! fields are stripped to the declaration: function/method bodies at the
//! `body` block, struct/enum/trait bodies at their `{ … }`, and const/static
//! initialisers at the `= value`. Visibility and generics are kept as
//! written, so a `fn` gaining `pub` or a type parameter reads as a
//! modification.
//!
//! Methods live in `impl` blocks, so those are descended one level: each
//! `fn` becomes a `Method` keyed by its type — `Type::method` for inherent
//! impls, `<Type as Trait>::method` for trait impls, so a type's inherent
//! and trait methods of the same name never collide on identity.
//!
//! Deliberately not extracted (parity with the Go producer's flat,
//! shape-only view): items nested in inline `mod` blocks, struct/enum field
//! and variant shapes, trait method signatures, associated consts/types,
//! unions, and macros. Each is a later refinement, not a silent gap.

use std::path::Path;

use tree_sitter::{Node, Parser};

use crate::item::{Item, ItemId, Kind};
use crate::producer::{Producer, ProducerError};
use crate::surface::Surface;

pub(crate) struct RustProducer {
    parser: Parser,
}

impl RustProducer {
    pub(crate) fn new() -> Result<Self, ProducerError> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
        Ok(Self { parser })
    }
}

impl Producer for RustProducer {
    fn extract(&mut self, path: &Path, source: &str) -> Result<Surface, ProducerError> {
        let tree = self
            .parser
            .parse(source, None)
            .ok_or(ProducerError::NoTree)?;
        let root = tree.root_node();
        let mut surface = Surface::new();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            extract_into(child, source, path, &mut surface);
        }
        Ok(surface)
    }
}

fn extract_into(node: Node<'_>, source: &str, path: &Path, out: &mut Surface) {
    match node.kind() {
        "function_item" => push_named(node, source, path, Kind::Function, body_cut(node), out),
        "struct_item" => push_named(node, source, path, Kind::Struct, body_cut(node), out),
        "enum_item" => push_named(node, source, path, Kind::Enum, body_cut(node), out),
        "trait_item" => push_named(node, source, path, Kind::Trait, body_cut(node), out),
        "type_item" => push_named(node, source, path, Kind::TypeAlias, None, out),
        "const_item" => push_named(node, source, path, Kind::Const, value_cut(node), out),
        "static_item" => push_named(node, source, path, Kind::Static, value_cut(node), out),
        "impl_item" => extract_impl(node, source, path, out),
        // Skipped: attributes, `use`, inline `mod`, `extern` blocks,
        // unions, and macros. None contribute top-level API shape here.
        _ => {}
    }
}

/// Pushes one item whose identifier is in the `name` field. `cut` is the
/// byte where the body/initialiser begins; everything from there on is
/// dropped from the signature.
fn push_named(
    node: Node<'_>,
    source: &str,
    path: &Path,
    kind: Kind,
    cut: Option<usize>,
    out: &mut Surface,
) {
    let Some(name) = name_field(node, source) else {
        return;
    };
    out.push(Item {
        id: ItemId {
            path: path.to_path_buf(),
            kind,
            name: name.into(),
        },
        signature: signature(node, source, cut),
    });
}

/// Descends one `impl` block, emitting each method. The receiver type is
/// reduced to its base identifier (so `impl Foo<T>` and `impl Foo<U>` share
/// `Foo::method`); a trait impl qualifies the name with the trait.
fn extract_impl(node: Node<'_>, source: &str, path: &Path, out: &mut Surface) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let Some(type_name) = first_type_identifier(type_node, source) else {
        return;
    };
    let trait_name = node
        .child_by_field_name("trait")
        .and_then(|t| first_type_identifier(t, source));

    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() != "function_item" {
            continue;
        }
        let Some(method) = name_field(child, source) else {
            continue;
        };
        let name = trait_name.map_or_else(
            || format!("{type_name}::{method}"),
            |tr| format!("<{type_name} as {tr}>::{method}"),
        );
        out.push(Item {
            id: ItemId {
                path: path.to_path_buf(),
                kind: Kind::Method,
                name,
            },
            signature: signature(child, source, body_cut(child)),
        });
    }
}

fn name_field<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    node.child_by_field_name("name")
        .map(|n| &source[n.byte_range()])
}

fn body_cut(node: Node<'_>) -> Option<usize> {
    node.child_by_field_name("body").map(|b| b.start_byte())
}

fn value_cut(node: Node<'_>) -> Option<usize> {
    node.child_by_field_name("value").map(|v| v.start_byte())
}

/// The declaration text from the item's start up to `cut` (or the node end),
/// with internal whitespace collapsed to single spaces and any trailing
/// delimiter (`;`, `=`, `{`) and whitespace trimmed.
fn signature(node: Node<'_>, source: &str, cut: Option<usize>) -> String {
    let end = cut.unwrap_or_else(|| node.end_byte());
    let raw = &source[node.start_byte()..end];
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .trim_end_matches(|c: char| c == ';' || c == '=' || c == '{' || c.is_whitespace())
        .to_owned()
}

/// First `type_identifier` in a pre-order walk of a type node. Reduces
/// `Foo`, `Foo<T>`, `&mut Foo`, `Box<Foo>` all to `Foo` — the base type a
/// method hangs off.
fn first_type_identifier<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    if node.kind() == "type_identifier" {
        return Some(&source[node.byte_range()]);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = first_type_identifier(child, source) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::producer::Producer;

    fn extract(source: &str) -> Vec<Item> {
        let mut p = RustProducer::new().unwrap();
        let s = p.extract(&PathBuf::from("f.rs"), source).unwrap();
        s.iter().cloned().collect()
    }

    fn only(source: &str) -> Item {
        let items = extract(source);
        assert_eq!(items.len(), 1, "expected exactly one item: {items:?}");
        items.into_iter().next().unwrap()
    }

    #[test]
    fn function_strips_body_keeps_pub_and_generics() {
        let item = only("pub fn connect(addr: &str) -> Client<String> {\n    todo!()\n}\n");
        assert_eq!(item.id.kind, Kind::Function);
        assert_eq!(item.id.name, "connect");
        assert_eq!(
            item.signature,
            "pub fn connect(addr: &str) -> Client<String>"
        );
    }

    #[test]
    fn private_function_has_no_pub() {
        let item = only("fn helper() {}\n");
        assert_eq!(item.signature, "fn helper()");
    }

    #[test]
    fn multiline_signature_is_collapsed() {
        let item = only("pub fn f(\n    a: i32,\n    b: i32,\n) -> i32 {\n    a\n}\n");
        assert_eq!(item.signature, "pub fn f( a: i32, b: i32, ) -> i32");
    }

    #[test]
    fn braced_struct_drops_fields() {
        let item = only("pub struct Client<T> {\n    timeout: u64,\n}\n");
        assert_eq!(item.id.kind, Kind::Struct);
        assert_eq!(item.id.name, "Client");
        assert_eq!(item.signature, "pub struct Client<T>");
    }

    #[test]
    fn tuple_struct_drops_fields() {
        let item = only("pub struct Pair(u32, String);\n");
        assert_eq!(item.id.kind, Kind::Struct);
        assert_eq!(item.signature, "pub struct Pair");
    }

    #[test]
    fn unit_struct_strips_semicolon() {
        let item = only("struct Unit;\n");
        assert_eq!(item.signature, "struct Unit");
    }

    #[test]
    fn enum_drops_variants() {
        let item = only("pub enum State {\n    Idle,\n    Running(u32),\n}\n");
        assert_eq!(item.id.kind, Kind::Enum);
        assert_eq!(item.id.name, "State");
        assert_eq!(item.signature, "pub enum State");
    }

    #[test]
    fn trait_drops_body() {
        let item = only("pub trait Reader {\n    fn read(&self) -> usize;\n}\n");
        assert_eq!(item.id.kind, Kind::Trait);
        assert_eq!(item.id.name, "Reader");
        assert_eq!(item.signature, "pub trait Reader");
    }

    #[test]
    fn type_alias_keeps_rhs() {
        let item = only("pub type Name = String;\n");
        assert_eq!(item.id.kind, Kind::TypeAlias);
        assert_eq!(item.signature, "pub type Name = String");
    }

    #[test]
    fn const_drops_value_keeps_type() {
        let item = only("pub const MAX: u32 = 100;\n");
        assert_eq!(item.id.kind, Kind::Const);
        assert_eq!(item.id.name, "MAX");
        assert_eq!(item.signature, "pub const MAX: u32");
    }

    #[test]
    fn static_keeps_mut_and_type() {
        let item = only("pub static mut COUNT: i64 = 0;\n");
        assert_eq!(item.id.kind, Kind::Static);
        assert_eq!(item.id.name, "COUNT");
        assert_eq!(item.signature, "pub static mut COUNT: i64");
    }

    #[test]
    fn inherent_method_keyed_by_type() {
        let items = extract(
            "impl Client<String> {\n    pub fn close(&mut self) -> Result<(), Error> { Ok(()) }\n}\n",
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.kind, Kind::Method);
        assert_eq!(items[0].id.name, "Client::close");
        assert_eq!(
            items[0].signature,
            "pub fn close(&mut self) -> Result<(), Error>"
        );
    }

    #[test]
    fn trait_impl_method_is_qualified_by_trait() {
        let items = extract("impl Reader for Client {\n    fn read(&self) -> usize { 0 }\n}\n");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.name, "<Client as Reader>::read");
        assert_eq!(items[0].signature, "fn read(&self) -> usize");
    }

    #[test]
    fn inherent_and_trait_methods_have_distinct_ids() {
        let items = extract(concat!(
            "impl Client {\n    pub fn read(&self) -> usize { 0 }\n}\n",
            "impl Reader for Client {\n    fn read(&self) -> usize { 0 }\n}\n",
        ));
        assert_eq!(items.len(), 2);
        assert_ne!(items[0].id, items[1].id);
        assert_eq!(items[0].id.name, "Client::read");
        assert_eq!(items[1].id.name, "<Client as Reader>::read");
    }

    #[test]
    fn inline_module_contents_are_not_extracted() {
        let items = extract("pub fn outer() {}\nmod inner {\n    pub fn hidden() {}\n}\n");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.name, "outer");
    }

    #[test]
    fn attributes_and_use_are_skipped() {
        let items = extract("use std::io;\n#[derive(Debug)]\npub struct Foo {\n    x: u8,\n}\n");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.kind, Kind::Struct);
        assert_eq!(items[0].signature, "pub struct Foo");
    }

    #[test]
    fn mixed_file_preserves_source_order() {
        let items = extract(concat!(
            "pub struct Client;\n",
            "pub fn connect() -> Client { Client }\n",
            "impl Client {\n    pub fn close(self) {}\n}\n",
        ));
        let kinds: Vec<_> = items.iter().map(|i| i.id.kind).collect();
        assert_eq!(kinds, vec![Kind::Struct, Kind::Function, Kind::Method]);
    }

    #[test]
    fn empty_file_yields_empty_surface() {
        assert!(extract("\n").is_empty());
    }
}
