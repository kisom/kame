//! Merging two `.kame` files, such as a document and a Syncthing conflict copy beside it.

use std::path::Path;

use kame_core::edit::apply_inverse;
use kame_core::edit::Op;
use kame_core::model::Document;
use kame_core::storage::Db;

use crate::merge::{merge, Merged, Side};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A document as a file holds it, with the part of its log that led to it.
pub struct Version {
    pub doc: Document,
    pub ops: Vec<Op>,
}

impl Version {
    pub fn read(path: &Path) -> Result<Version> {
        let db = Db::open_read_only(path)?;
        let doc = db.load_document()?;
        let cursor = db.cursor()?;
        let mut ops: Vec<Op> = db.load_ops()?.into_iter().map(|(_, op)| op).collect();
        // What undo took back is not part of how this document came to be.
        ops.truncate(cursor);
        Ok(Version { doc, ops })
    }
}

/// How many ops the two share from the start.
pub fn shared_ops(one: &[Op], other: &[Op]) -> usize {
    one.iter().zip(other).take_while(|(a, b)| a == b).count()
}

/// The document as it was after the ops both share, found by undoing the rest of one side.
///
/// The ops are only a way to get there. If compacting one side's history has left nothing shared,
/// this gives an older document than the true one, which means more conflicts and no lost words.
pub fn shared_base(version: &Version, shared: usize) -> Document {
    let mut doc = version.doc.clone();
    for op in version.ops[shared..].iter().rev() {
        apply_inverse(&mut doc, op);
    }
    doc
}

pub fn merge_versions(
    one: &Version,
    one_label: &str,
    other: &Version,
    other_label: &str,
) -> Merged {
    let shared = shared_ops(&one.ops, &other.ops);
    let base = shared_base(one, shared);
    // A file does not say when it was written, so the longer history counts as the later.
    merge(
        Some(&base),
        Side {
            doc: &one.doc,
            label: one_label,
            when: one.ops.len() as i64,
        },
        Side {
            doc: &other.doc,
            label: other_label,
            when: other.ops.len() as i64,
        },
    )
}
