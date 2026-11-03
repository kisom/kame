//! Two `.kame` files, the way a sync tool's conflict copy leaves them.

use std::path::PathBuf;

use kame_core::edit::Cursor;
use kame_core::session::Session;
use kame_sync::files::{merge_versions, shared_ops, Version};
use uuid::Uuid;

fn dir() -> PathBuf {
    let dir = std::env::temp_dir()
        .join("kame-sync-tests")
        .join(format!("files-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A document typed in two paragraphs, closed, and copied: two machines that then drift.
fn diverged() -> (PathBuf, PathBuf) {
    let dir = dir();
    let (one, other) = (dir.join("talos.kame"), dir.join("titan.kame"));
    {
        let mut session = Session::open(&one).unwrap();
        session.insert("A shared opening. It has words.");
        session.split();
        session.insert("A shared ending. It has more.");
    }
    std::fs::copy(&one, &other).unwrap();
    (one, other)
}

fn edit(path: &PathBuf, block: usize, offset: usize, words: &str) {
    let mut session = Session::open(path).unwrap();
    let block_id = session.document().blocks[block].id;
    session.set_caret(Cursor { block_id, offset });
    session.insert(words);
}

#[test]
fn the_shared_log_finds_the_base_and_the_merge_is_clean() {
    let (one, other) = diverged();
    edit(&one, 0, 1, " fine");
    edit(&other, 1, 1, " grand");
    let (first, second) = (Version::read(&one).unwrap(), Version::read(&other).unwrap());
    assert!(shared_ops(&first.ops, &second.ops) > 0);
    let merged = merge_versions(&first, "talos", &second, "titan");
    assert_eq!(
        merged.doc.blocks[0].text,
        "A fine shared opening. It has words."
    );
    assert_eq!(
        merged.doc.blocks[1].text,
        "A grand shared ending. It has more."
    );
    assert!(merged.notes.is_empty());
}

#[test]
fn a_compacted_history_costs_notes_and_no_words() {
    let (one, other) = diverged();
    edit(&one, 0, 1, " fine");
    edit(&other, 0, 1, " grand");
    {
        let mut session = Session::open(&one).unwrap();
        let _ = session.compact_history();
    }
    let (first, second) = (Version::read(&one).unwrap(), Version::read(&other).unwrap());
    let merged = merge_versions(&first, "talos", &second, "titan");
    let mut words: String = merged
        .doc
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect();
    words.extend(merged.doc.annotations.iter().map(|note| note.body.as_str()));
    assert!(words.contains("fine"), "{words}");
    assert!(words.contains("grand"), "{words}");
}
