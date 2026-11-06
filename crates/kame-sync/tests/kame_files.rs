//! Two `.kame` files, the way a sync tool's conflict copy leaves them.

mod common;

use std::path::{Path, PathBuf};

use kame_core::edit::Cursor;
use kame_core::session::Session;
use kame_sync::files::{merge_versions, shared_ops, Version};

/// A document typed in two paragraphs, closed, and copied: two machines that then drift.
fn diverged() -> (PathBuf, PathBuf) {
    let dir = common::test_folder("files");
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

fn edit(path: &Path, block: usize, offset: usize, words: &str) {
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

/// A damaged file can repeat a block id. Read from each copy, every block is kept, so merging two
/// that changed since a third loses none of their words.
#[test]
fn copies_of_a_document_that_repeats_a_block_id_merge_keeping_every_block() {
    use kame_core::model::{Block, BlockKind, Document};
    use kame_core::storage::Db;
    use kame_sync::merge::{merge, Side};
    let dir = common::test_folder("repeated");
    let id = uuid::Uuid::from_u128(16);
    let block = |text: &str| Block {
        id,
        kind: BlockKind::Paragraph,
        text: text.to_string(),
        spans: Vec::new(),
    };
    let doc = Document::new("Repeated");
    let read = |name: &str, first: &str, second: &str| {
        let mut doc = doc.clone();
        doc.blocks = vec![block(first), block(second)];
        let path = dir.join(name);
        Db::open(&path).unwrap().save_document(&doc).unwrap();
        Version::read(&path).unwrap().doc
    };
    let base = read("base.kame", "One here.", "Two there.");
    let talos = read("talos.kame", "One here, now.", "Two there.");
    let titan = read("titan.kame", "One here.", "Two there, and more.");

    let side = |doc, label| Side {
        doc,
        label,
        when: 1,
    };
    let merged = merge(Some(&base), side(&talos, "talos"), side(&titan, "titan"));

    let texts: Vec<&str> = merged.doc.blocks.iter().map(|b| b.text.as_str()).collect();
    assert_eq!(texts, ["One here, now.", "Two there, and more."]);
    assert_ne!(merged.doc.blocks[0].id, merged.doc.blocks[1].id);
}
