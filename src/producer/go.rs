//! Tree-sitter-based extraction of top-level Go declarations.
//!
//! Walks the root node's direct children. For each declaration:
//! function/method bodies are stripped at the `body` field; struct and
//! interface type bodies are stripped to `type <name> struct` /
//! `type <name> interface`; aliases and `type Foo Bar` definitions are
//! rendered as-is. `const` and `var` blocks render as a single
//! whitespace-normalized item — splitting grouped specs is a later
//! refinement.

use tree_sitter::{Node, Parser};

use crate::decl::{Decl, Kind};

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

    pub(crate) fn parse(&mut self, source: &str) -> Result<Vec<Decl>, GoError> {
        let tree = self.parser.parse(source, None).ok_or(GoError::NoTree)?;
        let root = tree.root_node();
        let mut decls = Vec::new();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            extract_into(child, source, &mut decls);
        }
        Ok(decls)
    }
}

fn extract_into(node: Node<'_>, source: &str, out: &mut Vec<Decl>) {
    match node.kind() {
        "function_declaration" => out.push(Decl {
            signature: signature_without_body(node, source),
            kind: Kind::Function,
        }),
        "method_declaration" => out.push(Decl {
            signature: signature_without_body(node, source),
            kind: Kind::Method,
        }),
        "type_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(child.kind(), "type_spec" | "type_alias")
                    && let Some(decl) = type_spec_decl(child, source)
                {
                    out.push(decl);
                }
            }
        }
        "const_declaration" => out.push(Decl {
            signature: normalize_whitespace(&source[node.byte_range()]),
            kind: Kind::Const,
        }),
        "var_declaration" => out.push(Decl {
            signature: normalize_whitespace(&source[node.byte_range()]),
            kind: Kind::Var,
        }),
        _ => {}
    }
}

fn signature_without_body(node: Node<'_>, source: &str) -> String {
    let end = node
        .child_by_field_name("body")
        .map_or_else(|| node.end_byte(), |b| b.start_byte());
    source[node.start_byte()..end].trim().to_owned()
}

fn type_spec_decl(spec: Node<'_>, source: &str) -> Option<Decl> {
    let name_node = spec.child_by_field_name("name")?;
    let name = &source[name_node.byte_range()];
    let ty_node = spec.child_by_field_name("type")?;
    let is_alias = spec.kind() == "type_alias";
    let (kind, signature) = match ty_node.kind() {
        "struct_type" => (Kind::Struct, format!("type {name} struct")),
        "interface_type" => (Kind::Interface, format!("type {name} interface")),
        _ => {
            let ty = &source[ty_node.byte_range()];
            let sig = if is_alias {
                format!("type {name} = {ty}")
            } else {
                format!("type {name} {ty}")
            };
            (Kind::TypeAlias, sig)
        }
    };
    Some(Decl { signature, kind })
}

fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Vec<Decl> {
        let mut p = GoProducer::new().unwrap();
        p.parse(source).unwrap()
    }

    #[test]
    fn function_strips_body() {
        let src = "package foo\n\nfunc Connect(addr string) Client {\n    return Client{}\n}\n";
        let decls = parse(src);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].kind, Kind::Function);
        assert_eq!(decls[0].signature, "func Connect(addr string) Client");
    }

    #[test]
    fn method_strips_body_keeps_receiver() {
        let src = "package foo\n\nfunc (c *Client) Close() error {\n    return nil\n}\n";
        let decls = parse(src);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].kind, Kind::Method);
        assert_eq!(decls[0].signature, "func (c *Client) Close() error");
    }

    #[test]
    fn struct_renders_without_body() {
        let src = "package foo\n\ntype Client struct {\n    timeout int\n}\n";
        let decls = parse(src);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].kind, Kind::Struct);
        assert_eq!(decls[0].signature, "type Client struct");
    }

    #[test]
    fn interface_renders_without_body() {
        let src = "package foo\n\ntype Reader interface {\n    Read(p []byte) (int, error)\n}\n";
        let decls = parse(src);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].kind, Kind::Interface);
        assert_eq!(decls[0].signature, "type Reader interface");
    }

    #[test]
    fn type_alias_uses_equals() {
        let src = "package foo\n\ntype Name = string\n";
        let decls = parse(src);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].kind, Kind::TypeAlias);
        assert_eq!(decls[0].signature, "type Name = string");
    }

    #[test]
    fn grouped_type_block_yields_each_spec() {
        let src = "package foo\n\ntype (\n    Client struct{}\n    Reader interface{}\n)\n";
        let decls = parse(src);
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].kind, Kind::Struct);
        assert_eq!(decls[1].kind, Kind::Interface);
    }

    #[test]
    fn mixed_file_preserves_source_order() {
        let src = "package foo\n\ntype Client struct{}\n\nfunc Connect() Client {\n    return Client{}\n}\n\nfunc (c *Client) Close() error {\n    return nil\n}\n";
        let decls = parse(src);
        assert_eq!(decls.len(), 3);
        assert_eq!(decls[0].kind, Kind::Struct);
        assert_eq!(decls[1].kind, Kind::Function);
        assert_eq!(decls[2].kind, Kind::Method);
    }

    #[test]
    fn empty_file_yields_no_decls() {
        let decls = parse("package foo\n");
        assert!(decls.is_empty());
    }
}
