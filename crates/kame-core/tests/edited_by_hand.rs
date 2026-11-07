//! A head changed by another program under a history kame wrote, so that the history no longer
//! leads to it. A step that no longer fits is refused at undo or redo, with nothing changed, so
//! that what's in the head isn't lost to what the history remembers.

use kame_core::edit::{Cursor, Op};
use kame_core::model::{Block, BlockKind, Document};
use kame_core::session::Session;
use kame_core::storage::Db;
use uuid::Uuid;

fn paragraph(text: &str) -> Block {
    Block {
        id: Uuid::new_v4(),
        kind: BlockKind::Paragraph,
        text: text.to_string(),
        spans: Vec::new(),
    }
}

/// Fuzzing found this (`kame-typeset/tests/fuzz_files.rs`, seed 1013081). A merge's step holds
/// both whole documents, and putting either back took no notice of what was there, so undoing it
/// and redoing it put back the merge's words over the ones the head had since.
#[test]
fn a_merge_undone_over_words_it_did_not_leave_is_refused() {
    let path = kame_test_dir::folder("by-hand").join("edited.kame");
    let mut old = Document::new("Edited");
    old.blocks = vec![paragraph("The end.")];
    let mut merged = old.clone();
    merged.blocks[0].text = "The end. Merged in.".to_string();
    let step = Op::ReplaceDocument {
        old: Box::new(old),
        new: Box::new(merged.clone()),
    };
    let mut db = Db::open(&path).unwrap();
    db.record_edit(&merged, &step).unwrap();
    // Another program changes the words, and not the history.
    let mut by_hand = merged.clone();
    by_hand.blocks[0].text = "The end. Merged in, and written by hand.".to_string();
    db.save_document(&by_hand).unwrap();
    drop(db);

    let mut session = Session::open(&path).unwrap();
    assert!(!session.undo(), "the merge was undone over other words");
    assert_eq!(session.document().blocks[0].text, by_hand.blocks[0].text);
    assert!(session.last_error.is_some(), "the refusal isn't said");
    assert!(!session.redo());
    assert_eq!(session.document().blocks[0].text, by_hand.blocks[0].text);
}

/// A merge undone and redone where nothing else changed the words goes on working, whatever
/// order the notes came back in.
#[test]
fn a_merge_undoes_and_redoes_over_its_own_words() {
    let path = kame_test_dir::folder("by-hand").join("merged.kame");
    let mut session = Session::open(&path).unwrap();
    session.insert("The end.");
    let mut merged = session.document().clone();
    merged.blocks[0].text.push_str(" Merged in.");
    assert!(session.replace_document(merged.clone()));
    let end = Cursor {
        block_id: merged.blocks[0].id,
        offset: merged.blocks[0].text.len(),
    };
    session.set_caret(end);
    session.insert(" Typed after.");
    assert!(session.undo());
    assert!(session.undo());
    assert_eq!(session.document().blocks[0].text, "The end.");
    assert!(session.redo());
    assert!(session.redo());
    assert_eq!(
        session.document().blocks[0].text,
        "The end. Merged in. Typed after."
    );
}
