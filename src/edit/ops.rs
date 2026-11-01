//! Invertible document mutations.
//!
//! Every mutation of a [`Document`] is expressed as an [`Op`]. Each op carries
//! the material it removes — deleted text, the notes that text took with it, a
//! removed block and its notes, the previous block kind, the previous title —
//! so [`invert`] can undo it without consulting anything but the op itself.
//! That is what makes undo durable: the op log, not the saved document blob, is
//! the source of truth for history.
//!
//! All offsets are **byte** offsets into a block's text, matching
//! [`Anchor`](crate::model::Anchor) and `String` indexing.
//!
//! # Payloads are part of the contract
//!
//! A removal op's payload is not advisory. [`apply`] checks it against the
//! document and panics on a mismatch (a `DeleteBlock` without its notes would
//! otherwise destroy them silently in release builds). Build removal ops with
//! the constructors on [`Op`] — [`Op::delete_block`], [`Op::delete_text`],
//! [`Op::set_block_kind`] — or go through [`crate::edit::Editor::apply_edit`], which fills a
//! missing payload in before applying.

use serde::{Deserialize, Serialize};

use crate::model::{shift_anchors, Annotation, Block, BlockId, BlockKind, Document};

/// A single invertible mutation of a [`Document`].
///
/// Removals carry their payload (`text`, `removed`, `block`, `annotations`,
/// `old`, `old_kind`) that [`invert`] can turn into the matching insertion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Op {
    InsertText {
        block_id: BlockId,
        offset: usize,
        text: String,
    },
    DeleteText {
        block_id: BlockId,
        start: usize,
        end: usize,
        text: String,
        /// Notes whose anchor lay wholly inside `start..end`, dropped by the
        /// delete; [`apply_inverse`] re-adds them.
        ///
        /// `serde(default)` lets rows written before this field existed
        /// deserialize: they simply cannot restore those notes.
        #[serde(default)]
        removed: Vec<Annotation>,
    },
    InsertBlock {
        index: usize,
        block: Block,
    },
    DeleteBlock {
        index: usize,
        block: Block,
        annotations: Vec<Annotation>,
    },
    /// `kind` is the new kind; `old_kind` is what was there before, so
    /// [`invert`] is involutive and undo restores the exact prior kind
    /// (including a heading's level).
    SetBlockKind {
        block_id: BlockId,
        old_kind: BlockKind,
        kind: BlockKind,
    },
    AddAnnotation {
        annotation: Annotation,
    },
    RemoveAnnotation {
        annotation: Annotation,
    },
    SetTitle {
        old: String,
        new: String,
    },
    /// A group of ops applied in order as **one** history step.
    ///
    /// A command that needs more than one primitive mutation — splitting a
    /// block, merging two, deleting a selection that spans blocks — builds its
    /// primitives in order and wraps them here, so the op log still records one
    /// entry per command and one undo puts the document back.
    ///
    /// The payload rules are unchanged: each member is built from the document
    /// as it will be when that member applies (the [`Op`] constructors do
    /// this), and [`apply`] still validates every payload. [`invert`] reverses
    /// the members and inverts each; [`apply_inverse`] undoes them in reverse
    /// order so a member's removal payload is re-attached before the member
    /// before it needs the document back.
    Batch {
        ops: Vec<Op>,
    },
}

impl Op {
    /// A [`Op::DeleteBlock`] for `doc.blocks[index]` that carries every
    /// annotation anchored in the block, so undo can put them back.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of range.
    pub fn delete_block(doc: &Document, index: usize) -> Op {
        let block = doc
            .blocks
            .get(index)
            .unwrap_or_else(|| {
                panic!(
                    "DeleteBlock index {index} out of range (len {})",
                    doc.blocks.len()
                )
            })
            .clone();
        let annotations = annotations_in_block(doc, block.id);
        Op::DeleteBlock {
            index,
            block,
            annotations,
        }
    }

    /// A [`Op::DeleteText`] for `start..end` that carries the deleted text and
    /// the notes the delete will drop.
    ///
    /// # Panics
    ///
    /// Panics if the block is unknown or the range is not a char-bounded range
    /// of its text.
    pub fn delete_text(doc: &Document, block_id: BlockId, start: usize, end: usize) -> Op {
        let block = block(doc, block_id);
        assert!(
            start <= end
                && end <= block.text.len()
                && block.text.is_char_boundary(start)
                && block.text.is_char_boundary(end),
            "DeleteText range {start}..{end} is not a char-bounded range of block {block_id}"
        );
        Op::DeleteText {
            block_id,
            start,
            end,
            text: block.text[start..end].to_string(),
            removed: annotations_wholly_inside(doc, block_id, start, end),
        }
    }

    /// A [`Op::SetBlockKind`] that records the block's current kind as
    /// `old_kind`, so undo restores it exactly.
    ///
    /// # Panics
    ///
    /// Panics if the block is unknown.
    pub fn set_block_kind(doc: &Document, block_id: BlockId, kind: BlockKind) -> Op {
        Op::SetBlockKind {
            block_id,
            old_kind: block(doc, block_id).kind.clone(),
            kind,
        }
    }
}

/// The op that undoes `op`.
///
/// One asymmetry remains, and it is inherent to the op set: [`Op::InsertBlock`]
/// carries no notes, so `invert(DeleteBlock)` cannot restore them. Undo goes
/// through [`apply_inverse`], which re-attaches the delete's payload; a
/// from-scratch replay of `apply(invert(op))` alone would not.
pub fn invert(op: &Op) -> Op {
    match op {
        Op::InsertText {
            block_id,
            offset,
            text,
        } => Op::DeleteText {
            block_id: *block_id,
            start: *offset,
            end: *offset + text.len(),
            text: text.clone(),
            // An insertion cannot leave an anchor wholly inside the inserted
            // run: existing anchors at or after the offset move right with it.
            removed: Vec::new(),
        },
        Op::DeleteText {
            block_id,
            start,
            text,
            ..
        } => Op::InsertText {
            block_id: *block_id,
            offset: *start,
            text: text.clone(),
        },
        Op::InsertBlock { index, block } => Op::DeleteBlock {
            index: *index,
            block: block.clone(),
            annotations: Vec::new(),
        },
        Op::DeleteBlock { index, block, .. } => Op::InsertBlock {
            index: *index,
            block: block.clone(),
        },
        Op::SetBlockKind {
            block_id,
            old_kind,
            kind,
        } => Op::SetBlockKind {
            block_id: *block_id,
            old_kind: kind.clone(),
            kind: old_kind.clone(),
        },
        Op::AddAnnotation { annotation } => Op::RemoveAnnotation {
            annotation: annotation.clone(),
        },
        Op::RemoveAnnotation { annotation } => Op::AddAnnotation {
            annotation: annotation.clone(),
        },
        Op::SetTitle { old, new } => Op::SetTitle {
            old: new.clone(),
            new: old.clone(),
        },
        // Reversing the members is what makes a batch undo in the only order
        // that works: undo something created after the thing that created it.
        Op::Batch { ops } => Op::Batch {
            ops: ops.iter().rev().map(invert).collect(),
        },
    }
}

/// Applies the *undo* of `op` to `doc`.
///
/// Equivalent to `apply(doc, &invert(op))` followed by re-adding whatever the
/// forward op destroyed: a `DeleteBlock`'s notes and a `DeleteText`'s wholly
/// removed notes both travel in the op's payload. Undoing those deletions is
/// therefore lossless; `invert` alone is not.
///
/// Restored notes are appended to `doc.annotations`, so their position in that
/// list can differ from before the delete. Annotation order carries no meaning
/// (each note carries its own id and anchor), but a caller comparing whole
/// documents after delete + undo should compare notes as a set.
///
/// Straddling notes are not fully restored — see [`crate::edit::Editor::undo`].
pub fn apply_inverse(doc: &mut Document, op: &Op) {
    // A batch is undone member by member in reverse, each through
    // `apply_inverse`, so *every* removal payload is re-attached — flattening
    // it through `invert` first would lose them.
    if let Op::Batch { ops } = op {
        for member in ops.iter().rev() {
            apply_inverse(doc, member);
        }
        return;
    }
    apply(doc, &invert(op));
    let restore: &[Annotation] = match op {
        Op::DeleteBlock { annotations, .. } => annotations,
        Op::DeleteText { removed, .. } => removed,
        _ => return,
    };
    for annotation in restore {
        apply(
            doc,
            &Op::AddAnnotation {
                annotation: annotation.clone(),
            },
        );
    }
}

/// Applies `op` to `doc`.
///
/// # Panics
///
/// Panics if the op does not fit the document (unknown block id, offset out of
/// range or inside a multi-byte character, block index out of range), or if a
/// removal op's payload does not match what the document actually holds. Ops
/// are expected to be built from the current document, so a mismatch is a bug,
/// not user input — and silently dropping a mismatched payload would destroy
/// notes in release builds, which is why it is a hard assert.
pub fn apply(doc: &mut Document, op: &Op) {
    match op {
        Op::InsertText {
            block_id,
            offset,
            text,
        } => {
            {
                let block = block_mut(doc, *block_id);
                assert!(
                    *offset <= block.text.len() && block.text.is_char_boundary(*offset),
                    "InsertText offset {offset} is not a char boundary in block {block_id}"
                );
                block.text.insert_str(*offset, text);
            }
            shift_anchors(&mut doc.annotations, *block_id, *offset, text.len() as isize);
        }
        Op::DeleteText {
            block_id,
            start,
            end,
            text,
            removed,
        } => {
            {
                let block = block_mut(doc, *block_id);
                assert!(*start <= *end, "DeleteText start {start} > end {end}");
                assert!(
                    *end <= block.text.len()
                        && block.text.is_char_boundary(*start)
                        && block.text.is_char_boundary(*end),
                    "DeleteText range {start}..{end} is not a char-bounded range \
                     of block {block_id}"
                );
                assert_eq!(
                    &block.text[*start..*end],
                    text.as_str(),
                    "DeleteText payload does not match the document text"
                );
                block.text.replace_range(*start..*end, "");
            }
            let dropped = rebase_annotations(doc, *block_id, *start, *end);
            assert_annotation_payload("DeleteText removed", removed, &dropped);
        }
        Op::InsertBlock { index, block } => {
            assert!(
                *index <= doc.blocks.len(),
                "InsertBlock index {index} out of range (len {})",
                doc.blocks.len()
            );
            doc.blocks.insert(*index, block.clone());
        }
        Op::DeleteBlock {
            index,
            block,
            annotations,
        } => {
            let at_index = doc.blocks.get(*index).unwrap_or_else(|| {
                panic!(
                    "DeleteBlock index {index} out of range (len {})",
                    doc.blocks.len()
                )
            });
            assert_eq!(
                at_index.id, block.id,
                "DeleteBlock payload does not match the block at index {index}"
            );
            let dropped = annotations_in_block(doc, block.id);
            assert_annotation_payload("DeleteBlock", annotations, &dropped);
            doc.blocks.remove(*index);
            doc.annotations
                .retain(|annotation| annotation.anchor.block_id != block.id);
        }
        Op::SetBlockKind {
            block_id,
            old_kind,
            kind,
        } => {
            let block = block_mut(doc, *block_id);
            assert_eq!(
                &block.kind, old_kind,
                "SetBlockKind payload old_kind does not match the document"
            );
            block.kind = kind.clone();
        }
        Op::AddAnnotation { annotation } => doc.annotations.push(annotation.clone()),
        Op::RemoveAnnotation { annotation } => {
            doc.annotations.retain(|existing| existing.id != annotation.id);
        }
        Op::SetTitle { old, new } => {
            assert_eq!(&doc.title, old, "SetTitle payload does not match the document");
            doc.title = new.clone();
        }
        Op::Batch { ops } => {
            for op in ops {
                apply(doc, op);
            }
        }
    }
}

/// Fills in payloads a caller left out of a removal op, against `doc`.
///
/// Called by [`crate::edit::Editor::apply_edit`] (the single chokepoint every edit passes
/// through), so a hand-built op that forgot its payload is repaired before
/// [`apply`] sees it. Only *missing* payloads are filled: a non-empty payload
/// is left alone, and [`apply`] panics if it disagrees with the document.
///
/// - `DeleteBlock`: an empty `annotations` is filled with the notes anchored in
///   the block.
/// - `DeleteText`: an empty `text` (for a non-empty range) is filled from the
///   document and an empty `removed` with the notes wholly inside the range.
/// - `SetBlockKind`: untouched — it has no way to express "missing", so an
///   incorrect `old_kind` is a hard error rather than a silent rewrite. Build
///   it with [`Op::set_block_kind`].
/// - `Batch`: each member is normalized in turn against the same document the
///   batch was built from (the `Op` constructors already saw that state).
pub(crate) fn normalize(doc: &Document, op: Op) -> Op {
    match op {
        Op::DeleteBlock {
            index,
            block,
            annotations,
        } => {
            if !annotations.is_empty() || !doc.blocks.iter().any(|b| b.id == block.id) {
                return Op::DeleteBlock {
                    index,
                    block,
                    annotations,
                };
            }
            let annotations = annotations_in_block(doc, block.id);
            Op::DeleteBlock {
                index,
                block,
                annotations,
            }
        }
        Op::DeleteText {
            block_id,
            start,
            end,
            text,
            removed,
        } => {
            let Some(current) = doc.blocks.iter().find(|b| b.id == block_id) else {
                return Op::DeleteText {
                    block_id,
                    start,
                    end,
                    text,
                    removed,
                };
            };
            let range_fits = start <= end
                && end <= current.text.len()
                && current.text.is_char_boundary(start)
                && current.text.is_char_boundary(end);
            if !range_fits {
                return Op::DeleteText {
                    block_id,
                    start,
                    end,
                    text,
                    removed,
                };
            }
            let text = if text.is_empty() && start < end {
                current.text[start..end].to_string()
            } else {
                text
            };
            let removed = if removed.is_empty() {
                annotations_wholly_inside(doc, block_id, start, end)
            } else {
                removed
            };
            Op::DeleteText {
                block_id,
                start,
                end,
                text,
                removed,
            }
        }
        // Members are normalized against the same (pre-batch) document they
        // were built from — the state every constructor saw.
        Op::Batch { ops } => Op::Batch {
            ops: ops.into_iter().map(|op| normalize(doc, op)).collect(),
        },
        other => other,
    }
}

fn block<'a>(doc: &'a Document, block_id: BlockId) -> &'a Block {
    doc.blocks
        .iter()
        .find(|block| block.id == block_id)
        .unwrap_or_else(|| panic!("op references unknown block {block_id}"))
}

fn block_mut<'a>(doc: &'a mut Document, block_id: BlockId) -> &'a mut Block {
    doc.blocks
        .iter_mut()
        .find(|block| block.id == block_id)
        .unwrap_or_else(|| panic!("op references unknown block {block_id}"))
}

/// Every note anchored in `block_id`, in document order.
fn annotations_in_block(doc: &Document, block_id: BlockId) -> Vec<Annotation> {
    doc.annotations
        .iter()
        .filter(|annotation| annotation.anchor.block_id == block_id)
        .cloned()
        .collect()
}

/// Every non-empty note anchored in `block_id` wholly inside `start..end`.
fn annotations_wholly_inside(
    doc: &Document,
    block_id: BlockId,
    start: usize,
    end: usize,
) -> Vec<Annotation> {
    doc.annotations
        .iter()
        .filter(|annotation| {
            let anchor = &annotation.anchor;
            anchor.block_id == block_id
                && anchor.start < anchor.end
                && anchor.start >= start
                && anchor.end <= end
        })
        .cloned()
        .collect()
}

/// Panics unless `carried` and `actual` are equal as multisets of whole
/// [`Annotation`] values (order-insensitive; identity comparison would miss a
/// payload carrying the note id with the wrong body or range).
fn assert_annotation_payload(what: &str, carried: &[Annotation], actual: &[Annotation]) {
    let mut remaining: Vec<&Annotation> = actual.iter().collect();
    for annotation in carried {
        let position = remaining
            .iter()
            .position(|candidate| *candidate == annotation)
            .unwrap_or_else(|| {
                panic!(
                    "{what} payload annotation {} does not match what the document holds",
                    annotation.id
                )
            });
        remaining.remove(position);
    }
    assert!(
        remaining.is_empty(),
        "{what} payload is missing {} annotation(s) the operation drops",
        remaining.len()
    );
}

/// Where an offset lands after the range `start..end` is deleted.
fn map_deleted_offset(offset: usize, start: usize, end: usize) -> usize {
    if offset <= start {
        offset
    } else if offset >= end {
        offset - (end - start)
    } else {
        start
    }
}

/// Keeps every annotation consistent with a deletion of `start..end`, and
/// returns the notes it dropped.
///
/// Three cases, in order:
/// 1. A non-empty anchor lying wholly inside the deleted range has no text
///    left, so it is dropped and returned (the caller's op payload carries it,
///    so undo can put it back).
/// 2. An anchor that straddles the range has its surviving endpoints remapped
///    one at a time: a prefix before `start` stays put, a suffix after `end`
///    closes up, and an endpoint inside the range collapses onto `start`.
///    `shift_anchors` cannot express this — it moves both endpoints by the same
///    delta, or neither — so the remap cannot be delegated to it.
/// 3. Survivors wholly after the range are rebased by `shift_anchors` with the
///    edit anchored at `end` (everything inside the range is already handled,
///    so `end` is where the remaining shift starts).
fn rebase_annotations(
    doc: &mut Document,
    block_id: BlockId,
    start: usize,
    end: usize,
) -> Vec<Annotation> {
    if start == end {
        return Vec::new();
    }

    let mut dropped: Vec<Annotation> = Vec::new();
    doc.annotations.retain(|annotation| {
        let anchor = &annotation.anchor;
        let wholly_inside = anchor.block_id == block_id
            && anchor.start < anchor.end
            && anchor.start >= start
            && anchor.end <= end;
        if wholly_inside {
            dropped.push(annotation.clone());
        }
        !wholly_inside
    });

    for annotation in doc.annotations.iter_mut() {
        let anchor = &mut annotation.anchor;
        if anchor.block_id != block_id || anchor.end <= start || anchor.start >= end {
            continue; // no overlap with the deleted range
        }
        anchor.start = map_deleted_offset(anchor.start, start, end);
        anchor.end = map_deleted_offset(anchor.end, start, end);
    }

    shift_anchors(&mut doc.annotations, block_id, end, -((end - start) as isize));
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Anchor, AnnotationKind};
    use uuid::Uuid;

    fn doc(text: &str) -> Document {
        let mut d = Document::new("Doc");
        d.blocks[0].text = text.to_string();
        d
    }

    fn note(block_id: BlockId, start: usize, end: usize) -> Annotation {
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
    fn invert_insert_text_is_delete_text() {
        let id = Uuid::new_v4();
        let op = Op::InsertText {
            block_id: id,
            offset: 3,
            text: "hi".into(),
        };
        assert_eq!(
            invert(&op),
            Op::DeleteText {
                block_id: id,
                start: 3,
                end: 5,
                text: "hi".into(),
                removed: Vec::new(),
            }
        );
    }

    #[test]
    fn invert_delete_text_is_insert_text() {
        let d = doc("abcdef");
        let id = d.blocks[0].id;
        let op = Op::delete_text(&d, id, 3, 5);
        assert_eq!(
            invert(&op),
            Op::InsertText {
                block_id: id,
                offset: 3,
                text: "de".into(),
            }
        );
    }

    /// The FIX-4 requirement: `invert` is involutive, so undoing a kind change
    /// restores the exact prior kind, heading level included.
    #[test]
    fn set_block_kind_invert_restores_the_previous_kind() {
        let mut d = doc("text");
        let id = d.blocks[0].id;
        d.blocks[0].kind = BlockKind::Heading { level: 3 };

        let op = Op::set_block_kind(&d, id, BlockKind::Paragraph);
        let undone = invert(&op);
        assert_eq!(
            undone,
            Op::SetBlockKind {
                block_id: id,
                old_kind: BlockKind::Paragraph,
                kind: BlockKind::Heading { level: 3 },
            }
        );
        assert_eq!(invert(&undone), op, "invert must be involutive");
    }

    #[test]
    fn insert_then_invert_restores_document() {
        let mut d = doc("hello");
        let before = d.clone();
        let op = Op::InsertText {
            block_id: d.blocks[0].id,
            offset: 0,
            text: "well ".into(),
        };
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "well hello");

        apply(&mut d, &invert(&op));
        assert_eq!(d, before);
    }

    #[test]
    fn insert_uses_byte_offsets_and_shifts_anchors() {
        let mut d = doc("hello");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 2, 4));
        let op = Op::InsertText {
            block_id: id,
            offset: 2,
            text: "é".into(), // two bytes
        };
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "heéllo");
        assert_eq!(d.annotations[0].anchor.start, 4);
        assert_eq!(d.annotations[0].anchor.end, 6);
    }

    #[test]
    fn delete_then_invert_restores_document_when_no_anchor_overlaps() {
        let mut d = doc("hello world");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 0, 4)); // entirely before the edit
        let before = d.clone();

        let op = Op::delete_text(&d, id, 5, 11);
        assert_eq!(op_text(&op), " world");
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "hello");
        assert_eq!(d.annotations[0].anchor.start, 0);
        assert_eq!(d.annotations[0].anchor.end, 4);

        apply(&mut d, &invert(&op));
        assert_eq!(d, before);
    }

    fn op_text(op: &Op) -> String {
        match op {
            Op::DeleteText { text, .. } => text.clone(),
            other => panic!("not a DeleteText: {other:?}"),
        }
    }

    #[test]
    fn delete_removes_annotation_wholly_inside_range() {
        let mut d = doc("hello world");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 6, 11)); // exactly the deleted range
        d.annotations.push(note(id, 7, 9)); // inside the deleted range
        let survivor = note(id, 0, 5);
        d.annotations.push(survivor.clone());

        let op = Op::delete_text(&d, id, 5, 11);
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "hello");
        assert_eq!(d.annotations, vec![survivor]);
    }

    /// Undoing a text delete puts back the notes the delete dropped: the op
    /// payload is the only record of them.
    #[test]
    fn undo_of_text_delete_restores_wholly_removed_notes() {
        let mut d = doc("hello world");
        let id = d.blocks[0].id;
        let inside = note(id, 6, 11);
        let survivor = note(id, 0, 5);
        d.annotations.push(survivor.clone());
        d.annotations.push(inside.clone());

        let op = Op::delete_text(&d, id, 5, 11);
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "hello");
        assert_eq!(d.annotations, vec![survivor.clone()]);

        apply_inverse(&mut d, &op);
        assert_eq!(d.blocks[0].text, "hello world");
        assert_eq!(d.annotations.len(), 2);
        assert!(d.annotations.contains(&survivor));
        assert!(d.annotations.contains(&inside));
        assert_eq!(d.annotations.iter().find(|a| a.id == inside.id), Some(&inside));
    }

    /// A delete that straddles a note's range must not silently corrupt the
    /// anchor: the endpoint that survives is rebased, the endpoint inside the
    /// deleted run collapses onto the edit start.
    #[test]
    fn delete_straddling_annotation_start_keeps_prefix_and_rebases_end() {
        let mut d = doc("abcdefgh");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 1, 8)); // covers "bcdefgh"

        let op = Op::delete_text(&d, id, 4, 6);
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "abcdgh");
        assert_eq!(d.annotations[0].anchor.start, 1); // "b" survives before the edit
        assert_eq!(d.annotations[0].anchor.end, 6); // "gh" closes the range
    }

    #[test]
    fn delete_rebases_annotation_whose_start_lies_inside_range() {
        let mut d = doc("abcdefgh");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 4, 8)); // covers "efgh", "ef" is deleted

        let op = Op::delete_text(&d, id, 2, 6);
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "abgh");
        assert_eq!(d.annotations[0].anchor.start, 2); // collapses onto the edit start
        assert_eq!(d.annotations[0].anchor.end, 4); // "gh" closed up
    }

    /// FIX 5: the only path in `rebase_annotations` that actually calls
    /// `shift_anchors` — notes wholly *after* the deleted range.
    #[test]
    fn delete_shifts_annotations_wholly_after_the_range() {
        let mut d = doc("abcdefghij");
        let id = d.blocks[0].id;
        let before = note(id, 0, 2); // "ab"
        let after = note(id, 6, 9); // "ghi"
        d.annotations.push(before.clone());
        d.annotations.push(after.clone());

        let op = Op::delete_text(&d, id, 3, 5); // deletes "de"
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "abcfghij");
        assert_eq!(d.annotations[0], before, "note before the range is untouched");
        assert_eq!(d.annotations[1].anchor.start, 4, "\"ghi\" closed up by 2");
        assert_eq!(d.annotations[1].anchor.end, 7);
        assert_eq!(&d.blocks[0].text[4..7], "ghi");

        apply_inverse(&mut d, &op);
        assert_eq!(d.blocks[0].text, "abcdefghij");
        assert_eq!(d.annotations[1], after);
    }

    /// FIX 5: deleting a block's entire text drops every note in it, and undo
    /// restores text plus notes.
    #[test]
    fn deleting_a_blocks_entire_text_removes_all_its_notes() {
        let mut d = doc("abc");
        let id = d.blocks[0].id;
        let whole = note(id, 0, 3);
        let inner = note(id, 1, 2);
        d.annotations.push(whole.clone());
        d.annotations.push(inner.clone());

        let op = Op::delete_text(&d, id, 0, 3);
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "");
        assert!(d.annotations.is_empty(), "no text left to anchor a note to");

        apply_inverse(&mut d, &op);
        assert_eq!(d.blocks[0].text, "abc");
        assert_eq!(d.annotations.len(), 2);
        assert!(d.annotations.contains(&whole));
        assert!(d.annotations.contains(&inner));
    }

    #[test]
    fn delete_leaves_other_block_anchors_alone() {
        let mut d = doc("abc");
        let id = d.blocks[0].id;
        let other = Uuid::new_v4();
        d.annotations.push(note(other, 1, 2));

        let op = Op::delete_text(&d, id, 0, 2);
        apply(&mut d, &op);
        assert_eq!(d.annotations[0].anchor.start, 1);
        assert_eq!(d.annotations[0].anchor.end, 2);
    }

    /// A delete whose payload text disagrees with the document is a bug in the
    /// caller: `invert` would restore the wrong text, so `apply` refuses.
    #[test]
    #[should_panic(expected = "DeleteText payload does not match")]
    fn delete_text_with_a_wrong_text_payload_panics() {
        let mut d = doc("hello");
        let id = d.blocks[0].id;
        apply(
            &mut d,
            &Op::DeleteText {
                block_id: id,
                start: 0,
                end: 5,
                text: "wrong".into(),
                removed: Vec::new(),
            },
        );
    }

    /// A DeleteBlock that forgot its notes must not silently destroy them.
    #[test]
    #[should_panic(expected = "DeleteBlock payload is missing")]
    fn delete_block_with_a_missing_payload_panics() {
        let mut d = doc("first");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 0, 5));
        let block = d.blocks[0].clone();
        apply(
            &mut d,
            &Op::DeleteBlock {
                index: 0,
                block,
                annotations: Vec::new(),
            },
        );
    }

    /// Full-value equality, not just id: a payload carrying the right note id
    /// with the wrong body is still a mismatch.
    #[test]
    #[should_panic(expected = "does not match what the document holds")]
    fn delete_block_with_a_tampered_payload_panics() {
        let mut d = doc("first");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 0, 5));
        let mut tampered = note(id, 0, 5);
        tampered.body = "other".into();
        let block = d.blocks[0].clone();
        apply(
            &mut d,
            &Op::DeleteBlock {
                index: 0,
                block,
                annotations: vec![tampered],
            },
        );
    }

    #[test]
    fn delete_block_carries_its_annotations_and_undo_restores_both() {
        let mut d = doc("first");
        let second = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "second".to_string(),
            spans: Vec::new(),
        };
        d.blocks.push(second.clone());
        let id = second.id;
        let attached = note(id, 0, 6);
        let elsewhere = note(d.blocks[0].id, 0, 5);
        d.annotations.push(attached.clone());
        d.annotations.push(elsewhere.clone());

        let op = Op::delete_block(&d, 1);
        assert_eq!(op, Op::DeleteBlock {
            index: 1,
            block: second.clone(),
            annotations: vec![attached.clone()],
        });
        apply(&mut d, &op);
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.annotations, vec![elsewhere.clone()]);

        apply_inverse(&mut d, &op);
        assert_eq!(d.blocks.len(), 2);
        assert_eq!(d.blocks[1], second);
        assert!(d.annotations.contains(&attached));
        assert_eq!(d.annotations.len(), 2);

        // Redoing the delete removes block and note again, so the payload is
        // recyclable.
        apply(&mut d, &op);
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.annotations, vec![elsewhere]);
    }

    #[test]
    fn insert_block_round_trips() {
        let mut d = doc("first");
        let added = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Heading { level: 2 },
            text: "added".to_string(),
            spans: Vec::new(),
        };
        let before = d.clone();
        let op = Op::InsertBlock {
            index: 1,
            block: added.clone(),
        };
        apply(&mut d, &op);
        assert_eq!(d.blocks.len(), 2);
        assert_eq!(d.blocks[1], added);

        apply(&mut d, &invert(&op));
        assert_eq!(d, before);
    }

    /// `SetBlockKind` records the previous kind, so undo restores it exactly —
    /// here a heading keeps its level instead of falling back to a paragraph.
    #[test]
    fn set_block_kind_undo_restores_heading_level() {
        let mut d = doc("text");
        let id = d.blocks[0].id;
        d.blocks[0].kind = BlockKind::Heading { level: 3 };

        let op = Op::set_block_kind(&d, id, BlockKind::ListItem { ordered: true });
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].kind, BlockKind::ListItem { ordered: true });

        apply_inverse(&mut d, &op);
        assert_eq!(d.blocks[0].kind, BlockKind::Heading { level: 3 });
        assert_eq!(d.blocks[0].text, "text");
    }

    #[test]
    fn set_title_round_trips() {
        let mut d = doc("text");
        let before = d.clone();
        let op = Op::SetTitle {
            old: "Doc".to_string(),
            new: "New Title".to_string(),
        };
        apply(&mut d, &op);
        assert_eq!(d.title, "New Title");

        apply(&mut d, &invert(&op));
        assert_eq!(d, before);
    }

    #[test]
    fn add_and_remove_annotation_round_trip() {
        let mut d = doc("text");
        let before = d.clone();
        let ann = note(d.blocks[0].id, 0, 4);

        let add = Op::AddAnnotation {
            annotation: ann.clone(),
        };
        apply(&mut d, &add);
        assert_eq!(d.annotations, vec![ann.clone()]);
        apply(&mut d, &invert(&add));
        assert_eq!(d, before);

        d.annotations.push(ann.clone());
        let remove = Op::RemoveAnnotation {
            annotation: ann.clone(),
        };
        apply(&mut d, &remove);
        assert!(d.annotations.is_empty());
        apply(&mut d, &invert(&remove));
        assert_eq!(d.annotations, vec![ann]);
    }

    #[test]
    fn op_serde_round_trip() {
        let op = Op::DeleteText {
            block_id: Uuid::new_v4(),
            start: 1,
            end: 4,
            text: "abc".into(),
            removed: Vec::new(),
        };
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(serde_json::from_str::<Op>(&json).unwrap(), op);
    }

    /// Rows written before `removed` existed must still load (they just cannot
    /// restore the notes that delete dropped).
    #[test]
    fn delete_text_without_removed_field_deserializes() {
        let json = r#"{"DeleteText":{"block_id":"a3f1c6c8-3a4e-4b1c-9d4f-5a1b2c3d4e5f","start":1,"end":4,"text":"abc"}}"#;
        match serde_json::from_str::<Op>(json).unwrap() {
            Op::DeleteText { removed, text, .. } => {
                assert_eq!(text, "abc");
                assert!(removed.is_empty());
            }
            other => panic!("expected DeleteText, got {other:?}"),
        }
    }

    /// Every variant is persisted as JSON in the op log, so all of them have to
    /// survive a round trip with their payload intact.
    #[test]
    fn every_op_variant_survives_serde() {
        let block_id = Uuid::new_v4();
        let block = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Heading { level: 3 },
            text: "b".into(),
            spans: Vec::new(),
        };
        let annotation = note(block.id, 0, 1);
        let ops = vec![
            Op::InsertText {
                block_id,
                offset: 0,
                text: "é".into(),
            },
            Op::DeleteText {
                block_id,
                start: 0,
                end: 2,
                text: "é".into(),
                removed: vec![annotation.clone()],
            },
            Op::InsertBlock {
                index: 0,
                block: block.clone(),
            },
            Op::DeleteBlock {
                index: 0,
                block: block.clone(),
                annotations: vec![annotation.clone()],
            },
            Op::SetBlockKind {
                block_id,
                old_kind: BlockKind::Heading { level: 3 },
                kind: BlockKind::ListItem { ordered: false },
            },
            Op::AddAnnotation {
                annotation: annotation.clone(),
            },
            Op::RemoveAnnotation { annotation },
            Op::SetTitle {
                old: "a".into(),
                new: "b".into(),
            },
        ];
        for op in ops {
            let json = serde_json::to_string(&op).unwrap();
            assert_eq!(serde_json::from_str::<Op>(&json).unwrap(), op, "json: {json}");
        }
    }

    /// A batch applies its members in order, so a delete followed by an append
    /// leaves one block holding both texts and the note re-anchored over them —
    /// the shape `Editor::merge_with_next` builds.
    fn merge_shaped_batch(d: &Document) -> (Block, Annotation, Annotation, Op) {
        let first = d.blocks[0].id;
        let second = d.blocks[1].clone();
        let original = d.annotations[0].clone();
        let moved = Annotation {
            id: original.id,
            kind: original.kind.clone(),
            anchor: Anchor {
                block_id: first,
                start: d.blocks[0].text.len(),
                end: d.blocks[0].text.len() + second.text.len(),
            },
            body: original.body.clone(),
        };
        let batch = Op::Batch {
            ops: vec![
                Op::delete_block(d, 1),
                Op::InsertText {
                    block_id: first,
                    offset: d.blocks[0].text.len(),
                    text: second.text.clone(),
                },
                Op::AddAnnotation {
                    annotation: moved.clone(),
                },
            ],
        };
        (second, original, moved, batch)
    }

    #[test]
    fn batch_applies_members_in_order() {
        let mut d = doc("one");
        d.blocks.push(Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "two".into(),
            spans: Vec::new(),
        });
        d.annotations.push(note(d.blocks[1].id, 0, 3));
        let (_second, _original, moved, batch) = merge_shaped_batch(&d);

        apply(&mut d, &batch);
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].text, "onetwo");
        assert_eq!(d.annotations, vec![moved]);
    }

    #[test]
    fn apply_inverse_of_a_batch_restores_every_members_payload() {
        let mut d = doc("one");
        d.blocks.push(Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "two".into(),
            spans: Vec::new(),
        });
        d.annotations.push(note(d.blocks[1].id, 0, 3));
        let before = d.clone();
        let (second, original, _moved, batch) = merge_shaped_batch(&d);

        apply(&mut d, &batch);
        apply_inverse(&mut d, &batch);

        assert_eq!(d.blocks.len(), 2);
        assert_eq!(d.blocks[0].text, "one");
        assert_eq!(d.blocks[1], second);
        assert_eq!(
            d.annotations,
            vec![original],
            "the note comes back where it was"
        );
        assert_eq!(d, before, "undo restores the exact prior document");

        // Redo: the batch is re-applicable after its undo.
        apply(&mut d, &batch);
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].text, "onetwo");
    }

    #[test]
    fn invert_batch_reverses_and_inverts_each_member() {
        let batch = Op::Batch {
            ops: vec![
                Op::InsertText {
                    block_id: Uuid::new_v4(),
                    offset: 0,
                    text: "ab".into(),
                },
                Op::InsertBlock {
                    index: 0,
                    block: Block {
                        id: Uuid::new_v4(),
                        kind: BlockKind::Paragraph,
                        text: "b".into(),
                        spans: Vec::new(),
                    },
                },
            ],
        };

        match invert(&batch) {
            Op::Batch { ops } => {
                assert_eq!(ops.len(), 2);
                assert!(
                    matches!(ops[0], Op::DeleteBlock { .. }),
                    "the member applied last is inverted first"
                );
                assert!(matches!(ops[1], Op::DeleteText { .. }));
            }
            other => panic!("expected a batch, got {other:?}"),
        }
    }

    #[test]
    fn batch_survives_serde() {
        let op = Op::Batch {
            ops: vec![Op::InsertText {
                block_id: Uuid::new_v4(),
                offset: 0,
                text: "x".into(),
            }],
        };
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(serde_json::from_str::<Op>(&json).unwrap(), op);
    }

    #[test]
    fn empty_batch_is_a_no_op() {
        let mut d = doc("text");
        let before = d.clone();
        apply(&mut d, &Op::Batch { ops: Vec::new() });
        assert_eq!(d, before);
        apply_inverse(&mut d, &Op::Batch { ops: Vec::new() });
        assert_eq!(d, before);
    }
}
