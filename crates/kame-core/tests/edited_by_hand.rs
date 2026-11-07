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

/// Fuzzing found this (`kame-typeset/tests/fuzz_files.rs`, seed 1052619): a log missing the step
/// that typed into a new block. Undoing the step that made the block took it out, words and all,
/// as only its id was checked, and redoing it put back the empty block it made.
#[test]
fn undoing_a_new_block_that_has_words_it_was_not_made_with_is_refused() {
    let path = kame_test_dir::folder("by-hand").join("typed.kame");
    let mut doc = Document::new("Typed");
    doc.blocks = vec![paragraph("First.")];
    let made = paragraph("");
    let step = Op::InsertBlock {
        index: 1,
        block: made.clone(),
    };
    // The head has the words typed into the block, and the log lost the step that typed them.
    doc.blocks.push(Block {
        text: "Typed into it.".to_string(),
        ..made
    });
    let mut db = Db::open(&path).unwrap();
    db.record_edit(&doc, &step).unwrap();
    drop(db);

    let mut session = Session::open(&path).unwrap();
    assert!(!session.undo(), "the block was taken out with its words");
    assert_eq!(session.document().blocks[1].text, "Typed into it.");
    assert!(session.last_error.is_some(), "the refusal isn't said");
}

/// As for a block, which fuzzing found as well (seed 1105775): a log missing the step that wrote a
/// note's words. Undoing the step that added the note took it out, words and all, as nothing was
/// checked, and redoing it put back the note it added, with none.
#[test]
fn undoing_a_new_note_that_has_words_it_was_not_added_with_is_refused() {
    use kame_core::model::{Anchor, Annotation, AnnotationKind};
    let path = kame_test_dir::folder("by-hand").join("noted.kame");
    let mut doc = Document::new("Noted");
    doc.blocks = vec![paragraph("Some words.")];
    let added = Annotation {
        id: Uuid::new_v4(),
        kind: AnnotationKind::Note,
        anchor: Anchor::range(doc.blocks[0].id, 0, 4),
        body: String::new(),
    };
    let step = Op::AddAnnotation {
        annotation: added.clone(),
    };
    // The head has the note's words, and the log lost the step that wrote them.
    doc.annotations.push(Annotation {
        body: "Written into it.".to_string(),
        ..added
    });
    let mut db = Db::open(&path).unwrap();
    db.record_edit(&doc, &step).unwrap();
    drop(db);

    let mut session = Session::open(&path).unwrap();
    assert!(!session.undo(), "the note was taken out with its words");
    assert_eq!(session.document().annotations[0].body, "Written into it.");
}
