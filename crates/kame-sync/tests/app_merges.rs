//! Merging as the app does it: into an open session.

mod common;

use std::path::{Path, PathBuf};

use kame_core::edit::Cursor;
use kame_core::session::Session;
use kame_sync::files::{conflict_copies, merge_copy_into, merge_replaced};

const COPY: &str = "doc.sync-conflict-20261103-011500-ABCDEFG.kame";

/// A document, and a copy of it as a sync tool would leave beside it.
fn with_copy() -> (PathBuf, PathBuf) {
    let dir = common::test_folder("app");
    let (path, copy) = (dir.join("doc.kame"), dir.join(COPY));
    {
        let mut session = Session::open(&path).unwrap();
        session.insert("A shared opening. It has words.");
        session.split();
        session.insert("A shared ending. It has more.");
    }
    std::fs::copy(&path, &copy).unwrap();
    (path, copy)
}

fn edit(session: &mut Session, block: usize, offset: usize, words: &str) {
    let block_id = session.document().blocks[block].id;
    session.set_caret(Cursor { block_id, offset });
    session.insert(words);
}

fn edit_file(path: &Path, block: usize, offset: usize, words: &str) {
    edit(&mut Session::open(path).unwrap(), block, offset, words);
}

#[test]
fn a_copy_of_this_document_is_found_and_nothing_else_is() {
    let (path, copy) = with_copy();
    let dir = path.parent().unwrap();
    assert_eq!(conflict_copies(&path), vec![copy.clone()]);
    for other in [
        "doc.kame.sync-conflict-20261103-011500-ABCDEFG",
        "doc.sync-conflict-20261103-011500-ABCDEFG.kame-wal",
        "doc.sync-conflict-20261103-011500-ABCDEFG.kame.merged",
        "docs.sync-conflict-20261103-011500-ABCDEFG.kame",
    ] {
        std::fs::write(dir.join(other), b"").unwrap();
    }
    assert_eq!(conflict_copies(&path), vec![copy]);
}

#[test]
fn a_file_named_like_a_copy_of_some_other_document_is_ignored() {
    let (path, _) = with_copy();
    let stranger = path
        .parent()
        .unwrap()
        .join("doc.sync-conflict-20261104-011500-XXXXXXX.kame");
    Session::open(&stranger)
        .unwrap()
        .insert("Something else entirely.");
    assert_eq!(conflict_copies(&path).len(), 1);
}

#[test]
fn merging_a_copy_takes_its_words_and_retires_it() {
    let (path, copy) = with_copy();
    edit_file(&copy, 1, 1, " grand");
    let mut session = Session::open(&path).unwrap();
    edit(&mut session, 0, 1, " fine");
    let merged = merge_copy_into(&mut session, &copy).unwrap();
    assert_eq!(merged.notes, 0);
    assert!(merged.changed);
    assert_eq!(
        session.document().blocks[0].text,
        "A fine shared opening. It has words."
    );
    assert_eq!(
        session.document().blocks[1].text,
        "A grand shared ending. It has more."
    );
    assert!(!copy.exists(), "no longer offered");
    assert!(conflict_copies(&path).is_empty());
    let mut retired = copy.into_os_string();
    retired.push(".merged");
    assert!(
        Path::new(&retired).exists(),
        "still there, under another name"
    );
}

#[test]
fn a_merge_is_one_undo_step_with_a_snapshot_before_it() {
    let (path, copy) = with_copy();
    edit_file(&copy, 1, 1, " grand");
    let mut session = Session::open(&path).unwrap();
    edit(&mut session, 0, 1, " fine");
    let before = session.document().clone();
    let snapshots = session.snapshots().unwrap().len();
    merge_copy_into(&mut session, &copy).unwrap();
    assert_eq!(session.snapshots().unwrap().len(), snapshots + 1);
    session.undo();
    assert_eq!(session.document(), &before);
}

#[test]
fn a_conflict_leaves_a_note_that_says_when_the_other_words_are_from() {
    let (path, copy) = with_copy();
    edit_file(&copy, 0, 1, " grand");
    let mut session = Session::open(&path).unwrap();
    edit(&mut session, 0, 1, " fine and also long enough to be later");
    let merged = merge_copy_into(&mut session, &copy).unwrap();
    assert_eq!(merged.notes, 1);
    let note = &session.document().annotations[0].body;
    assert!(note.contains("grand"), "{note}");
    assert!(note.contains("2026-11-03 01:15"), "{note}");
}

#[test]
fn a_copy_with_nothing_new_is_retired_without_touching_the_document() {
    let (path, copy) = with_copy();
    let mut session = Session::open(&path).unwrap();
    let steps = session.history().0;
    let merged = merge_copy_into(&mut session, &copy).unwrap();
    assert!(!merged.changed);
    assert_eq!(session.history().0, steps);
    assert!(!copy.exists());
}

#[test]
fn a_different_document_is_refused_and_left_where_it_is() {
    let (path, copy) = with_copy();
    let stranger = path.parent().unwrap().join("stranger.kame");
    Session::open(&stranger)
        .unwrap()
        .insert("Something else entirely.");
    let mut session = Session::open(&path).unwrap();
    let before = session.document().clone();
    assert!(merge_copy_into(&mut session, &stranger).is_err());
    assert_eq!(session.document(), &before);
    assert!(stranger.exists() && copy.exists());
}

#[test]
fn a_session_that_cannot_save_merges_nothing_and_retires_nothing() {
    let (path, copy) = with_copy();
    edit_file(&copy, 1, 1, " grand");
    let mut session = Session::open(&path).unwrap();
    session.save_failed = true;
    assert!(merge_copy_into(&mut session, &copy).is_err());
    assert!(copy.exists());
}

/// A file replaced under an open document: the words on screen and the words on disk both
/// count, including what was typed after nobody could save it.
#[test]
fn merging_with_the_file_that_replaced_this_one_keeps_both() {
    let (path, other) = with_copy();
    edit_file(&other, 1, 1, " grand");
    let mut old = Session::open(&path).unwrap();
    edit(&mut old, 0, 1, " fine");
    std::fs::rename(&other, &path).unwrap();
    edit(&mut old, 0, 0, "Unsaved words. ");
    assert!(old.replaced);
    let (mut new, merged) = merge_replaced(&old).unwrap();
    drop(old);
    assert!(!new.replaced && !new.save_failed);
    assert!(!merged.changed || merged.notes == 0, "{merged:?}");
    assert_eq!(
        new.document().blocks[1].text,
        "A grand shared ending. It has more."
    );
    assert!(new.document().blocks[0].text.contains("fine"));
    new.insert("x");
    assert!(!new.save_failed, "it saves to the file now at the path");
    drop(new);
    let reopened = Session::open(&path).unwrap();
    assert!(reopened.document().blocks[0].text.contains("fine"));
    assert!(reopened.document().blocks[1].text.contains("grand"));
}

#[test]
fn undoing_that_merge_gives_the_file_as_it_arrived() {
    let (path, other) = with_copy();
    edit_file(&other, 1, 1, " grand");
    let arrived = Session::open(&other).unwrap().document().clone();
    let mut old = Session::open(&path).unwrap();
    edit(&mut old, 0, 1, " fine");
    std::fs::rename(&other, &path).unwrap();
    old.check_file();
    let (mut new, _) = merge_replaced(&old).unwrap();
    new.undo();
    assert_eq!(new.document(), &arrived);
}

#[test]
fn a_file_gone_altogether_cannot_be_merged_with() {
    let (path, _) = with_copy();
    let mut old = Session::open(&path).unwrap();
    edit(&mut old, 0, 1, " fine");
    std::fs::remove_file(&path).unwrap();
    old.check_file();
    assert!(merge_replaced(&old).is_err());
    assert!(
        !path.exists(),
        "and opening it must not have made a new one"
    );
}

fn word_list_on(path: &Path, word: &str) {
    let mut session = Session::open(path).unwrap();
    let mut metadata = session.metadata.clone();
    metadata.words.push(word.to_string());
    session.set_metadata(metadata);
}

#[test]
fn merging_a_copy_keeps_the_word_lists_of_both() {
    let (path, copy) = with_copy();
    word_list_on(&path, "ours");
    word_list_on(&copy, "theirs");
    let mut session = Session::open(&path).unwrap();
    merge_copy_into(&mut session, &copy).unwrap();
    assert_eq!(session.metadata.words, vec!["ours", "theirs"]);
    drop(session);
    // It reached the file, and didn't need a change to the text to get there.
    assert_eq!(
        Session::open(&path).unwrap().metadata.words,
        vec!["ours", "theirs"]
    );
}

#[test]
fn merging_with_a_replaced_file_keeps_the_word_lists_of_both() {
    let (path, copy) = with_copy();
    let mut old = Session::open(&path).unwrap();
    let mut metadata = old.metadata.clone();
    metadata.words.push("ours".to_string());
    old.set_metadata(metadata);
    word_list_on(&copy, "theirs");
    std::fs::rename(&copy, &path).unwrap();
    old.check_file();
    let (session, _) = merge_replaced(&old).unwrap();
    assert_eq!(session.metadata.words, vec!["theirs", "ours"]);
}

/// A file whose log took away a note that started inside "é", recorded by the program that
/// damaged it. Taking the note away records it whole, so undo brings it back as it was, in memory,
/// where nothing reads it from the file to fit it.
fn with_a_damaged_note() -> (PathBuf, PathBuf) {
    use kame_core::edit::Op;
    use kame_core::model::{Anchor, Annotation, AnnotationKind, Document};
    let dir = common::test_folder("damaged");
    let (path, copy) = (dir.join("doc.kame"), dir.join(COPY));
    let mut doc = Document::new("Damaged");
    doc.blocks[0].text = "Café au lait.".to_string();
    let note = Annotation {
        id: uuid::Uuid::new_v4(),
        kind: AnnotationKind::Note,
        anchor: Anchor::range(doc.blocks[0].id, 4, 8),
        body: "a note".to_string(),
    };
    let taken = Op::RemoveAnnotation { annotation: note };
    kame_core::storage::Db::open(&path)
        .unwrap()
        .record_edit(&doc, &taken)
        .unwrap();
    std::fs::copy(&path, &copy).unwrap();
    (path, copy)
}

#[test]
fn a_copy_merges_into_a_document_holding_a_note_inside_a_character() {
    let (path, copy) = with_a_damaged_note();
    {
        let mut session = Session::open(&copy).unwrap();
        assert!(session.undo());
        let end = session.document().blocks[0].text.len();
        edit(&mut session, 0, end, " Sweet.");
    }
    let mut session = Session::open(&path).unwrap();
    assert!(session.undo());
    assert_eq!(
        session.document().annotations[0].anchor.byte_range(),
        Some((4, 8)),
        "as it was taken"
    );
    edit(&mut session, 0, 0, "Oh. ");

    merge_copy_into(&mut session, &copy).unwrap();

    // Both changed the one sentence, so the copy's words are in a note.
    let doc = session.document();
    assert_eq!(doc.blocks[0].text, "Oh. Café au lait.");
    let body = |ends: &str| {
        doc.annotations
            .iter()
            .find(|note| note.body.ends_with(ends))
            .unwrap_or_else(|| panic!("no note ending {ends:?}: {doc:?}"))
    };
    body("Café au lait. Sweet.");
    assert_eq!(body("a note").anchor.byte_range(), Some((7, 12)));
}
