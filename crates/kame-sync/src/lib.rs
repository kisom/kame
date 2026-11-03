//! A proof of concept for merging one person's diverged kame documents.
//!
//! It works on document states rather than on the op log, so coalescing and compacting a log
//! cannot get in its way. See `docs/superpowers/specs/2026-11-03-sync-design.md`.
//!
//! - [`text3`] merges one block's text by sentence
//! - [`merge`] merges two documents against the one they share
//! - [`files`] finds that shared document from two `.kame` files
//! - [`revlog`] carries revisions between machines in logs that each have one writer

pub mod files;
pub mod merge;
pub mod revlog;
pub mod text3;

use sha2::{Digest, Sha256};

/// A hex digest of `bytes`, shortened to 128 bits, which is plenty for one person's documents.
pub fn digest(bytes: &[u8]) -> String {
    let full = Sha256::digest(bytes);
    full[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
