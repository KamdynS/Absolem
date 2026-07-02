//! Language producers: turn source text into a `Surface`, dispatched by
//! file extension. Adding a language is a new `Producer` impl plus one
//! entry in `Registry::with_defaults` — grammar (data), not a new binary.

use std::collections::HashMap;
use std::path::Path;

use crate::surface::Surface;

pub(crate) mod go;
pub(crate) mod rust;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProducerError {
    #[error("tree-sitter language load failed")]
    LanguageLoad(#[from] tree_sitter::LanguageError),
    #[error("tree-sitter returned no tree")]
    NoTree,
}

/// A syntactic producer for one language: parses source at a path into the
/// structural `Surface`. Object-safe so a heterogeneous `Registry` can hold
/// producers for different languages behind one extraction path.
pub(crate) trait Producer {
    fn extract(&mut self, path: &Path, source: &str) -> Result<Surface, ProducerError>;
}

/// Maps a file extension to the producer that handles it. The one place
/// `dyn` is warranted: Surfaces from different languages flow through a
/// single path, chosen by the file's extension.
pub(crate) struct Registry {
    by_extension: HashMap<&'static str, Box<dyn Producer>>,
}

impl Registry {
    /// Builds the registry of languages absolem ships today.
    pub(crate) fn with_defaults() -> Result<Self, ProducerError> {
        let mut by_extension: HashMap<&'static str, Box<dyn Producer>> = HashMap::new();
        by_extension.insert("go", Box::new(go::GoProducer::new()?));
        by_extension.insert("rs", Box::new(rust::RustProducer::new()?));
        Ok(Self { by_extension })
    }

    /// Whether any producer claims this path — i.e. it's a source file in a
    /// language we extract. Lets the composition root drop unrelated files
    /// (and unrelated deletions) before diffing.
    pub(crate) fn supports(&self, path: &Path) -> bool {
        extension(path).is_some_and(|ext| self.by_extension.contains_key(ext))
    }

    /// The producer for this path, if any.
    pub(crate) fn for_path(&mut self, path: &Path) -> Option<&mut (dyn Producer + 'static)> {
        let ext = extension(path)?;
        self.by_extension.get_mut(ext).map(Box::as_mut)
    }
}

fn extension(path: &Path) -> Option<&str> {
    path.extension().and_then(|ext| ext.to_str())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn dispatches_by_extension() {
        let mut registry = Registry::with_defaults().unwrap();
        assert!(registry.supports(Path::new("a/b/main.go")));
        assert!(registry.supports(Path::new("src/lib.rs")));
        assert!(!registry.supports(Path::new("README.md")));
        assert!(!registry.supports(Path::new("Makefile")));

        assert!(registry.for_path(Path::new("x.go")).is_some());
        assert!(registry.for_path(Path::new("x.rs")).is_some());
        assert!(registry.for_path(Path::new("x.py")).is_none());
    }

    #[test]
    fn each_producer_extracts_its_own_language() {
        let mut registry = Registry::with_defaults().unwrap();
        let go = registry
            .for_path(Path::new("f.go"))
            .unwrap()
            .extract(Path::new("f.go"), "package p\nfunc F() {}\n")
            .unwrap();
        assert_eq!(go.iter().count(), 1);

        let rust = registry
            .for_path(Path::new("f.rs"))
            .unwrap()
            .extract(Path::new("f.rs"), "pub fn f() {}\n")
            .unwrap();
        assert_eq!(rust.iter().count(), 1);
    }
}
