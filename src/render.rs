//! Stdout tree renderer. Pure: takes a `&mut impl Write` so tests render
//! to a `Vec<u8>` and assert on the bytes.

use std::io::{self, Write};
use std::path::Path;

use crate::decl::Decl;

pub(crate) fn render_blank(out: &mut impl Write) -> io::Result<()> {
    writeln!(out)
}

pub(crate) fn render_deleted(out: &mut impl Write, path: &Path) -> io::Result<()> {
    writeln!(out, "DELETED {}", path.display())
}

pub(crate) fn render_file(out: &mut impl Write, path: &Path, decls: &[Decl]) -> io::Result<()> {
    writeln!(out, "{}", path.display())?;
    for decl in decls {
        writeln!(out, "  {}", decl.signature)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::decl::Kind;

    fn capture(f: impl FnOnce(&mut Vec<u8>) -> io::Result<()>) -> String {
        let mut buf = Vec::new();
        f(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn deleted_prints_marker() {
        let s = capture(|out| render_deleted(out, &PathBuf::from("foo.go")));
        assert_eq!(s, "DELETED foo.go\n");
    }

    #[test]
    fn file_prints_path_then_indented_decls() {
        let decls = vec![
            Decl {
                signature: "type Client struct".into(),
                kind: Kind::Struct,
            },
            Decl {
                signature: "func Connect() Client".into(),
                kind: Kind::Function,
            },
        ];
        let s = capture(|out| render_file(out, &PathBuf::from("net/client.go"), &decls));
        assert_eq!(
            s,
            "net/client.go\n  type Client struct\n  func Connect() Client\n"
        );
    }

    #[test]
    fn file_with_no_decls_prints_only_path() {
        let s = capture(|out| render_file(out, &PathBuf::from("empty.go"), &[]));
        assert_eq!(s, "empty.go\n");
    }
}
