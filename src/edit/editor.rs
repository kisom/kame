//! The in-memory editor: a document plus a linear undo/redo history.
//!
//! History is a cursor over the durable op log (see [`crate::storage::Db`]).
//! Every edit is applied through [`Editor::apply_edit`], which returns the op so
//! the caller can append it to the log and save the resulting document. Undo and
//! redo move the cursor; a new edit after undo discards the redo tail, which
//! keeps the log linear.

use crate::edit::ops::{apply, apply_inverse, invert, Op};
use crate::model::Document;

/// A document with an undo/redo history.
///
/// Persist after every mutation: `apply_edit` returns the op to
/// `Db::append_op`, `undo`/`redo` return the op that was applied to the
/// document, and the caller saves `document()` as the head.
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
    pub fn with_history(doc: Document, ops: Vec<Op>) -> Editor {
        Editor {
            doc,
            undo: ops,
            redo: Vec::new(),
        }
    }

    /// The current document.
    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// Applies `op` to the document, records it for undo, discards the redo
    /// tail (the log stays linear), and returns it for the caller to persist.
    pub fn apply_edit(&mut self, op: Op) -> Op {
        apply(&mut self.doc, &op);
        self.undo.push(op.clone());
        self.redo.clear();
        op
    }

    /// Undoes the most recent edit, returning the inverse that was applied so
    /// the caller can persist the new head. `None` when there is nothing to
    /// undo.
    ///
    /// The undo itself goes through [`apply_inverse`], so a `DeleteBlock` is
    /// undone with its notes; the returned op is `invert(&op)` (for a block
    /// delete, the block insert alone — the notes ride in the original op).
    pub fn undo(&mut self) -> Option<Op> {
        let op = self.undo.pop()?;
        apply_inverse(&mut self.doc, &op);
        let inverse = invert(&op);
        self.redo.push(op);
        Some(inverse)
    }

    /// Redoes the most recently undone edit, returning the op that was applied.
    /// `None` when there is nothing to redo.
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
    use crate::model::{Anchor, Annotation, AnnotationKind, BlockKind};
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

        e.undo();
        assert_eq!(e.document().blocks[0].text, "");
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
        let second = crate::model::Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "second".to_string(),
            spans: Vec::new(),
        };
        doc.blocks.push(second.clone());
        let annotation = Annotation {
            id: Uuid::new_v4(),
            kind: AnnotationKind::Marginal,
            anchor: Anchor {
                block_id: second.id,
                start: 0,
                end: 6,
            },
            body: "margin note".to_string(),
        };
        doc.annotations.push(annotation.clone());
        let before = doc.clone();

        let mut e = Editor::new(doc);
        e.apply_edit(Op::DeleteBlock {
            index: 1,
            block: second.clone(),
            annotations: vec![annotation.clone()],
        });
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

    /// History survives a close/reopen: undo after reloading the head document
    /// plus the op log must produce the same state as undoing in session.
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
            db.append_op(&applied).unwrap();
            db.save_document(e.document()).unwrap();
        }
        let edited = e.document().clone();
        e.undo().expect("in-session undo");
        let in_session_after_undo = e.document().clone();

        // Simulate a fresh process: read the head document and the log back.
        drop(db);
        let db = Db::open(&path).unwrap();
        let head = db.load_document().unwrap();
        assert_eq!(head, edited);
        let log: Vec<Op> = db.load_ops().unwrap().into_iter().map(|(_, op)| op).collect();
        assert_eq!(log.len(), 2);

        let mut reopened = Editor::with_history(head, log);
        assert_eq!(reopened.history(), (2, 0));
        reopened.undo().expect("reloaded undo");
        assert_eq!(reopened.document(), &in_session_after_undo);
        assert_eq!(reopened.document().blocks[0].text, "hello world");
    }
}
