//! Tree-sitter-based extraction of top-level Go items.
//!
//! Walks the root node's direct children. For each item:
//! function/method bodies are stripped at the `body` field; struct and
//! interface type bodies are stripped to `type <name> struct` /
//! `type <name> interface`; aliases and `type Foo Bar` definitions are
//! rendered as-is. `const` and `var` blocks are split per-spec so each
//! identifier becomes its own `Item` with a stable id; this is what
//! lets the diff distinguish "added a const" from "modified a const"
//! when the file is reordered.

use std::path::Path;

use tree_sitter::{Node, Parser};

use crate::item::{Item, ItemId, Kind, Line, TypeRef};
use crate::producer::{Producer, ProducerError};
use crate::surface::Surface;

/// Go's predeclared identifiers — never worth a `TypeRef`.
const GO_PREDECLARED: &[&str] = &[
    "any",
    "bool",
    "byte",
    "comparable",
    "complex64",
    "complex128",
    "error",
    "float32",
    "float64",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "rune",
    "string",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
];

/// Collects the type names a declaration references — `type_identifier`
/// and whole `qualified_type` nodes — in source order, deduped, skipping
/// predeclared identifiers, `exclude`d names (the item's own type), and
/// everything at or past `cut` (the body).
fn collect_refs(
    node: Node<'_>,
    source: &str,
    cut: Option<usize>,
    exclude: &[&str],
) -> Vec<TypeRef> {
    let mut out: Vec<TypeRef> = Vec::new();
    let mut push = |text: &str| {
        if GO_PREDECLARED.contains(&text) || exclude.contains(&text) {
            return;
        }
        if out.iter().all(|r| r.0 != text) {
            out.push(TypeRef(text.to_owned()));
        }
    };
    collect_refs_into(node, source, cut, &mut push);
    out
}

fn collect_refs_into(
    node: Node<'_>,
    source: &str,
    cut: Option<usize>,
    push: &mut impl FnMut(&str),
) {
    if cut.is_some_and(|c| node.start_byte() >= c) {
        return;
    }
    match node.kind() {
        // A qualified type (`io.Reader`) is kept whole, not descended.
        "qualified_type" | "type_identifier" => {
            push(&source[node.byte_range()]);
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_refs_into(child, source, cut, push);
    }
}

pub(crate) struct GoProducer {
    parser: Parser,
}

impl GoProducer {
    pub(crate) fn new() -> Result<Self, ProducerError> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_go::LANGUAGE.into())?;
        Ok(Self { parser })
    }
}

impl Producer for GoProducer {
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
    // Node kinds are strings defined by the vendored grammar; the
    // tree-sitter bindings generate no Rust enum for them, so string
    // matching is the supported dispatch form.
    match node.kind() {
        "function_declaration" => {
            if let Some(name) = name_field(node, source) {
                out.push(Item {
                    id: ItemId {
                        path: path.to_path_buf(),
                        kind: Kind::Function,
                        name: name.into(),
                    },
                    signature: signature_without_body(node, source),
                    line: start_line(node),
                    parent: None,
                    refs: collect_refs(node, source, body_start(node), &[]),
                });
            }
        }
        "method_declaration" => {
            let receiver = node
                .child_by_field_name("receiver")
                .and_then(|r| receiver_type_name(r, source));
            if let (Some(method), Some(receiver)) = (name_field(node, source), receiver) {
                out.push(Item {
                    id: ItemId {
                        path: path.to_path_buf(),
                        kind: Kind::Method,
                        name: format!("{receiver}.{method}"),
                    },
                    signature: signature_without_body(node, source),
                    line: start_line(node),
                    parent: Some(receiver.to_owned()),
                    // The receiver type is the parent, not a reference.
                    refs: collect_refs(node, source, body_start(node), &[receiver]),
                });
            }
        }
        "type_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(child.kind(), "type_spec" | "type_alias") {
                    emit_type_spec(child, source, path, out);
                }
            }
        }
        "const_declaration" => emit_value_specs(node, source, path, Kind::Const, "const", out),
        "var_declaration" => emit_value_specs(node, source, path, Kind::Var, "var", out),
        // Skipped: `package_clause`, `import_declaration`, `comment`,
        // and `ERROR` nodes. None contribute API shape.
        _ => {}
    }
}

/// The 1-based line a node's declaration starts on.
fn start_line(node: Node<'_>) -> Line {
    Line(u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX))
}

fn name_field<'a>(node: Node<'a>, source: &'a str) -> Option<&'a str> {
    node.child_by_field_name("name")
        .map(|n| &source[n.byte_range()])
}

/// Where a declaration's body starts, if it has one — the cut point for
/// signatures and reference collection.
fn body_start(node: Node<'_>) -> Option<usize> {
    node.child_by_field_name("body").map(|b| b.start_byte())
}

/// Method ids are `Receiver.Method`; the receiver is the type name with
/// `*` stripped so `func (c *Client) X()` and `func (c Client) X()`
/// collide as one id. Go's rules forbid both existing simultaneously,
/// so this is safe.
fn receiver_type_name<'a>(receiver: Node<'a>, source: &'a str) -> Option<&'a str> {
    let mut cursor = receiver.walk();
    for param in receiver.children(&mut cursor) {
        if param.kind() != "parameter_declaration" {
            continue;
        }
        let ty = param.child_by_field_name("type")?;
        return Some(unwrap_pointer(ty, source));
    }
    None
}

fn unwrap_pointer<'a>(ty: Node<'a>, source: &'a str) -> &'a str {
    if ty.kind() == "pointer_type"
        && let Some(inner) = ty.named_child(0)
    {
        return &source[inner.byte_range()];
    }
    &source[ty.byte_range()]
}

fn signature_without_body(node: Node<'_>, source: &str) -> String {
    let end = body_start(node).unwrap_or_else(|| node.end_byte());
    source[node.start_byte()..end].trim().to_owned()
}

/// Emits the item for one `type_spec` / `type_alias`, then — for structs
/// and interfaces — one item per member, so a member change diffs to
/// exactly that member.
fn emit_type_spec(spec: Node<'_>, source: &str, path: &Path, out: &mut Surface) {
    let Some(name_node) = spec.child_by_field_name("name") else {
        return;
    };
    let name = &source[name_node.byte_range()];
    let Some(ty_node) = spec.child_by_field_name("type") else {
        return;
    };
    let is_alias = spec.kind() == "type_alias";
    let (kind, signature, refs) = match ty_node.kind() {
        // Headers of composites carry no refs; their members do.
        "struct_type" => (Kind::Struct, format!("type {name} struct"), Vec::new()),
        "interface_type" => (
            Kind::Interface,
            format!("type {name} interface"),
            Vec::new(),
        ),
        _ => {
            let ty = &source[ty_node.byte_range()];
            let signature = if is_alias {
                format!("type {name} = {ty}")
            } else {
                format!("type {name} {ty}")
            };
            let kind = if is_alias {
                Kind::TypeAlias
            } else {
                Kind::Type
            };
            (
                kind,
                signature,
                collect_refs(ty_node, source, None, &[name]),
            )
        }
    };
    out.push(Item {
        id: ItemId {
            path: path.to_path_buf(),
            kind,
            name: name.into(),
        },
        signature,
        line: start_line(spec),
        parent: None,
        refs,
    });
    match ty_node.kind() {
        "struct_type" => emit_struct_fields(ty_node, source, path, name, out),
        "interface_type" => emit_interface_members(ty_node, source, path, name, out),
        _ => {}
    }
}

/// One `Item` per `field_declaration`, named `Struct.field`. Multi-name
/// fields (`a, b int`) collapse to one item keyed by the first name,
/// like multi-name const/var specs. Embedded fields are keyed by their
/// type text (pointer stripped) and read `Struct embeds Type`.
fn emit_struct_fields(
    struct_ty: Node<'_>,
    source: &str,
    path: &Path,
    parent: &str,
    out: &mut Surface,
) {
    let Some(list) = struct_ty.named_child(0) else {
        return;
    };
    let mut cursor = list.walk();
    for field in list.named_children(&mut cursor) {
        if field.kind() != "field_declaration" {
            continue;
        }
        let mut name_cursor = field.walk();
        let mut names = field.children_by_field_name("name", &mut name_cursor);
        let (name, signature) = if let Some(first) = names.next() {
            let field_text = normalize_whitespace(&source[field.byte_range()]);
            (
                source[first.byte_range()].to_owned(),
                format!("{parent}.{field_text}"),
            )
        } else {
            // No name field: an embedded type.
            let Some(ty) = field.child_by_field_name("type") else {
                continue;
            };
            let ty_text = normalize_whitespace(unwrap_pointer(ty, source));
            (ty_text.clone(), format!("{parent} embeds {ty_text}"))
        };
        out.push(Item {
            id: ItemId {
                path: path.to_path_buf(),
                kind: Kind::Field,
                name: format!("{parent}.{name}"),
            },
            signature,
            line: start_line(field),
            parent: Some(parent.to_owned()),
            refs: collect_refs(field, source, None, &[parent]),
        });
    }
}

/// One `Item` per interface member: `method_elem`s become
/// `Interface.Method(sig)`, embedded interfaces and type constraints
/// (`type_elem`) read `Interface embeds T`.
fn emit_interface_members(
    iface_ty: Node<'_>,
    source: &str,
    path: &Path,
    parent: &str,
    out: &mut Surface,
) {
    let mut cursor = iface_ty.walk();
    for member in iface_ty.named_children(&mut cursor) {
        let (name, signature) = match member.kind() {
            "method_elem" => {
                let Some(method) = name_field(member, source) else {
                    continue;
                };
                let text = normalize_whitespace(&source[member.byte_range()]);
                (method.to_owned(), format!("{parent}.{text}"))
            }
            "type_elem" => {
                let text = normalize_whitespace(&source[member.byte_range()]);
                (text.clone(), format!("{parent} embeds {text}"))
            }
            _ => continue,
        };
        out.push(Item {
            id: ItemId {
                path: path.to_path_buf(),
                kind: Kind::InterfaceMethod,
                name: format!("{parent}.{name}"),
            },
            signature,
            line: start_line(member),
            parent: Some(parent.to_owned()),
            refs: collect_refs(member, source, None, &[parent]),
        });
    }
}

/// Splits `const (...)` / `var (...)` blocks into one `Item` per spec,
/// each keyed by its first identifier. Multi-name specs like
/// `const A, B = 1, 2` collapse to one item under `A` — splitting those
/// further is a refinement we'll make if real diffs demand it.
fn emit_value_specs(
    node: Node<'_>,
    source: &str,
    path: &Path,
    kind: Kind,
    prefix: &str,
    out: &mut Surface,
) {
    let target_spec = match kind {
        Kind::Const => "const_spec",
        Kind::Var => "var_spec",
        _ => return,
    };
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != target_spec {
            continue;
        }
        let Some(name) = first_spec_name(child, source) else {
            continue;
        };
        let body = normalize_whitespace(&source[child.byte_range()]);
        out.push(Item {
            id: ItemId {
                path: path.to_path_buf(),
                kind,
                name: name.into(),
            },
            signature: format!("{prefix} {body}"),
            line: start_line(child),
            parent: None,
            refs: collect_refs(child, source, None, &[]),
        });
    }
}

fn first_spec_name<'a>(spec: Node<'_>, source: &'a str) -> Option<&'a str> {
    let mut cursor = spec.walk();
    for child in spec.children(&mut cursor) {
        if child.kind() == "identifier" {
            return Some(&source[child.byte_range()]);
        }
    }
    None
}

fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::producer::Producer;

    fn extract(source: &str) -> Vec<Item> {
        let mut p = GoProducer::new().unwrap();
        let s = p.extract(&PathBuf::from("f.go"), source).unwrap();
        s.iter().cloned().collect()
    }

    #[test]
    fn function_strips_body() {
        let src = "package foo\n\nfunc Connect(addr string) Client {\n    return Client{}\n}\n";
        let items = extract(src);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.kind, Kind::Function);
        assert_eq!(items[0].id.name, "Connect");
        assert_eq!(items[0].signature, "func Connect(addr string) Client");
    }

    #[test]
    fn method_strips_body_keeps_receiver() {
        let src = "package foo\n\nfunc (c *Client) Close() error {\n    return nil\n}\n";
        let items = extract(src);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.kind, Kind::Method);
        assert_eq!(items[0].id.name, "Client.Close");
        assert_eq!(items[0].signature, "func (c *Client) Close() error");
    }

    #[test]
    fn method_id_collapses_value_and_pointer_receiver() {
        let pointer = extract("package foo\nfunc (c *Client) Close() error { return nil }");
        let value = extract("package foo\nfunc (c Client) Close() error { return nil }");
        assert_eq!(pointer[0].id, value[0].id);
    }

    #[test]
    fn methods_on_different_types_have_distinct_ids() {
        let src = "package foo\nfunc (c *Client) Close() error { return nil }\nfunc (s *Server) Close() error { return nil }\n";
        let items = extract(src);
        assert_eq!(items.len(), 2);
        assert_ne!(items[0].id, items[1].id);
        assert_eq!(items[0].id.name, "Client.Close");
        assert_eq!(items[1].id.name, "Server.Close");
    }

    #[test]
    fn struct_emits_header_then_one_item_per_field() {
        let src = "package foo\n\ntype Client struct {\n    timeout int\n    retries int\n}\n";
        let items = extract(src);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id.kind, Kind::Struct);
        assert_eq!(items[0].id.name, "Client");
        assert_eq!(items[0].signature, "type Client struct");
        assert_eq!(items[1].id.kind, Kind::Field);
        assert_eq!(items[1].id.name, "Client.timeout");
        assert_eq!(items[1].signature, "Client.timeout int");
        assert_eq!(items[2].id.name, "Client.retries");
        assert_eq!(items[2].line.0, 5);
    }

    #[test]
    fn multi_name_field_collapses_to_first_name() {
        let src = "package foo\n\ntype P struct {\n    x, y float64\n}\n";
        let items = extract(src);
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].id.name, "P.x");
        assert_eq!(items[1].signature, "P.x, y float64");
    }

    #[test]
    fn field_tag_is_part_of_the_shape() {
        let src = "package foo\n\ntype U struct {\n    Name string `json:\"name\"`\n}\n";
        let items = extract(src);
        assert_eq!(items[1].signature, "U.Name string `json:\"name\"`");
    }

    #[test]
    fn embedded_field_reads_embeds_and_strips_pointer() {
        let src = "package foo\n\ntype Client struct {\n    *Base\n    io.Reader\n}\n";
        let items = extract(src);
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].id.kind, Kind::Field);
        assert_eq!(items[1].id.name, "Client.Base");
        assert_eq!(items[1].signature, "Client embeds Base");
        assert_eq!(items[2].id.name, "Client.io.Reader");
        assert_eq!(items[2].signature, "Client embeds io.Reader");
    }

    #[test]
    fn interface_emits_header_then_one_item_per_method() {
        let src = "package foo\n\ntype Reader interface {\n    Read(p []byte) (int, error)\n    Close() error\n}\n";
        let items = extract(src);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id.kind, Kind::Interface);
        assert_eq!(items[0].signature, "type Reader interface");
        assert_eq!(items[1].id.kind, Kind::InterfaceMethod);
        assert_eq!(items[1].id.name, "Reader.Read");
        assert_eq!(items[1].signature, "Reader.Read(p []byte) (int, error)");
        assert_eq!(items[2].id.name, "Reader.Close");
        assert_eq!(items[2].signature, "Reader.Close() error");
    }

    #[test]
    fn embedded_interface_reads_embeds() {
        let src = "package foo\n\ntype ReadCloser interface {\n    Reader\n    io.Closer\n}\n";
        let items = extract(src);
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].id.kind, Kind::InterfaceMethod);
        assert_eq!(items[1].id.name, "ReadCloser.Reader");
        assert_eq!(items[1].signature, "ReadCloser embeds Reader");
        assert_eq!(items[2].id.name, "ReadCloser.io.Closer");
        assert_eq!(items[2].signature, "ReadCloser embeds io.Closer");
    }

    #[test]
    fn empty_struct_and_interface_emit_no_members() {
        let src = "package foo\n\ntype S struct{}\n\ntype I interface{}\n";
        let items = extract(src);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id.kind, Kind::Struct);
        assert_eq!(items[1].id.kind, Kind::Interface);
    }

    #[test]
    fn type_alias_uses_equals_kind_alias() {
        let src = "package foo\n\ntype Name = string\n";
        let items = extract(src);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.kind, Kind::TypeAlias);
        assert_eq!(items[0].signature, "type Name = string");
    }

    #[test]
    fn named_type_uses_kind_type() {
        let src = "package foo\n\ntype UserId string\n";
        let items = extract(src);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.kind, Kind::Type);
        assert_eq!(items[0].signature, "type UserId string");
    }

    #[test]
    fn grouped_type_block_yields_each_spec() {
        let src = "package foo\n\ntype (\n    Client struct{}\n    Reader interface{}\n)\n";
        let items = extract(src);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id.kind, Kind::Struct);
        assert_eq!(items[1].id.kind, Kind::Interface);
    }

    #[test]
    fn const_block_splits_per_spec() {
        let src = "package foo\n\nconst (\n    A = 1\n    B = 2\n)\n";
        let items = extract(src);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id.kind, Kind::Const);
        assert_eq!(items[0].id.name, "A");
        assert_eq!(items[0].signature, "const A = 1");
        assert_eq!(items[1].id.name, "B");
    }

    #[test]
    fn single_var_emits_one_item() {
        let src = "package foo\n\nvar Default Client = Client{timeout: 30}\n";
        let items = extract(src);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.kind, Kind::Var);
        assert_eq!(items[0].id.name, "Default");
    }

    #[test]
    fn mixed_file_preserves_source_order() {
        let src = "package foo\n\ntype Client struct{}\n\nfunc Connect() Client {\n    return Client{}\n}\n\nfunc (c *Client) Close() error {\n    return nil\n}\n";
        let items = extract(src);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id.kind, Kind::Struct);
        assert_eq!(items[1].id.kind, Kind::Function);
        assert_eq!(items[2].id.kind, Kind::Method);
    }

    #[test]
    fn empty_file_yields_empty_surface() {
        let items = extract("package foo\n");
        assert!(items.is_empty());
    }

    #[test]
    fn items_carry_their_declaration_line() {
        let src = "package foo\n\nfunc A() {}\n\nconst (\n    X = 1\n    Y = 2\n)\n";
        let items = extract(src);
        let lines: Vec<u32> = items.iter().map(|i| i.line.0).collect();
        // A on line 3; X and Y on their own spec lines inside the block.
        assert_eq!(lines, vec![3, 6, 7]);
    }
    #[test]
    fn items_carry_type_refs() {
        let src = "package foo\n\nfunc Connect(addr string, cfg Config) (*Client, error) {\n    return nil, nil\n}\n";
        let items = extract(src);
        let refs: Vec<&str> = items[0].refs.iter().map(|r| r.0.as_str()).collect();
        // string and error are predeclared; Config and Client are refs.
        assert_eq!(refs, vec!["Config", "Client"]);
    }

    #[test]
    fn members_and_methods_carry_parent_but_not_self_ref() {
        let src = concat!(
            "package foo\n",
            "type Client struct {\n    conn net.Conn\n}\n",
            "func (c *Client) Send(m Message) error { return nil }\n",
        );
        let items = extract(src);
        // items: struct header, field, method
        assert_eq!(items[0].parent, None);
        assert_eq!(items[1].parent.as_deref(), Some("Client"));
        let field_refs: Vec<&str> = items[1].refs.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(field_refs, vec!["net.Conn"]);
        assert_eq!(items[2].parent.as_deref(), Some("Client"));
        let method_refs: Vec<&str> = items[2].refs.iter().map(|r| r.0.as_str()).collect();
        // The receiver type is the parent, not a reference.
        assert_eq!(method_refs, vec!["Message"]);
    }
}
