//! The in-memory editor: a document plus a linear undo/redo history.
//!
//! History is a cursor over the durable op log (see [`crate::storage::Db`]).
//! Every edit is applied through [`Editor::apply_edit`], which returns the op
//! so the caller can persist it with `Db::record_edit`; undo and redo move the
//! cursor and are persisted with `Db::save_state`, which writes the head
//! document and the cursor in one transaction. A new edit after undo discards
//! the redo tail, which keeps the log linear.
//!
//! The cursor is what makes reload correct: `Db::cursor()` says how much of the
//! log is reflected in the saved head, so [`Editor::with_cursor`] rebuilds the
//! same undo/redo split and an undone op is never offered for undo twice.

use crate::edit::ops::{apply, apply_inverse, invert, normalize, Op};
use crate::model::Document;

/// A document with an undo/redo history.
///
/// Persist after every mutation:
///
/// - after [`Editor::apply_edit`], save with `Db::record_edit(document(), &op)`;
/// - after [`Editor::undo`]/[`Editor::redo`], save with
///   `Db::save_state(document(), editor.cursor())`.
///
/// Those two calls are the only write paths: each writes the head document and
/// the cursor (and, for an edit, the op) in one transaction, so the stored
/// cursor always describes the stored head. Consequently `Db::cursor()` and
/// [`Editor::cursor`] must agree before every write — always persist the edit
/// you just made.
pub struct Editor {
    doc: Document,
    undo: Vec<Op>,
    redo: Vec<Op>,
}

impl Editor {
    /// A fresh editor with empty history.
    pub fn new(doc: Document) -> Editor {
        Editor {
            doc,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// An editor resumed from a saved head document plus the ops that produced
    /// it, oldest first (`Db::load_ops` order). The whole log becomes the undo
    /// stack, so undo works identically before and after a reopen.
    ///
    /// Prefer [`Editor::with_cursor`] with `Db::cursor()`: this treats every
    /// logged op as applied, which is only right when nothing was undone.
    pub fn with_history(doc: Document, ops: Vec<Op>) -> Editor {
        let cursor = ops.len();
        Editor::with_cursor(doc, ops, cursor)
    }

    /// An editor resumed from a saved head document, the whole log, and the
    /// persisted undo cursor — the number of ops already applied to `doc`.
    ///
    /// The undo stack is the first `cursor` ops (reversed), the redo stack the
    /// rest. `cursor` is clamped to `ops.len()`; a larger value can only come
    /// from a log that was truncated behind the cursor's back.
    pub fn with_cursor(doc: Document, ops: Vec<Op>, cursor: usize) -> Editor {
        let cursor = cursor.min(ops.len());
        let redo: Vec<Op> = ops[cursor..].iter().rev().cloned().collect();
        let undo: Vec<Op> = ops[..cursor].to_vec();
        Editor { doc, undo, redo }
    }

    /// The current document.
    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// How many logged ops are applied to [`Editor::document`]: the undo cursor
    /// to persist alongside the head document.
    pub fn cursor(&self) -> usize {
        self.undo.len()
    }

    /// Applies `op` to the document, records it for undo, discards the redo
    /// tail (the log stays linear), and returns it for the caller to persist.
    ///
    /// This is the single chokepoint for edits, so it also normalizes a
    /// removal op's payload against the current document first: a hand-built
    /// `DeleteBlock` with no `annotations` is repaired here instead of
    /// destroying the block's notes. A payload that is present but wrong is
    /// rejected by [`apply`], not silently overwritten.
    pub fn apply_edit(&mut self, op: Op) -> Op {
        let op = normalize(&self.doc, op);
        apply(&mut self.doc, &op);
        self.undo.push(op.clone());
        self.redo.clear();
        op
    }

    /// Undoes the most recent edit, returning the inverse that was applied so
    /// the caller can report it. `None` when there is nothing to undo.
    ///
    /// Persist the result with `Db::save_state(document(), cursor())` — the
    /// cursor after this call is one lower, and head + cursor must move
    /// together.
    ///
    /// The undo goes through [`apply_inverse`], so the notes destroyed by a
    /// deletion come back with it: a `DeleteBlock` restores its block and the
    /// notes anchored in it; a `DeleteText` restores the deleted text and the
    /// notes that lay wholly inside the deleted range.
    ///
    /// # Limitation: straddling notes
    ///
    /// A note whose anchor merely *overlaps* the deleted range (it starts
    /// before the delete and ends inside or after it) has its surviving
    /// endpoints remapped, so the note keeps pointing at the right text — but
    /// its range is permanently shorter. Undo re-inserts the text and shifts
    /// the note left/right with it; it cannot re-expand the range to the
    /// original extent, because the collapsed anchor carries no record of how
    /// far it used to reach. Such a note ends up anchored too short after
    /// delete + undo. Snapshots remain the recovery path for that case; the
    /// op log recovers every other deletion losslessly.
    pub fn undo(&mut self) -> Option<Op> {
        let op = self.undo.pop()?;
        apply_inverse(&mut self.doc, &op);
        let inverse = invert(&op);
        self.redo.push(op);
        Some(inverse)
    }

    /// Redoes the most recently undone edit, returning the op that was applied.
    /// `None` when there is nothing to redo.
    ///
    /// Persist with `Db::save_state(document(), cursor())`, as for
    /// [`Editor::undo`].
    pub fn redo(&mut self) -> Option<Op> {
        let op = self.redo.pop()?;
        apply(&mut self.doc, &op);
        self.undo.push(op.clone());
        Some(op)
    }

    /// `(undo depth, redo depth)` — how many edits can be undone and redone.
    pub fn history(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Anchor, Annotation, AnnotationKind, Block, BlockKind};
    use crate::storage::Db;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn doc_with(text: &str) -> Document {
        let mut d = Document::new("Doc");
        d.blocks[0].text = text.to_string();
        d
    }

    fn temp_db_path() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kame-edit-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("kame.db")
    }

    fn note(block_id: Uuid, start: usize, end: usize) -> Annotation {
        Annotation {
            id: Uuid::new_v4(),
            kind: AnnotationKind::Marginal,
            anchor: Anchor {
                block_id,
                start,
                end,
            },
            body: "note".to_string(),
        }
    }

    #[test]
    fn insert_then_undo_restores_document() {
        let doc = doc_with("");
        let block_id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        e.apply_edit(Op::InsertText {
            block_id,
            offset: 0,
            text: "hi".into(),
        });
        assert_eq!(e.document().blocks[0].text, "hi");
        assert_eq!(e.cursor(), 1);

        e.undo();
        assert_eq!(e.document().blocks[0].text, "");
        assert_eq!(e.cursor(), 0);
    }

    #[test]
    fn undo_then_redo_round_trips_and_reports_depths() {
        let doc = doc_with("ab");
        let block_id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        e.apply_edit(Op::InsertText {
            block_id,
            offset: 2,
            text: "cd".into(),
        });
        assert_eq!(e.history(), (1, 0));

        let undone = e.undo().expect("an op to undo");
        assert_eq!(e.document().blocks[0].text, "ab");
        assert_eq!(e.history(), (0, 1));
        assert_eq!(
            undone,
            Op::DeleteText {
                block_id,
                start: 2,
                end: 4,
                text: "cd".into(),
                removed: Vec::new(),
            }
        );

        let redone = e.redo().expect("an op to redo");
        assert_eq!(e.document().blocks[0].text, "abcd");
        assert_eq!(e.history(), (1, 0));
        assert_eq!(
            redone,
            Op::InsertText {
                block_id,
                offset: 2,
                text: "cd".into(),
            }
        );
    }

    #[test]
    fn undo_on_empty_history_is_none() {
        let mut e = Editor::new(doc_with("x"));
        assert_eq!(e.undo(), None);
        assert_eq!(e.redo(), None);
    }

    /// `with_history` is `with_cursor` at the end of the log; an
    /// out-of-range cursor clamps instead of panicking.
    #[test]
    fn with_history_is_with_cursor_at_the_end_of_the_log() {
        let doc = doc_with("ab");
        let block_id = doc.blocks[0].id;
        let op = Op::InsertText {
            block_id,
            offset: 2,
            text: "cd".into(),
        };
        let mut edited = doc.clone();
        edited.blocks[0].text = "abcd".to_string();

        let mut a = Editor::with_history(edited.clone(), vec![op.clone()]);
        let mut b = Editor::with_cursor(edited.clone(), vec![op.clone()], 1);
        assert_eq!(a.history(), (1, 0));
        assert_eq!(b.history(), (1, 0));
        assert_eq!(a.cursor(), 1);
        assert_eq!(b.cursor(), 1);
        a.undo().expect("undo");
        b.undo().expect("undo");
        assert_eq!(a.document(), b.document());
        assert_eq!(a.document().blocks[0].text, "ab");

        let clamped = Editor::with_cursor(edited, vec![op], 99);
        assert_eq!(clamped.cursor(), 1, "a cursor past the log clamps");
        assert_eq!(clamped.history(), (1, 0));
    }

    #[test]
    fn new_edit_after_undo_clears_redo() {
        let doc = doc_with("ab");
        let block_id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        e.apply_edit(Op::InsertText {
            block_id,
            offset: 0,
            text: "x".into(),
        });
        e.undo();
        assert_eq!(e.history(), (0, 1));

        e.apply_edit(Op::InsertText {
            block_id,
            offset: 2,
            text: "y".into(),
        });
        assert_eq!(e.history(), (1, 0));
        assert_eq!(e.redo(), None);
        assert_eq!(e.document().blocks[0].text, "aby");
    }

    #[test]
    fn deleting_anchor_block_removes_annotation_and_undo_restores() {
        let mut doc = doc_with("first");
        let second = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "second".to_string(),
            spans: Vec::new(),
        };
        doc.blocks.push(second.clone());
        let annotation = note(second.id, 0, 6);
        doc.annotations.push(annotation.clone());
        let before = doc.clone();

        let mut e = Editor::new(doc);
        let delete = Op::delete_block(e.document(), 1);
        e.apply_edit(delete);
        assert_eq!(e.document().blocks.len(), 1);
        assert!(e.document().annotations.is_empty());

        e.undo().expect("delete to be undoable");
        assert_eq!(e.document().blocks.len(), 2);
        assert_eq!(e.document().blocks[1], second);
        assert_eq!(e.document().annotations, vec![annotation.clone()]);
        assert_eq!(e.document(), &before, "undo restores the exact prior document");

        e.redo().expect("delete to be redoable");
        assert_eq!(e.document().blocks.len(), 1);
        assert!(e.document().annotations.is_empty());
    }

    /// FIX 2: a hand-built `DeleteBlock` that forgot its payload is repaired at
    /// the `apply_edit` chokepoint instead of silently dropping the notes.
    #[test]
    fn apply_edit_fills_a_missing_delete_block_payload() {
        let mut doc = doc_with("first");
        let second = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "second".to_string(),
            spans: Vec::new(),
        };
        doc.blocks.push(second.clone());
        let annotation = note(second.id, 0, 6);
        doc.annotations.push(annotation.clone());

        let mut e = Editor::new(doc);
        let applied = e.apply_edit(Op::DeleteBlock {
            index: 1,
            block: second.clone(),
            annotations: Vec::new(),
        });
        assert_eq!(
            applied,
            Op::DeleteBlock {
                index: 1,
                block: second.clone(),
                annotations: vec![annotation.clone()],
            },
            "apply_edit must return the repaired op so the log can undo it"
        );
        assert!(e.document().annotations.is_empty());

        e.undo().expect("repaired delete to be undoable");
        assert_eq!(e.document().annotations, vec![annotation]);
    }

    /// FIX 2, the other half: a payload that is present but wrong is an error,
    /// not something to paper over.
    #[test]
    #[should_panic(expected = "payload is missing 1 annotation")]
    fn apply_edit_rejects_a_delete_block_with_a_partial_payload() {
        let mut doc = doc_with("first");
        let id = doc.blocks[0].id;
        let carried = note(id, 0, 5);
        doc.annotations.push(carried.clone());
        doc.annotations.push(note(id, 1, 3)); // a second note the payload omits
        let block = doc.blocks[0].clone();
        let mut e = Editor::new(doc);
        e.apply_edit(Op::DeleteBlock {
            index: 0,
            block,
            annotations: vec![carried],
        });
    }

    /// FIX 3: undoing a text delete brings back the notes the delete dropped.
    #[test]
    fn undo_of_a_text_delete_restores_wholly_removed_notes() {
        let mut doc = doc_with("hello world");
        let id = doc.blocks[0].id;
        let inside = note(id, 6, 11);
        doc.annotations.push(inside.clone());

        let mut e = Editor::new(doc);
        let delete = Op::delete_text(e.document(), id, 5, 11);
        e.apply_edit(delete);
        assert_eq!(e.document().blocks[0].text, "hello");
        assert!(e.document().annotations.is_empty());

        e.undo().expect("delete to be undoable");
        assert_eq!(e.document().blocks[0].text, "hello world");
        assert_eq!(e.document().annotations, vec![inside]);
    }

    /// FIX 3: a hand-built text delete with no payload is filled in first.
    #[test]
    fn apply_edit_fills_a_missing_text_delete_payload() {
        let mut doc = doc_with("hello world");
        let id = doc.blocks[0].id;
        doc.annotations.push(note(id, 7, 9));

        let mut e = Editor::new(doc);
        let applied = e.apply_edit(Op::DeleteText {
            block_id: id,
            start: 5,
            end: 11,
            text: String::new(),
            removed: Vec::new(),
        });
        match &applied {
            Op::DeleteText { text, removed, .. } => {
                assert_eq!(text, " world");
                assert_eq!(removed.len(), 1);
            }
            other => panic!("expected DeleteText, got {other:?}"),
        }
    }

    /// FIX 4: undo restores the exact prior kind, heading level included.
    #[test]
    fn undo_of_set_block_kind_restores_the_heading_level() {
        let mut doc = doc_with("title");
        let id = doc.blocks[0].id;
        doc.blocks[0].kind = BlockKind::Heading { level: 2 };

        let mut e = Editor::new(doc);
        let change = Op::set_block_kind(e.document(), id, BlockKind::Paragraph);
        e.apply_edit(change);
        assert_eq!(e.document().blocks[0].kind, BlockKind::Paragraph);

        e.undo().expect("kind change to be undoable");
        assert_eq!(e.document().blocks[0].kind, BlockKind::Heading { level: 2 });
    }

    /// History survives a close/reopen: undo after reloading the head document,
    /// the op log and the cursor must produce the same state as undoing in
    /// session.
    #[test]
    fn undo_after_reload_matches_in_session_undo() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();

        let doc = doc_with("hello");
        let block_id = doc.blocks[0].id;
        db.save_document(&doc).unwrap();

        let mut e = Editor::new(doc);
        for op in [
            Op::InsertText {
                block_id,
                offset: 5,
                text: " world".into(),
            },
            Op::InsertText {
                block_id,
                offset: 0,
                text: ">> ".into(),
            },
        ] {
            let applied = e.apply_edit(op);
            db.record_edit(e.document(), &applied).unwrap();
        }
        let edited = e.document().clone();
        assert_eq!(db.cursor().unwrap(), 2);

        // Undo once, in session, and persist head + cursor together.
        e.undo().expect("in-session undo");
        db.save_state(e.document(), e.cursor()).unwrap();
        let in_session_after_undo = e.document().clone();
        assert_eq!(db.cursor().unwrap(), 1);

        // Simulate a fresh process: read the head document, the log and the
        // cursor back.
        drop(db);
        let db = Db::open(&path).unwrap();
        let head = db.load_document().unwrap();
        assert_eq!(head, in_session_after_undo);
        let log: Vec<Op> = db.load_ops().unwrap().into_iter().map(|(_, op)| op).collect();
        assert_eq!(log.len(), 2, "the undone op stays in the log, as redo");
        assert_eq!(edited.blocks[0].text, ">> hello world");

        let mut reopened = Editor::with_cursor(head, log, db.cursor().unwrap());
        assert_eq!(reopened.history(), (1, 1), "one op applied, one undone");
        assert_eq!(reopened.cursor(), 1);
        assert_eq!(reopened.document(), &in_session_after_undo);

        // The undone op is redo, not a second undo.
        reopened.redo().expect("the undone op is redoable");
        assert_eq!(reopened.document(), &edited);
        reopened.undo().expect("undo the redone op");
        assert_eq!(reopened.document(), &in_session_after_undo);

        // A further undo after the reload matches the same further undo in
        // session — the double-undo corruption this fix is about.
        reopened.undo().expect("reloaded undo");
        assert_eq!(reopened.document().blocks[0].text, "hello");
        e.undo().expect("in-session undo");
        assert_eq!(reopened.document(), e.document());
        assert!(reopened.undo().is_none(), "no op left to undo");
    }

    /// The regression FIX 1 exists for: edit, undo, persist, reopen — the
    /// undone op must sit on the redo stack, not be offered for undo a second
    /// time (which corrupted the document and could panic on a second apply).
    #[test]
    fn redo_after_reopen_does_not_undo_the_undone_op_again() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();

        let doc = doc_with("ab");
        let block_id = doc.blocks[0].id;
        db.save_document(&doc).unwrap();

        let mut e = Editor::new(doc);
        for op in [
            Op::InsertText {
                block_id,
                offset: 2,
                text: "cd".into(),
            },
            Op::InsertText {
                block_id,
                offset: 4,
                text: "ef".into(),
            },
        ] {
            let applied = e.apply_edit(op);
            db.record_edit(e.document(), &applied).unwrap();
        }
        e.undo().expect("undo the second edit");
        db.save_state(e.document(), e.cursor()).unwrap();
        let after_first_undo = e.document().clone();
        assert_eq!(after_first_undo.blocks[0].text, "abcd");

        // Second undo, in session: takes the document back to "ab".
        e.undo().expect("undo the first edit");
        db.save_state(e.document(), e.cursor()).unwrap();
        let after_second_undo = e.document().clone();
        assert_eq!(after_second_undo.blocks[0].text, "ab");

        // Fresh process at the same point: the cursor says one op is applied,
        // so the first undo reproduces the in-session first undo, not a repeat
        // of an op already undone.
        drop(db);
        let db = Db::open(&path).unwrap();
        assert_eq!(db.cursor().unwrap(), 0);
        let log: Vec<Op> = db.load_ops().unwrap().into_iter().map(|(_, op)| op).collect();
        let mut reopened =
            Editor::with_cursor(db.load_document().unwrap(), log, db.cursor().unwrap());
        assert_eq!(
            reopened.document(),
            &after_second_undo,
            "head and cursor agree"
        );
        assert_eq!(reopened.history(), (0, 2));
        assert!(reopened.undo().is_none(), "nothing left to undo");

        // Redo walks forward through the same states, in order.
        reopened.redo().expect("first redo");
        assert_eq!(reopened.document(), &after_first_undo);
        reopened.redo().expect("second redo");
        assert_eq!(reopened.document().blocks[0].text, "abcdef");
    }

    /// A new edit after a reopen-at-an-earlier-cursor replaces the redo tail:
    /// `record_edit` truncates it in the same transaction that appends.
    #[test]
    fn record_edit_after_undo_replaces_the_redo_tail() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();

        let doc = doc_with("ab");
        let block_id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        for op in [
            Op::InsertText {
                block_id,
                offset: 2,
                text: "cd".into(),
            },
            Op::InsertText {
                block_id,
                offset: 0,
                text: ">> ".into(),
            },
        ] {
            let applied = e.apply_edit(op);
            db.record_edit(e.document(), &applied).unwrap();
        }
        e.undo().expect("undo");
        db.save_state(e.document(), e.cursor()).unwrap();

        let applied = e.apply_edit(Op::InsertText {
            block_id,
            offset: 2,
            text: "!".into(),
        });
        db.record_edit(e.document(), &applied).unwrap();

        let log = db.load_ops().unwrap();
        assert_eq!(log.len(), 2, "the undone op must be gone from the log");
        assert_eq!(log[1].1, applied);
        assert_eq!(db.cursor().unwrap(), 2);

        let mut reopened = Editor::with_cursor(
            db.load_document().unwrap(),
            log.into_iter().map(|(_, op)| op).collect(),
            db.cursor().unwrap(),
        );
        assert_eq!(reopened.document().blocks[0].text, "ab!cd");
        assert_eq!(reopened.history(), (2, 0));
        reopened.undo().expect("undo the replacement edit");
        assert_eq!(reopened.document().blocks[0].text, "abcd");

        // Same state the in-session editor reaches.
        e.undo().expect("in-session undo of the replacement edit");
        assert_eq!(reopened.document(), e.document());
    }
}
