//! Tree-sitter-based extraction of top-level Rust items.
//!
//! Walks the root's direct children, mirroring the Go producer. Bodies and
//! fields are stripped to the declaration: function/method bodies at the
//! `body` block, struct/enum/trait bodies at their `{ … }`, and const/static
//! initialisers at the `= value`. Visibility and generics are kept as
//! written, so a `fn` gaining `pub` or a type parameter reads as a
//! modification.
//!
//! Composite items are descended one level, mirroring the Go producer:
//! struct fields, enum variants, trait members, and impl members each
//! become their own `Item` named `Parent::member` (`<Type as
//! Trait>::method` for trait impls), so a member change diffs to exactly
//! that member. Member signatures splice the qualified name into the
//! declaration text (`pub fn Client::close(&mut self)`), keeping every
//! rendered line self-contained — frontends print signatures flat.
//!
//! Deliberately not extracted: items nested in inline `mod` blocks,
//! unions, macros, and fields of individual enum variants (the variant's
//! whole text is its signature). Each is a later refinement, not a
//! silent gap.

use std::path::Path;

use tree_sitter::{Node, Parser};

use crate::item::{Item, ItemId, Kind, Line};
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
        "struct_item" => {
            push_named(node, source, path, Kind::Struct, body_cut(node), out);
            extract_struct_fields(node, source, path, out);
        }
        "enum_item" => {
            push_named(node, source, path, Kind::Enum, body_cut(node), out);
            extract_variants(node, source, path, out);
        }
        "trait_item" => {
            push_named(node, source, path, Kind::Trait, body_cut(node), out);
            extract_trait_members(node, source, path, out);
        }
        "type_item" => push_named(node, source, path, Kind::TypeAlias, None, out),
        "const_item" => push_named(node, source, path, Kind::Const, value_cut(node), out),
        "static_item" => push_named(node, source, path, Kind::Static, value_cut(node), out),
        "impl_item" => extract_impl(node, source, path, out),
        // Skipped: attributes, `use`, inline `mod`, `extern` blocks,
        // unions, and macros. None contribute top-level API shape here.
        _ => {}
    }
}

/// One `Item` per struct field. Named fields key as `Struct::field`;
/// tuple fields key positionally as `Struct::0`, `Struct::1`, …
fn extract_struct_fields(node: Node<'_>, source: &str, path: &Path, out: &mut Surface) {
    let Some(parent) = name_field(node, source) else {
        return;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    match body.kind() {
        "field_declaration_list" => {
            let mut cursor = body.walk();
            for field in body.named_children(&mut cursor) {
                if field.kind() != "field_declaration" {
                    continue;
                }
                let Some(name) = name_field(field, source) else {
                    continue;
                };
                push_member(
                    field,
                    source,
                    path,
                    Kind::Field,
                    format!("{parent}::{name}"),
                    None,
                    out,
                );
            }
        }
        "ordered_field_declaration_list" => {
            let mut cursor = body.walk();
            let mut index = 0usize;
            for child in body.named_children(&mut cursor) {
                // The list's `type` children are the fields; a
                // `visibility_modifier` sits before its type as a sibling.
                if child.kind() == "visibility_modifier" || child.kind() == "attribute_item" {
                    continue;
                }
                let ty = normalize_whitespace(&source[child.byte_range()]);
                let signature = child
                    .prev_named_sibling()
                    .filter(|p| p.kind() == "visibility_modifier")
                    .map(|p| &source[p.byte_range()])
                    .map_or_else(
                        || format!("{parent}::{index}: {ty}"),
                        |vis| format!("{vis} {parent}::{index}: {ty}"),
                    );
                out.push(Item {
                    id: ItemId {
                        path: path.to_path_buf(),
                        kind: Kind::Field,
                        name: format!("{parent}::{index}"),
                    },
                    signature,
                    line: start_line(child),
                });
                index += 1;
            }
        }
        _ => {}
    }
}

/// One `Item` per enum variant, `Enum::Variant`, with the variant's whole
/// text (including any payload) as the signature.
fn extract_variants(node: Node<'_>, source: &str, path: &Path, out: &mut Surface) {
    let Some(parent) = name_field(node, source) else {
        return;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for variant in body.named_children(&mut cursor) {
        if variant.kind() != "enum_variant" {
            continue;
        }
        let Some(name) = name_field(variant, source) else {
            continue;
        };
        push_member(
            variant,
            source,
            path,
            Kind::Variant,
            format!("{parent}::{name}"),
            None,
            out,
        );
    }
}

/// One `Item` per trait member: required and default methods
/// (`TraitMethod`), associated types (`AssocType`), and associated
/// consts (`Const`).
fn extract_trait_members(node: Node<'_>, source: &str, path: &Path, out: &mut Surface) {
    let Some(parent) = name_field(node, source) else {
        return;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        let kind = match member.kind() {
            "function_signature_item" | "function_item" => Kind::TraitMethod,
            "associated_type" => Kind::AssocType,
            "const_item" => Kind::Const,
            _ => continue,
        };
        let Some(name) = name_field(member, source) else {
            continue;
        };
        push_member(
            member,
            source,
            path,
            kind,
            format!("{parent}::{name}"),
            member_cut(member),
            out,
        );
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
        line: start_line(node),
    });
}

/// Descends one `impl` block, emitting each member: methods, associated
/// consts, and associated types. The receiver type is reduced to its base
/// identifier (so `impl Foo<T>` and `impl Foo<U>` share `Foo::method`); a
/// trait impl qualifies the name with the trait.
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
        let kind = match child.kind() {
            "function_item" => Kind::Method,
            "const_item" => Kind::Const,
            "type_item" => Kind::AssocType,
            _ => continue,
        };
        let Some(member) = name_field(child, source) else {
            continue;
        };
        let qualified = trait_name.map_or_else(
            || format!("{type_name}::{member}"),
            |tr| format!("<{type_name} as {tr}>::{member}"),
        );
        push_member(child, source, path, kind, qualified, member_cut(child), out);
    }
}

/// Pushes one member of a composite item: its id is `qualified`, and its
/// signature is the member's declaration text with `qualified` spliced
/// over the bare name, so the rendered line is self-contained.
fn push_member(
    node: Node<'_>,
    source: &str,
    path: &Path,
    kind: Kind,
    qualified: String,
    cut: Option<usize>,
    out: &mut Surface,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let signature = spliced_signature(node, source, cut, name_node, &qualified);
    out.push(Item {
        id: ItemId {
            path: path.to_path_buf(),
            kind,
            name: qualified,
        },
        signature,
        line: start_line(node),
    });
}

/// Where a member's uninteresting tail starts: a method's body block or a
/// const's initialiser. `None` for members whose whole text is shape
/// (fields, variants, associated types).
fn member_cut(node: Node<'_>) -> Option<usize> {
    body_cut(node).or_else(|| value_cut(node))
}

/// The 1-based line a node's declaration starts on.
fn start_line(node: Node<'_>) -> Line {
    Line(u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX))
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
    normalize_whitespace(&source[node.start_byte()..end])
}

/// `signature`, but with `qualified` spliced over the node's bare name so
/// a member's rendered line names its parent.
fn spliced_signature(
    node: Node<'_>,
    source: &str,
    cut: Option<usize>,
    name_node: Node<'_>,
    qualified: &str,
) -> String {
    let end = cut.unwrap_or_else(|| node.end_byte());
    let before = &source[node.start_byte()..name_node.start_byte()];
    let after = &source[name_node.end_byte()..end];
    normalize_whitespace(&format!("{before}{qualified}{after}"))
}

/// Collapses internal whitespace to single spaces and trims any trailing
/// delimiter (`;`, `=`, `{`) and whitespace.
fn normalize_whitespace(raw: &str) -> String {
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
    fn braced_struct_emits_header_then_fields() {
        let items = extract("pub struct Client<T> {\n    timeout: u64,\n    pub retries: u8,\n}\n");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id.kind, Kind::Struct);
        assert_eq!(items[0].id.name, "Client");
        assert_eq!(items[0].signature, "pub struct Client<T>");
        assert_eq!(items[1].id.kind, Kind::Field);
        assert_eq!(items[1].id.name, "Client::timeout");
        assert_eq!(items[1].signature, "Client::timeout: u64");
        assert_eq!(items[2].id.name, "Client::retries");
        assert_eq!(items[2].signature, "pub Client::retries: u8");
        assert_eq!(items[2].line.0, 3);
    }

    #[test]
    fn tuple_struct_fields_key_positionally() {
        let items = extract("pub struct Pair(pub u32, String);\n");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].signature, "pub struct Pair");
        assert_eq!(items[1].id.kind, Kind::Field);
        assert_eq!(items[1].id.name, "Pair::0");
        assert_eq!(items[1].signature, "pub Pair::0: u32");
        assert_eq!(items[2].id.name, "Pair::1");
        assert_eq!(items[2].signature, "Pair::1: String");
    }

    #[test]
    fn unit_struct_strips_semicolon() {
        let item = only("struct Unit;\n");
        assert_eq!(item.signature, "struct Unit");
    }

    #[test]
    fn enum_emits_header_then_variants() {
        let items = extract(
            "pub enum State {\n    Idle,\n    Running(u32),\n    Failed { code: i32 },\n}\n",
        );
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].id.kind, Kind::Enum);
        assert_eq!(items[0].signature, "pub enum State");
        assert_eq!(items[1].id.kind, Kind::Variant);
        assert_eq!(items[1].id.name, "State::Idle");
        assert_eq!(items[1].signature, "State::Idle");
        assert_eq!(items[2].signature, "State::Running(u32)");
        assert_eq!(items[3].signature, "State::Failed { code: i32 }");
    }

    #[test]
    fn trait_emits_header_then_members() {
        let items = extract(concat!(
            "pub trait Reader {\n",
            "    const CHUNK: usize = 8;\n",
            "    type Item;\n",
            "    fn read(&self) -> usize;\n",
            "    fn ready(&self) -> bool { true }\n",
            "}\n",
        ));
        assert_eq!(items.len(), 5);
        assert_eq!(items[0].id.kind, Kind::Trait);
        assert_eq!(items[0].signature, "pub trait Reader");
        assert_eq!(items[1].id.kind, Kind::Const);
        assert_eq!(items[1].id.name, "Reader::CHUNK");
        assert_eq!(items[1].signature, "const Reader::CHUNK: usize");
        assert_eq!(items[2].id.kind, Kind::AssocType);
        assert_eq!(items[2].signature, "type Reader::Item");
        assert_eq!(items[3].id.kind, Kind::TraitMethod);
        assert_eq!(items[3].id.name, "Reader::read");
        assert_eq!(items[3].signature, "fn Reader::read(&self) -> usize");
        // A default method is still a trait method; only its body is cut.
        assert_eq!(items[4].id.kind, Kind::TraitMethod);
        assert_eq!(items[4].signature, "fn Reader::ready(&self) -> bool");
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
            "pub fn Client::close(&mut self) -> Result<(), Error>"
        );
    }

    #[test]
    fn trait_impl_method_is_qualified_by_trait() {
        let items = extract("impl Reader for Client {\n    fn read(&self) -> usize { 0 }\n}\n");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.name, "<Client as Reader>::read");
        assert_eq!(
            items[0].signature,
            "fn <Client as Reader>::read(&self) -> usize"
        );
    }

    #[test]
    fn impl_assoc_const_and_type_are_members() {
        let items = extract(concat!(
            "impl Iterator for Counter {\n",
            "    type Item = u32;\n",
            "    const LIMIT: u32 = 5;\n",
            "    fn next(&mut self) -> Option<u32> { None }\n",
            "}\n",
        ));
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id.kind, Kind::AssocType);
        assert_eq!(items[0].id.name, "<Counter as Iterator>::Item");
        assert_eq!(items[0].signature, "type <Counter as Iterator>::Item = u32");
        assert_eq!(items[1].id.kind, Kind::Const);
        assert_eq!(
            items[1].signature,
            "const <Counter as Iterator>::LIMIT: u32"
        );
        assert_eq!(items[2].id.kind, Kind::Method);
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
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id.kind, Kind::Struct);
        assert_eq!(items[0].signature, "pub struct Foo");
        assert_eq!(items[1].id.kind, Kind::Field);
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

    #[test]
    fn items_carry_their_declaration_line() {
        let src = "pub struct S;\n\nimpl S {\n    pub fn m(&self) {}\n}\n";
        let items = extract(src);
        let lines: Vec<u32> = items.iter().map(|i| i.line.0).collect();
        // S on line 1; the method on line 4 inside the impl block.
        assert_eq!(lines, vec![1, 4]);
    }
}
