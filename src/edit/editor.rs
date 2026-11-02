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

use std::fmt;

use uuid::Uuid;

use crate::edit::ops::{anchor_wholly_inside, apply, apply_inverse, invert, normalize, Op};
use crate::model::{
    Anchor, Annotation, AnnotationId, AnnotationKind, Block, BlockId, BlockKind, Document,
};

/// A caret position: the block it sits in and a **byte** offset into that
/// block's text.
///
/// Offsets are the same unit as [`Anchor`]: `str` indices, so `offset` must be
/// a character boundary. A caret at `offset == text.len()` sits after the last
/// character, which is where a caret at the end of a block lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub block_id: BlockId,
    pub offset: usize,
}

/// A range between two carets, either end first.
///
/// The commands that take a `Selection` normalize the direction (document
/// order), so a dragged-back selection works as well as a dragged-forward one.
/// The two carets may sit in different blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub start: Cursor,
    pub end: Cursor,
}

/// Why a caret or selection could not be turned into an edit.
///
/// A failed command leaves the document and the undo history untouched: the
/// checks run before anything is built or applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    /// The cursor names a block id the document does not hold.
    BlockNotFound,
    /// The offset is past the block's text, or inside a multi-byte character.
    OffsetOutOfRange,
    /// [`Editor::merge_with_next`] was called on the last block.
    NoNextBlock,
    /// [`Editor::delete_selection`] was given a zero-width selection.
    EmptySelection,
    /// The annotation id names no note in the document.
    AnnotationNotFound,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::BlockNotFound => write!(f, "block not found"),
            EditError::OffsetOutOfRange => write!(f, "offset out of range"),
            EditError::NoNextBlock => write!(f, "no block follows this one"),
            EditError::EmptySelection => write!(f, "selection is empty"),
            EditError::AnnotationNotFound => write!(f, "annotation not found"),
        }
    }
}

impl std::error::Error for EditError {}

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
    #[cfg(test)]
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

    /// Inserts `text` at `cursor`, returning the op that was applied.
    ///
    /// The caret's offset is validated first: an unknown block is
    /// [`EditError::BlockNotFound`], an offset past the text or inside a
    /// multi-byte character is [`EditError::OffsetOutOfRange`]. A rejected
    /// command leaves the document and the history untouched.
    pub fn insert_text(&mut self, cursor: &Cursor, text: &str) -> Result<Op, EditError> {
        self.check_cursor(*cursor)?;
        let op = Op::InsertText {
            block_id: cursor.block_id,
            offset: cursor.offset,
            text: text.to_string(),
        };
        Ok(self.apply_edit(op))
    }

    /// Deletes the text covered by `sel`, returning the op that was applied.
    ///
    /// A selection may run backwards (the two carets are put in document
    /// order) and may span blocks: the selected tail of the first block, every
    /// block wholly inside the range, and the selected head of the last block
    /// are removed. A boundary block the selection covers completely is removed
    /// with the rest, and deleting the whole document leaves one empty
    /// paragraph to type into.
    ///
    /// v1 removes the interior blocks the selection covers completely and
    /// trims the boundary blocks, but it does not merge them: joining the
    /// first and last blocks is deferred to v2.
    ///
    /// A zero-width selection is [`EditError::EmptySelection`]: callers that
    /// delete a character at a caret build the one-character selection.
    /// A selection that spans blocks yet covers nothing (the end of one block
    /// to the start of the next) is also [`EditError::EmptySelection`] — it
    /// would otherwise apply an empty batch and add a no-op undo step.
    pub fn delete_selection(&mut self, sel: &Selection) -> Result<Op, EditError> {
        self.check_cursor(sel.start)?;
        self.check_cursor(sel.end)?;
        let (start, end) = self.ordered(sel)?;
        if start == end {
            return Err(EditError::EmptySelection);
        }
        let start_index = self.block_index(start.block_id)?;
        let end_index = self.block_index(end.block_id)?;

        if start_index == end_index {
            let op = Op::delete_text(&self.doc, start.block_id, start.offset, end.offset);
            return Ok(self.apply_edit(op));
        }

        // Cross-block: everything is built against the document as it stands
        // now. The end block is trimmed first (it does not move), the covered
        // blocks are deleted highest index first so the low indices stay
        // valid, and the start block is trimmed last (by id, so its index
        // never matters).
        let start_len = self.block(start.block_id)?.text.len();
        let end_len = self.block(end.block_id)?.text.len();

        let mut ops = Vec::new();
        if end.offset > 0 && end.offset < end_len {
            ops.push(Op::delete_text(&self.doc, end.block_id, 0, end.offset));
        }

        // Block indices covered entirely by the selection, lowest first. A
        // block is wholly covered at the start only when the selection begins
        // at its first character, and at the end only when it runs to the last.
        let mut doomed: Vec<usize> = Vec::new();
        if start.offset == 0 {
            doomed.push(start_index);
        }
        doomed.extend(start_index + 1..end_index);
        if end.offset == end_len && end.offset > 0 {
            doomed.push(end_index);
        }
        for index in doomed.iter().rev() {
            ops.push(Op::delete_block(&self.doc, *index));
        }

        if start.offset > 0 && start.offset < start_len {
            ops.push(Op::delete_text(
                &self.doc,
                start.block_id,
                start.offset,
                start_len,
            ));
        }

        // Every block gone would leave a document with nowhere to put a caret.
        if doomed.len() == self.doc.blocks.len() {
            ops.push(Op::InsertBlock {
                index: 0,
                block: Block {
                    id: Uuid::new_v4(),
                    kind: BlockKind::Paragraph,
                    text: String::new(),
                    spans: Vec::new(),
                },
            });
        }

        // A boundary-only selection (end of one block to the start of the next)
        // covers no text and no block: every branch above skipped. Applying the
        // empty batch would record a history step that changes nothing, so a
        // no-op selection is rejected exactly like a zero-width one.
        if ops.is_empty() {
            return Err(EditError::EmptySelection);
        }
        Ok(self.apply_edit(Op::Batch { ops }))
    }

    /// Splits the block under `cursor` at its offset, moving the trailing text
    /// into a new block inserted after it, and returns the op that was applied.
    ///
    /// Notes anchored wholly in the moved text follow it into the new block; a
    /// note that straddles the split stays with the leading block. This holds
    /// for zero-width point anchors too (a marginal added at the end of a line
    /// anchors to a single offset): a point at or after the split follows the
    /// text it sits on. The new block is a paragraph when a heading is split
    /// (so the next line is body text) and keeps the source kind otherwise (a
    /// list item stays a list item).
    ///
    /// Splitting at the end of a block appends an empty block.
    ///
    /// # Limitation: inline spans
    ///
    /// The op set does not rebase [`Span`](crate::model::Span) ranges on text
    /// edits yet, so the moved text loses its bold/italic ranges. Layout
    /// clamps stale spans, so nothing breaks; a later styling task has to
    /// rebase spans for both this command and [`Editor::delete_selection`].
    pub fn split_paragraph(&mut self, cursor: &Cursor) -> Result<Op, EditError> {
        let index = self.block_index(cursor.block_id)?;
        let block = self.block(cursor.block_id)?;
        self.check_offset(block, cursor.offset)?;
        let text = block.text.clone();
        // Enter on an empty list item exits the list: the item becomes body
        // text instead of spawning another empty bullet.
        if text.is_empty()
            && matches!(
                &block.kind,
                BlockKind::ListItem { .. } | BlockKind::BlockQuote
            )
        {
            return self.set_block_kind(cursor.block_id, BlockKind::Paragraph);
        }
        let kind = split_kind(&block.kind);

        // Exactly the range-scoped notes the tail delete below will drop:
        // anchors lying wholly in the trailing text. The predicate must match
        // `anchor_wholly_inside` so a boundary point is not both left on the
        // leading block and re-added to the new one.
        let tail_notes: Vec<Annotation> = self
            .doc
            .annotations
            .iter()
            .filter(|annotation| {
                let Some((anchor_start, anchor_end)) = annotation.anchor.byte_range() else {
                    return false; // block-scoped anchors are moved separately below
                };
                annotation.anchor.block_id() == cursor.block_id
                    && anchor_wholly_inside(anchor_start, anchor_end, cursor.offset, text.len())
            })
            .cloned()
            .collect();

        // Block-scoped anchors (marginalia) follow the half that kept the text:
        // a split at offset 0 empties the leading block, so they move to the
        // trailing block; otherwise they already sit on the leading block and
        // stay put.
        let block_notes: Vec<Annotation> = if cursor.offset == 0 {
            self.doc
                .annotations
                .iter()
                .filter(|annotation| {
                    annotation.anchor.byte_range().is_none()
                        && annotation.anchor.block_id() == cursor.block_id
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        let new_block = Block {
            id: Uuid::new_v4(),
            kind,
            text: text[cursor.offset..].to_string(),
            spans: Vec::new(),
        };

        let mut ops = Vec::new();
        if cursor.offset < text.len() {
            ops.push(Op::delete_text(
                &self.doc,
                cursor.block_id,
                cursor.offset,
                text.len(),
            ));
        }
        ops.push(Op::InsertBlock {
            index: index + 1,
            block: new_block.clone(),
        });
        // The tail delete dropped these notes into its payload; re-anchor them
        // in the new block so they follow the text they belong to. Undo puts
        // the originals back from that payload (after the copies are removed).
        for annotation in tail_notes {
            let mut moved = annotation;
            let (start, end) = moved
                .anchor
                .byte_range()
                .expect("tail note is range-scoped");
            moved.anchor = Anchor::range(new_block.id, start - cursor.offset, end - cursor.offset);
            ops.push(Op::AddAnnotation { annotation: moved });
        }
        // Move block-scoped anchors off the emptied leading block onto the
        // trailing block (remove + re-add keeps the op log invertible).
        for annotation in block_notes {
            let mut moved = annotation.clone();
            moved.anchor = Anchor::block(new_block.id);
            ops.push(Op::RemoveAnnotation { annotation });
            ops.push(Op::AddAnnotation { annotation: moved });
        }
        Ok(self.apply_edit(Op::Batch { ops }))
    }

    /// Merges the block after `cursor`'s block into it, returning the op that
    /// was applied: the next block's text is appended and its notes move with
    /// it.
    ///
    /// The last block has nothing to merge into and is
    /// [`EditError::NoNextBlock`].
    pub fn merge_with_next(&mut self, cursor: &Cursor) -> Result<Op, EditError> {
        let index = self.block_index(cursor.block_id)?;
        let block = self.block(cursor.block_id)?;
        self.check_offset(block, cursor.offset)?;
        let join = block.text.len();
        let Some(next) = self.doc.blocks.get(index + 1).cloned() else {
            return Err(EditError::NoNextBlock);
        };

        let next_notes: Vec<Annotation> = self
            .doc
            .annotations
            .iter()
            .filter(|annotation| annotation.anchor.block_id() == next.id)
            .cloned()
            .collect();

        // Delete first: it captures the removed block's notes in its payload,
        // so undo restores both. Appending then puts the text where the block
        // was, and the notes are re-anchored over it.
        let mut ops = vec![
            Op::delete_block(&self.doc, index + 1),
            Op::InsertText {
                block_id: cursor.block_id,
                offset: join,
                text: next.text.clone(),
            },
        ];
        for annotation in next_notes {
            let mut moved = annotation;
            moved.anchor = match moved.anchor {
                Anchor::Block(_) => Anchor::block(cursor.block_id),
                Anchor::Range { start, end, .. } => {
                    Anchor::range(cursor.block_id, start + join, end + join)
                }
            };
            ops.push(Op::AddAnnotation { annotation: moved });
        }
        Ok(self.apply_edit(Op::Batch { ops }))
    }

    /// Changes `block_id`'s kind, returning the op that was applied.
    ///
    /// The previous kind travels in the op, so undo restores it exactly
    /// (including a heading's level).
    pub fn set_block_kind(&mut self, block_id: BlockId, kind: BlockKind) -> Result<Op, EditError> {
        self.block_index(block_id)?;
        let op = Op::set_block_kind(&self.doc, block_id, kind);
        Ok(self.apply_edit(op))
    }

    /// Applies a markdown-style prefix at the start of `block_id`'s paragraph
    /// (see [`markdown_prefix`]): strips the prefix and changes the kind in one
    /// atomic op. Returns the applied op, or `None` when there is nothing to
    /// apply.
    pub fn apply_markdown_prefix(&mut self, block_id: BlockId) -> Option<Op> {
        let (kind, prefix_len) = {
            let block = self.block(block_id).ok()?;
            markdown_prefix(&block.text, &block.kind)?
        };
        let ops = vec![
            Op::set_block_kind(&self.doc, block_id, kind),
            Op::delete_text(&self.doc, block_id, 0, prefix_len),
        ];
        Some(self.apply_edit(Op::Batch { ops }))
    }

    /// Anchors a new note of `kind` over `anchor`, returning the op that was
    /// applied. The note's id is fresh, so the returned op can be persisted as
    /// is.
    ///
    /// The anchor is validated like a caret: an unknown block is
    /// [`EditError::BlockNotFound`], a reversed or out-of-range span is
    /// [`EditError::OffsetOutOfRange`].
    pub fn add_annotation(
        &mut self,
        kind: AnnotationKind,
        anchor: Anchor,
        body: &str,
    ) -> Result<Op, EditError> {
        let block = self.block(anchor.block_id())?;
        if let Some((start, end)) = anchor.byte_range() {
            self.check_offset(block, start)?;
            self.check_offset(block, end)?;
            if start > end {
                return Err(EditError::OffsetOutOfRange);
            }
        }
        let annotation = Annotation {
            id: Uuid::new_v4(),
            kind,
            anchor,
            body: body.to_string(),
        };
        Ok(self.apply_edit(Op::AddAnnotation { annotation }))
    }

    /// Replaces the body of the note `annotation_id`, returning the op that was
    /// applied.
    ///
    /// The previous body travels in the op, so undo restores the exact prose.
    /// An unknown id is [`EditError::AnnotationNotFound`]; a rejected call
    /// leaves the document and the history untouched.
    ///
    /// A body that is already `new_body` still produces an op — the caller owns
    /// the choice of whether that is an edit worth logging (the GUI compares
    /// before calling, so per-keystroke commits only record real changes).
    pub fn set_annotation_body(
        &mut self,
        annotation_id: AnnotationId,
        new_body: &str,
    ) -> Result<Op, EditError> {
        let old_body = self
            .doc
            .annotations
            .iter()
            .find(|annotation| annotation.id == annotation_id)
            .map(|annotation| annotation.body.clone())
            .ok_or(EditError::AnnotationNotFound)?;
        let op = Op::SetAnnotationBody {
            annotation_id,
            old_body,
            new_body: new_body.to_string(),
        };
        Ok(self.apply_edit(op))
    }

    /// Removes the annotation `annotation_id`, returning the op that was
    /// applied. The removed annotation travels in the op, so undo restores it.
    /// An unknown id is [`EditError::AnnotationNotFound`].
    pub fn remove_annotation(&mut self, annotation_id: AnnotationId) -> Result<Op, EditError> {
        let annotation = self
            .doc
            .annotations
            .iter()
            .find(|annotation| annotation.id == annotation_id)
            .cloned()
            .ok_or(EditError::AnnotationNotFound)?;
        Ok(self.apply_edit(Op::RemoveAnnotation { annotation }))
    }

    // --- caret helpers -----------------------------------------------------

    fn block(&self, block_id: BlockId) -> Result<&Block, EditError> {
        self.doc
            .blocks
            .iter()
            .find(|block| block.id == block_id)
            .ok_or(EditError::BlockNotFound)
    }

    fn block_index(&self, block_id: BlockId) -> Result<usize, EditError> {
        self.doc
            .blocks
            .iter()
            .position(|block| block.id == block_id)
            .ok_or(EditError::BlockNotFound)
    }

    fn check_offset(&self, block: &Block, offset: usize) -> Result<(), EditError> {
        if offset > block.text.len() || !block.text.is_char_boundary(offset) {
            return Err(EditError::OffsetOutOfRange);
        }
        Ok(())
    }

    fn check_cursor(&self, cursor: Cursor) -> Result<(), EditError> {
        let block = self.block(cursor.block_id)?;
        self.check_offset(block, cursor.offset)
    }

    /// The selection's two carets in document order: by block index, then
    /// offset. Both carets must already be valid.
    fn ordered(&self, sel: &Selection) -> Result<(Cursor, Cursor), EditError> {
        let start = self.block_index(sel.start.block_id)?;
        let end = self.block_index(sel.end.block_id)?;
        if (start, sel.start.offset) <= (end, sel.end.offset) {
            Ok((sel.start, sel.end))
        } else {
            Ok((sel.end, sel.start))
        }
    }
}

/// The kind of block a split creates: a heading's successor is body text, and
/// everything else keeps its kind (paragraphs stay paragraphs, list items stay
/// list items).
fn split_kind(kind: &BlockKind) -> BlockKind {
    match kind {
        BlockKind::Heading { .. } => BlockKind::Paragraph,
        other => other.clone(),
    }
}

/// Recognizes a markdown-style prefix at the start of a paragraph and returns
/// the block kind it selects plus the byte length of the prefix to strip.
/// `# `, `## ` and `### ` become headings; `- `/`* ` start a bullet, `1. ` an
/// ordered item and `> ` a block quote. A block that already has a kind is left
/// alone.
fn markdown_prefix(text: &str, kind: &BlockKind) -> Option<(BlockKind, usize)> {
    if !matches!(kind, BlockKind::Paragraph) {
        return None;
    }
    for (marker, level) in [("### ", 3), ("## ", 2), ("# ", 1)] {
        if text.starts_with(marker) {
            return Some((BlockKind::Heading { level }, marker.len()));
        }
    }
    if text.starts_with("- ") || text.starts_with("* ") {
        return Some((BlockKind::ListItem { ordered: false }, 2));
    }
    if text.starts_with("1. ") {
        return Some((BlockKind::ListItem { ordered: true }, 3));
    }
    if text.starts_with("> ") {
        return Some((BlockKind::BlockQuote, 2));
    }
    None
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
            anchor: Anchor::range(block_id, start, end),
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
        assert_eq!(
            e.document(),
            &before,
            "undo restores the exact prior document"
        );

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
        let log: Vec<Op> = db
            .load_ops()
            .unwrap()
            .into_iter()
            .map(|(_, op)| op)
            .collect();
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
        let log: Vec<Op> = db
            .load_ops()
            .unwrap()
            .into_iter()
            .map(|(_, op)| op)
            .collect();
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

    // --- Task 9: caret, selection, and block-editing commands ---------------

    fn paragraph(id: Uuid, text: &str) -> Block {
        Block {
            id,
            kind: BlockKind::Paragraph,
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    #[test]
    fn insert_text_command_inserts_at_the_caret_and_returns_the_op() {
        let doc = doc_with("helo");
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        let op = e
            .insert_text(
                &Cursor {
                    block_id: id,
                    offset: 2,
                },
                "l",
            )
            .expect("insert at the caret");
        assert_eq!(e.document().blocks[0].text, "hello");
        assert_eq!(
            op,
            Op::InsertText {
                block_id: id,
                offset: 2,
                text: "l".into(),
            }
        );
        assert_eq!(e.history(), (1, 0));
    }

    #[test]
    fn insert_text_command_rejects_bad_carets_without_editing() {
        let doc = doc_with("é"); // two bytes
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        assert_eq!(
            e.insert_text(
                &Cursor {
                    block_id: id,
                    offset: 3,
                },
                "x"
            ),
            Err(EditError::OffsetOutOfRange)
        );
        assert_eq!(
            e.insert_text(
                &Cursor {
                    block_id: id,
                    offset: 1, // inside the two-byte character
                },
                "x"
            ),
            Err(EditError::OffsetOutOfRange)
        );
        assert_eq!(
            e.insert_text(
                &Cursor {
                    block_id: Uuid::new_v4(),
                    offset: 0,
                },
                "x"
            ),
            Err(EditError::BlockNotFound)
        );
        assert_eq!(
            e.document().blocks[0].text,
            "é",
            "a rejected command edits nothing"
        );
        assert_eq!(e.history(), (0, 0), "a rejected command logs nothing");
    }

    #[test]
    fn split_paragraph_moves_trailing_text_into_a_new_block() {
        let doc = doc_with("one two");
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        e.split_paragraph(&Cursor {
            block_id: id,
            offset: 3,
        })
        .expect("split at the caret");

        assert_eq!(e.document().blocks.len(), 2);
        assert_eq!(e.document().blocks[0].id, id);
        assert_eq!(e.document().blocks[0].text, "one");
        assert_eq!(e.document().blocks[1].text, " two");
        assert_ne!(
            e.document().blocks[1].id,
            id,
            "the new block gets its own id"
        );
        assert_eq!(e.history(), (1, 0), "a split is one undo step");

        e.undo().expect("split to be undoable");
        assert_eq!(e.document().blocks.len(), 1);
        assert_eq!(e.document().blocks[0].text, "one two");
    }

    #[test]
    fn split_paragraph_moves_notes_on_the_trailing_text() {
        let mut doc = doc_with("one two");
        let id = doc.blocks[0].id;
        doc.annotations.push(note(id, 4, 7)); // "two"
        let before = doc.clone();

        let mut e = Editor::new(doc);
        e.split_paragraph(&Cursor {
            block_id: id,
            offset: 3,
        })
        .expect("split at the caret");

        let new_id = e.document().blocks[1].id;
        assert_eq!(e.document().annotations.len(), 1);
        assert_eq!(
            e.document().annotations[0].anchor,
            Anchor::range(new_id, 1, 4),
            "the note follows its text into the new block"
        );

        e.undo().expect("split to be undoable");
        assert_eq!(e.document(), &before, "undo restores block, text and note");
    }

    #[test]
    fn split_paragraph_at_the_end_appends_an_empty_block() {
        let doc = doc_with("end");
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        e.split_paragraph(&Cursor {
            block_id: id,
            offset: 3,
        })
        .expect("split at the end");
        assert_eq!(e.document().blocks.len(), 2);
        assert_eq!(e.document().blocks[0].text, "end");
        assert_eq!(e.document().blocks[1].text, "");
    }

    #[test]
    fn enter_on_an_empty_list_item_turns_it_into_body_text() {
        let mut doc = doc_with("");
        doc.blocks[0].kind = BlockKind::ListItem { ordered: false };
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        e.split_paragraph(&Cursor {
            block_id: id,
            offset: 0,
        })
        .expect("split an empty list item");

        // No new block: the empty bullet becomes a body paragraph.
        assert_eq!(e.document().blocks.len(), 1);
        assert_eq!(e.document().blocks[0].kind, BlockKind::Paragraph);
        assert_eq!(e.document().blocks[0].text, "");

        e.undo().expect("undo the exit");
        assert_eq!(
            e.document().blocks[0].kind,
            BlockKind::ListItem { ordered: false }
        );
    }

    #[test]
    fn markdown_prefix_recognizes_headings_and_lists() {
        let paragraph = BlockKind::Paragraph;
        assert_eq!(
            markdown_prefix("## Hi", &paragraph),
            Some((BlockKind::Heading { level: 2 }, 3))
        );
        assert_eq!(
            markdown_prefix("# Hi", &paragraph),
            Some((BlockKind::Heading { level: 1 }, 2))
        );
        assert_eq!(
            markdown_prefix("### Hi", &paragraph),
            Some((BlockKind::Heading { level: 3 }, 4))
        );
        assert_eq!(
            markdown_prefix("- Hi", &paragraph),
            Some((BlockKind::ListItem { ordered: false }, 2))
        );
        assert_eq!(
            markdown_prefix("1. Hi", &paragraph),
            Some((BlockKind::ListItem { ordered: true }, 3))
        );
        assert_eq!(
            markdown_prefix("> A quote", &paragraph),
            Some((BlockKind::BlockQuote, 2))
        );
        // No trailing space, or a block that already has a kind: nothing.
        assert_eq!(markdown_prefix("##Hi", &paragraph), None);
        assert_eq!(
            markdown_prefix("## Hi", &BlockKind::Heading { level: 1 }),
            None
        );
    }

    #[test]
    fn apply_markdown_prefix_converts_and_strips_in_one_undo_step() {
        let doc = doc_with("## Section");
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        e.apply_markdown_prefix(id).expect("a prefix to apply");
        let block = &e.document().blocks[0];
        assert_eq!(block.kind, BlockKind::Heading { level: 2 });
        assert_eq!(block.text, "Section");

        e.undo().expect("the conversion is one undo step");
        let block = &e.document().blocks[0];
        assert_eq!(block.kind, BlockKind::Paragraph);
        assert_eq!(block.text, "## Section");
    }

    #[test]
    fn split_moves_point_anchored_marginalia_with_the_text() {
        // A marginal added at the end of a sentence (caret after the period)
        // anchors to a single offset (start == end). It must follow the text
        // through a split rather than collapse onto an empty leading block.
        let mut doc = doc_with("This is my paragraph");
        let id = doc.blocks[0].id;
        doc.annotations.push(note(id, 11, 20)); // range marginal on "paragraph"
        doc.annotations.push(note(id, 13, 13)); // point marginal at the end

        let mut e = Editor::new(doc);
        let mut cursor = Cursor {
            block_id: id,
            offset: 0,
        };
        e.split_paragraph(&cursor).unwrap();
        cursor = Cursor {
            block_id: e.document().blocks[1].id,
            offset: 0,
        };
        e.split_paragraph(&cursor).unwrap();

        let final_block = e.document().blocks[2].id;
        assert_eq!(e.document().annotations.len(), 2, "no marginal is dropped");
        for a in &e.document().annotations {
            assert_eq!(
                a.anchor.block_id(),
                final_block,
                "every marginal follows the text, none lingers on an empty block"
            );
        }
        assert!(e
            .document()
            .annotations
            .iter()
            .any(|a| { a.anchor == Anchor::range(final_block, 11, 20) }));
        assert!(e
            .document()
            .annotations
            .iter()
            .any(|a| { a.anchor == Anchor::range(final_block, 13, 13) }));

        e.undo().unwrap();
        e.undo().unwrap();
        assert_eq!(e.document().blocks.len(), 1);
        assert_eq!(e.document().blocks[0].text, "This is my paragraph");
        assert_eq!(e.document().annotations.len(), 2);
    }

    /// Deleting the character before a zero-width (point) anchor re-anchors it
    /// instead of dropping it, and undo/redo stay consistent.
    #[test]
    fn deleting_before_a_point_anchor_reanchors_it_across_undo_and_redo() {
        let mut doc = doc_with("ab c");
        let id = doc.blocks[0].id;
        let point = note(id, 3, 3); // at the start of "c", after the space
        doc.annotations.push(point.clone());

        let mut e = Editor::new(doc);
        // Backspace the space [2, 3) before the point at 3.
        let sel = Selection {
            start: Cursor {
                block_id: id,
                offset: 3,
            },
            end: Cursor {
                block_id: id,
                offset: 2,
            },
        };
        e.delete_selection(&sel).unwrap();
        assert_eq!(e.document().blocks[0].text, "abc");
        assert_eq!(e.document().annotations.len(), 1, "the point survives");
        assert_eq!(
            e.document().annotations[0].anchor.byte_range(),
            Some((2, 2)),
            "the point closes up by one"
        );

        e.undo().unwrap();
        assert_eq!(e.document().blocks[0].text, "ab c");
        assert!(e.document().annotations.contains(&point));

        e.redo().unwrap();
        assert_eq!(e.document().blocks[0].text, "abc");
        assert_eq!(
            e.document().annotations[0].anchor.byte_range(),
            Some((2, 2))
        );
    }

    #[test]
    fn split_moves_block_scoped_marginalia_with_the_content() {
        // A marginal anchors to its whole block. Splitting at the very start
        // empties the leading block, so the marginal follows the content into
        // the new block; a split in the middle keeps it on the leading block.
        let mut doc = doc_with("This is my paragraph");
        let id = doc.blocks[0].id;
        doc.annotations.push(Annotation {
            id: Uuid::new_v4(),
            kind: AnnotationKind::Marginal,
            anchor: Anchor::block(id),
            body: "marginal".to_string(),
        });

        let mut e = Editor::new(doc);
        // Split at the start: leading block empties, marginal moves to the new block.
        e.split_paragraph(&Cursor {
            block_id: id,
            offset: 0,
        })
        .unwrap();
        let new_block = e.document().blocks[1].id;
        assert_eq!(e.document().annotations.len(), 1);
        assert_eq!(
            e.document().annotations[0].anchor,
            Anchor::block(new_block),
            "the marginal follows the content into the new block"
        );

        // Split in the middle of that block: the marginal stays on the leading half.
        e.split_paragraph(&Cursor {
            block_id: new_block,
            offset: 4,
        })
        .unwrap();
        assert_eq!(e.document().annotations.len(), 1);
        assert_eq!(
            e.document().annotations[0].anchor,
            Anchor::block(new_block),
            "a mid-block split keeps the marginal on the leading half"
        );

        e.undo().unwrap();
        e.undo().unwrap();
        assert_eq!(e.document().blocks.len(), 1);
        assert_eq!(e.document().blocks[0].text, "This is my paragraph");
        assert_eq!(e.document().annotations[0].anchor, Anchor::block(id));
    }

    #[test]
    fn merge_with_next_concatenates_and_undo_restores() {
        let mut doc = doc_with("one");
        let first = doc.blocks[0].id;
        doc.blocks.push(paragraph(Uuid::new_v4(), "two"));
        let second = doc.blocks[1].id;
        doc.annotations.push(note(second, 0, 3));
        let before = doc.clone();

        let mut e = Editor::new(doc);
        e.merge_with_next(&Cursor {
            block_id: first,
            offset: 3,
        })
        .expect("merge with the next block");

        assert_eq!(e.document().blocks.len(), 1);
        assert_eq!(e.document().blocks[0].text, "onetwo");
        assert_eq!(e.document().annotations.len(), 1);
        assert_eq!(
            e.document().annotations[0].anchor,
            Anchor::range(first, 3, 6),
            "the merged block's notes move with its text"
        );
        assert_eq!(e.history(), (1, 0), "a merge is one undo step");

        e.undo().expect("merge to be undoable");
        assert_eq!(
            e.document(),
            &before,
            "undo restores both blocks and the note"
        );
    }

    #[test]
    fn merge_with_next_on_the_last_block_errors() {
        let doc = doc_with("only");
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        assert_eq!(
            e.merge_with_next(&Cursor {
                block_id: id,
                offset: 4,
            }),
            Err(EditError::NoNextBlock)
        );
        assert_eq!(e.history(), (0, 0));
    }

    #[test]
    fn set_block_kind_command_changes_the_kind_and_undo_restores_it() {
        let doc = doc_with("title");
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        let op = e
            .set_block_kind(id, BlockKind::Heading { level: 2 })
            .expect("set the kind");
        assert_eq!(
            op,
            Op::SetBlockKind {
                block_id: id,
                old_kind: BlockKind::Paragraph,
                kind: BlockKind::Heading { level: 2 },
            }
        );
        assert_eq!(e.document().blocks[0].kind, BlockKind::Heading { level: 2 });

        e.undo().expect("kind change to be undoable");
        assert_eq!(e.document().blocks[0].kind, BlockKind::Paragraph);
        assert_eq!(
            e.set_block_kind(Uuid::new_v4(), BlockKind::Paragraph),
            Err(EditError::BlockNotFound)
        );
    }

    #[test]
    fn add_annotation_places_a_note_over_the_given_range() {
        let doc = doc_with("hello world");
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);
        let anchor = Anchor::range(id, 6, 11);

        let op = e
            .add_annotation(AnnotationKind::Note, anchor.clone(), "a note")
            .expect("add the note");
        match &op {
            Op::AddAnnotation { annotation } => {
                assert_eq!(annotation.kind, AnnotationKind::Note);
                assert_eq!(annotation.anchor, anchor);
                assert_eq!(annotation.body, "a note");
            }
            other => panic!("expected AddAnnotation, got {other:?}"),
        }
        assert_eq!(e.document().annotations.len(), 1);
        assert_eq!(e.document().annotations[0].anchor, anchor);

        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(
            serde_json::from_str::<Op>(&json).unwrap(),
            op,
            "the returned op must be loggable"
        );
    }

    #[test]
    fn add_annotation_rejects_a_bad_anchor_without_editing() {
        let doc = doc_with("hi");
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        assert_eq!(
            e.add_annotation(AnnotationKind::Note, Anchor::range(id, 2, 1), "x"),
            Err(EditError::OffsetOutOfRange),
            "a reversed range is not an anchor"
        );
        assert_eq!(
            e.add_annotation(AnnotationKind::Note, Anchor::range(id, 0, 3), "x"),
            Err(EditError::OffsetOutOfRange)
        );
        assert_eq!(
            e.add_annotation(
                AnnotationKind::Note,
                Anchor::range(Uuid::new_v4(), 0, 1),
                "x"
            ),
            Err(EditError::BlockNotFound)
        );
        assert_eq!(e.history(), (0, 0));
    }

    #[test]
    fn set_annotation_body_applies_and_is_undoable() {
        let mut doc = doc_with("hello world");
        let id = doc.blocks[0].id;
        let annotation = note(id, 6, 11);
        let annotation_id = annotation.id;
        doc.annotations.push(annotation);

        let mut e = Editor::new(doc);
        let op = e
            .set_annotation_body(annotation_id, "the world, considered")
            .expect("set the body");
        assert_eq!(
            op,
            Op::SetAnnotationBody {
                annotation_id,
                old_body: "note".into(),
                new_body: "the world, considered".into(),
            }
        );
        assert_eq!(
            e.document().annotations[0].body,
            "the world, considered",
            "the note's body is the new prose"
        );
        assert_eq!(e.history(), (1, 0), "a body edit is one undo step");
        assert_eq!(
            serde_json::from_str::<Op>(&serde_json::to_string(&op).unwrap()).unwrap(),
            op,
            "the returned op must be loggable"
        );

        let undone = e.undo().expect("body edit to be undoable");
        assert_eq!(
            undone,
            Op::SetAnnotationBody {
                annotation_id,
                old_body: "the world, considered".into(),
                new_body: "note".into(),
            },
            "undo reports the swap"
        );
        assert_eq!(e.document().annotations[0].body, "note");

        e.redo().expect("body edit to be redoable");
        assert_eq!(e.document().annotations[0].body, "the world, considered");
    }

    #[test]
    fn set_annotation_body_rejects_an_unknown_annotation_without_editing() {
        let mut e = Editor::new(doc_with("hi"));

        assert_eq!(
            e.set_annotation_body(Uuid::new_v4(), "x"),
            Err(EditError::AnnotationNotFound)
        );
        assert_eq!(e.history(), (0, 0), "a rejected call logs nothing");
    }

    #[test]
    fn delete_selection_removes_the_selected_range() {
        let doc = doc_with("hello world");
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        e.delete_selection(&Selection {
            start: Cursor {
                block_id: id,
                offset: 5,
            },
            end: Cursor {
                block_id: id,
                offset: 11,
            },
        })
        .expect("delete the selection");
        assert_eq!(e.document().blocks[0].text, "hello");
        assert_eq!(e.history(), (1, 0));

        e.undo().expect("delete to be undoable");
        assert_eq!(e.document().blocks[0].text, "hello world");
    }

    #[test]
    fn delete_selection_accepts_a_backwards_selection() {
        let doc = doc_with("hello world");
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        e.delete_selection(&Selection {
            start: Cursor {
                block_id: id,
                offset: 11,
            },
            end: Cursor {
                block_id: id,
                offset: 5,
            },
        })
        .expect("delete the backwards selection");
        assert_eq!(e.document().blocks[0].text, "hello");
    }

    #[test]
    fn delete_selection_across_blocks_drops_the_covered_blocks() {
        let mut doc = doc_with("aaa");
        let first = doc.blocks[0].id;
        doc.blocks.push(paragraph(Uuid::new_v4(), "bbb"));
        let middle = doc.blocks[1].id;
        doc.blocks.push(paragraph(Uuid::new_v4(), "ccc"));
        let last = doc.blocks[2].id;
        doc.annotations.push(note(middle, 0, 3));
        let before = doc.clone();

        let mut e = Editor::new(doc);
        e.delete_selection(&Selection {
            start: Cursor {
                block_id: first,
                offset: 1,
            },
            end: Cursor {
                block_id: last,
                offset: 2,
            },
        })
        .expect("delete the cross-block selection");

        assert_eq!(e.document().blocks.len(), 2);
        assert_eq!(e.document().blocks[0].id, first);
        assert_eq!(e.document().blocks[0].text, "a");
        assert_eq!(e.document().blocks[1].id, last);
        assert_eq!(e.document().blocks[1].text, "c");
        assert!(
            e.document().annotations.is_empty(),
            "the covered block's note goes with it"
        );
        assert_eq!(e.history(), (1, 0));

        e.undo().expect("cross-block delete to be undoable");
        assert_eq!(
            e.document(),
            &before,
            "undo restores every block and the note"
        );
    }

    #[test]
    fn delete_selection_over_whole_blocks_removes_them_and_leaves_a_paragraph() {
        let mut doc = doc_with("aaa");
        let first = doc.blocks[0].id;
        doc.blocks.push(paragraph(Uuid::new_v4(), "bbb"));
        let second = doc.blocks[1].id;
        doc.annotations.push(note(second, 0, 3));
        let before = doc.clone();

        let mut e = Editor::new(doc);
        e.delete_selection(&Selection {
            start: Cursor {
                block_id: first,
                offset: 0,
            },
            end: Cursor {
                block_id: second,
                offset: 3,
            },
        })
        .expect("delete the whole document");

        assert_eq!(
            e.document().blocks.len(),
            1,
            "one paragraph is left to type into"
        );
        assert_eq!(e.document().blocks[0].text, "");
        assert!(e.document().annotations.is_empty());

        e.undo().expect("whole-document delete to be undoable");
        assert_eq!(e.document(), &before);
    }

    #[test]
    fn delete_selection_ending_at_a_blocks_last_character_drops_that_block() {
        let mut doc = doc_with("aaa");
        let first = doc.blocks[0].id;
        doc.blocks.push(paragraph(Uuid::new_v4(), "bbb"));
        let second = doc.blocks[1].id;
        let before = doc.clone();

        let mut e = Editor::new(doc);
        e.delete_selection(&Selection {
            start: Cursor {
                block_id: first,
                offset: 1,
            },
            end: Cursor {
                block_id: second,
                offset: 3,
            },
        })
        .expect("delete to the end of the second block");

        assert_eq!(e.document().blocks.len(), 1);
        assert_eq!(e.document().blocks[0].id, first);
        assert_eq!(e.document().blocks[0].text, "a");

        e.undo().expect("delete to be undoable");
        assert_eq!(e.document(), &before);
    }

    #[test]
    fn delete_selection_rejects_an_empty_selection() {
        let doc = doc_with("abc");
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);
        let caret = Cursor {
            block_id: id,
            offset: 1,
        };

        assert_eq!(
            e.delete_selection(&Selection {
                start: caret,
                end: caret,
            }),
            Err(EditError::EmptySelection)
        );
        assert_eq!(e.document().blocks[0].text, "abc");
        assert_eq!(e.history(), (0, 0));
    }

    /// FIX 3: a selection from the end of one block to the start of the next
    /// covers no text and no block. It must be rejected, not applied as an
    /// empty batch that adds a phantom undo step.
    #[test]
    fn delete_selection_rejects_a_boundary_only_selection_without_logging_it() {
        let mut doc = doc_with("aaa");
        let first = doc.blocks[0].id;
        doc.blocks.push(paragraph(Uuid::new_v4(), "bbb"));
        let second = doc.blocks[1].id;
        let before = doc.clone();
        let mut e = Editor::new(doc);

        assert_eq!(
            e.delete_selection(&Selection {
                start: Cursor {
                    block_id: first,
                    offset: 3, // end of the first block
                },
                end: Cursor {
                    block_id: second,
                    offset: 0, // start of the next
                },
            }),
            Err(EditError::EmptySelection)
        );
        assert_eq!(e.document(), &before, "the document is unchanged");
        assert_eq!(e.history(), (0, 0), "no phantom undo step is recorded");
        assert!(e.undo().is_none(), "there is nothing to undo");
    }

    #[test]
    fn commands_validate_the_selection_before_building_an_op() {
        let doc = doc_with("abc");
        let id = doc.blocks[0].id;
        let mut e = Editor::new(doc);

        assert_eq!(
            e.delete_selection(&Selection {
                start: Cursor {
                    block_id: id,
                    offset: 0,
                },
                end: Cursor {
                    block_id: Uuid::new_v4(),
                    offset: 1,
                },
            }),
            Err(EditError::BlockNotFound)
        );
        assert_eq!(
            e.delete_selection(&Selection {
                start: Cursor {
                    block_id: id,
                    offset: 0,
                },
                end: Cursor {
                    block_id: id,
                    offset: 9,
                },
            }),
            Err(EditError::OffsetOutOfRange)
        );
        assert_eq!(e.history(), (0, 0));
    }

    /// A command's op is what the log stores, so one persisted op must rebuild
    /// the same document and undo the whole command after a reopen.
    #[test]
    fn split_round_trips_through_the_op_log() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();
        let doc = doc_with("one two");
        let id = doc.blocks[0].id;
        db.save_document(&doc).unwrap();

        let mut e = Editor::new(doc);
        let op = e
            .split_paragraph(&Cursor {
                block_id: id,
                offset: 3,
            })
            .expect("split");
        db.record_edit(e.document(), &op).unwrap();
        let edited = e.document().clone();

        drop(db);
        let db = Db::open(&path).unwrap();
        let log: Vec<Op> = db
            .load_ops()
            .unwrap()
            .into_iter()
            .map(|(_, op)| op)
            .collect();
        assert_eq!(log.len(), 1, "a split is one op in the log");
        assert_eq!(db.cursor().unwrap(), 1);

        let mut reopened =
            Editor::with_cursor(db.load_document().unwrap(), log, db.cursor().unwrap());
        assert_eq!(reopened.document(), &edited);
        reopened
            .undo()
            .expect("the split is undoable after a reopen");
        assert_eq!(reopened.document().blocks.len(), 1);
        assert_eq!(reopened.document().blocks[0].text, "one two");
        reopened.redo().expect("and redoable");
        assert_eq!(reopened.document(), &edited);
    }
}
