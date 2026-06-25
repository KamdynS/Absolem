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

use crate::item::{Item, ItemId, Kind};
use crate::surface::Surface;

#[derive(Debug, thiserror::Error)]
pub(crate) enum GoError {
    #[error("tree-sitter language load failed")]
    LanguageLoad(#[from] tree_sitter::LanguageError),
    #[error("tree-sitter returned no tree")]
    NoTree,
}

pub(crate) struct GoProducer {
    parser: Parser,
}

impl GoProducer {
    pub(crate) fn new() -> Result<Self, GoError> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_go::LANGUAGE.into())?;
        Ok(Self { parser })
    }

    pub(crate) fn extract(&mut self, path: &Path, source: &str) -> Result<Surface, GoError> {
        let tree = self.parser.parse(source, None).ok_or(GoError::NoTree)?;
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
        "function_declaration" => {
            if let Some(name) = name_field(node, source) {
                out.push(Item {
                    id: ItemId {
                        path: path.to_path_buf(),
                        kind: Kind::Function,
                        name: name.into(),
                    },
                    signature: signature_without_body(node, source),
                });
            }
        }
        "method_declaration" => {
            if let Some(method_name) = method_name(node, source) {
                out.push(Item {
                    id: ItemId {
                        path: path.to_path_buf(),
                        kind: Kind::Method,
                        name: method_name,
                    },
                    signature: signature_without_body(node, source),
                });
            }
        }
        "type_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(child.kind(), "type_spec" | "type_alias")
                    && let Some(item) = type_spec_item(child, source, path)
                {
                    out.push(item);
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

fn name_field<'a>(node: Node<'a>, source: &'a str) -> Option<&'a str> {
    node.child_by_field_name("name")
        .map(|n| &source[n.byte_range()])
}

/// `Receiver.Method`; the receiver is the type name with `*` stripped so
/// `func (c *Client) X()` and `func (c Client) X()` collide as one id.
/// Go's rules forbid both existing simultaneously, so this is safe.
fn method_name(node: Node<'_>, source: &str) -> Option<String> {
    let method = name_field(node, source)?;
    let receiver = node.child_by_field_name("receiver")?;
    let receiver_type = receiver_type_name(receiver, source)?;
    Some(format!("{receiver_type}.{method}"))
}

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
    let end = node
        .child_by_field_name("body")
        .map_or_else(|| node.end_byte(), |b| b.start_byte());
    source[node.start_byte()..end].trim().to_owned()
}

fn type_spec_item(spec: Node<'_>, source: &str, path: &Path) -> Option<Item> {
    let name_node = spec.child_by_field_name("name")?;
    let name = &source[name_node.byte_range()];
    let ty_node = spec.child_by_field_name("type")?;
    let is_alias = spec.kind() == "type_alias";
    let (kind, signature) = match ty_node.kind() {
        "struct_type" => (Kind::Struct, format!("type {name} struct")),
        "interface_type" => (Kind::Interface, format!("type {name} interface")),
        _ => {
            let ty = &source[ty_node.byte_range()];
            if is_alias {
                (Kind::TypeAlias, format!("type {name} = {ty}"))
            } else {
                (Kind::Type, format!("type {name} {ty}"))
            }
        }
    };
    Some(Item {
        id: ItemId {
            path: path.to_path_buf(),
            kind,
            name: name.into(),
        },
        signature,
    })
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
    fn struct_renders_without_body() {
        let src = "package foo\n\ntype Client struct {\n    timeout int\n}\n";
        let items = extract(src);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.kind, Kind::Struct);
        assert_eq!(items[0].id.name, "Client");
        assert_eq!(items[0].signature, "type Client struct");
    }

    #[test]
    fn interface_renders_without_body() {
        let src = "package foo\n\ntype Reader interface {\n    Read(p []byte) (int, error)\n}\n";
        let items = extract(src);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.kind, Kind::Interface);
        assert_eq!(items[0].signature, "type Reader interface");
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
}
