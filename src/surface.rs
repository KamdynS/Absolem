//! The structural API of one source file at one git ref.
//!
//! Currently single-file scoped; one `Surface` per `(path, ref)`. When the
//! diff grows to whole-codebase scope (or cross-file resolution lands),
//! this is where that aggregation belongs — producers will keep emitting
//! per-file Surfaces and the composition root will compose them.
//!
//! Insertion order is preserved so renderers can present items in source
//! order without re-sorting. `by_id` is an index built lazily for the diff.

use std::collections::HashMap;

use crate::item::{Item, ItemId};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Surface {
    items: Vec<Item>,
}

impl Surface {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, item: Item) {
        self.items.push(item);
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Item> {
        self.items.iter()
    }

    pub(crate) fn by_id(&self) -> HashMap<&ItemId, &Item> {
        self.items.iter().map(|i| (&i.id, i)).collect()
    }
}
