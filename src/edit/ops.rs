//! Invertible document mutations.
//!
//! Every mutation of a [`Document`] is expressed as an [`Op`]. Each op carries
//! the material it removes — deleted text, a removed block, a removed note, the
//! previous title — so [`invert`] can undo it without consulting anything but
//! the op itself. That is what makes undo durable: the op log, not the saved
//! document blob, is the source of truth for history.
//!
//! All offsets are **byte** offsets into a block's text, matching
//! [`Anchor`](crate::model::Anchor) and `String` indexing.

use serde::{Deserialize, Serialize};

use crate::model::{shift_anchors, Annotation, Block, BlockId, BlockKind, Document};

/// A single invertible mutation of a [`Document`].
///
/// Removals carry their payload (`text`, `block`, `annotations`, `old`) that
/// [`invert`] can turn into the matching insertion.
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
    SetBlockKind {
        block_id: BlockId,
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
}

/// The op that undoes `op`.
///
/// Two plan-level asymmetries are documented rather than hidden (see the
/// task-5 report):
///
/// - [`Op::DeleteBlock`] inverts to a plain [`Op::InsertBlock`], which carries
///   no notes. Use [`apply_inverse`] — not `apply(invert(op))` — to undo, so the
///   delete's annotation payload is re-attached.
/// - [`Op::SetBlockKind`] records only the *new* kind, so its inverse falls back
///   to [`BlockKind::Paragraph`]. Undoing a kind change therefore returns the
///   block to a paragraph; it cannot restore a heading level.
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
        },
        Op::DeleteText {
            block_id,
            start,
            end,
            text,
        } => {
            debug_assert_eq!(
                text.len(),
                end - start,
                "DeleteText payload text must be exactly the deleted range"
            );
            Op::InsertText {
                block_id: *block_id,
                offset: *start,
                text: text.clone(),
            }
        }
        Op::InsertBlock { index, block } => Op::DeleteBlock {
            index: *index,
            block: block.clone(),
            annotations: Vec::new(),
        },
        Op::DeleteBlock { index, block, .. } => Op::InsertBlock {
            index: *index,
            block: block.clone(),
        },
        Op::SetBlockKind { block_id, .. } => Op::SetBlockKind {
            block_id: *block_id,
            kind: BlockKind::Paragraph,
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
    }
}

/// Applies the *undo* of `op` to `doc`.
///
/// Equivalent to `apply(doc, &invert(op))` except for [`Op::DeleteBlock`],
/// whose notes travel in the op's `annotations` payload: the inverse
/// [`Op::InsertBlock`] re-inserts the block, then the payload's notes are
/// re-added. Undoing a block delete is lossless; `invert` alone is not.
pub fn apply_inverse(doc: &mut Document, op: &Op) {
    apply(doc, &invert(op));
    if let Op::DeleteBlock { annotations, .. } = op {
        for annotation in annotations {
            apply(
                doc,
                &Op::AddAnnotation {
                    annotation: annotation.clone(),
                },
            );
        }
    }
}

/// Applies `op` to `doc`.
///
/// # Panics
///
/// Panics if the op does not fit the document (unknown block id, offset out of
/// range or inside a multi-byte character, block index out of range). Ops are
/// expected to be built from the current document, so a mismatch is a bug, not
/// user input.
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
                debug_assert_eq!(
                    &block.text[*start..*end],
                    text.as_str(),
                    "DeleteText payload does not match the document text"
                );
                block.text.replace_range(*start..*end, "");
            }
            rebase_annotations(doc, *block_id, *start, *end);
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
            assert!(
                *index < doc.blocks.len(),
                "DeleteBlock index {index} out of range (len {})",
                doc.blocks.len()
            );
            let removed = doc.blocks.remove(*index);
            assert_eq!(
                removed.id, block.id,
                "DeleteBlock payload does not match the block at index {index}"
            );
            let mut dropped: Vec<Annotation> = Vec::new();
            doc.annotations.retain(|annotation| {
                if annotation.anchor.block_id == block.id {
                    dropped.push(annotation.clone());
                    false
                } else {
                    true
                }
            });
            debug_assert!(
                dropped.len() == annotations.len()
                    && annotations
                        .iter()
                        .all(|carried| dropped.iter().any(|d| d.id == carried.id)),
                "DeleteBlock payload must carry every annotation anchored in the block"
            );
        }
        Op::SetBlockKind { block_id, kind } => {
            block_mut(doc, *block_id).kind = kind.clone();
        }
        Op::AddAnnotation { annotation } => doc.annotations.push(annotation.clone()),
        Op::RemoveAnnotation { annotation } => {
            doc.annotations.retain(|existing| existing.id != annotation.id);
        }
        Op::SetTitle { old, new } => {
            debug_assert_eq!(&doc.title, old, "SetTitle payload does not match the document");
            doc.title = new.clone();
        }
    }
}

fn block_mut<'a>(doc: &'a mut Document, block_id: BlockId) -> &'a mut Block {
    doc.blocks
        .iter_mut()
        .find(|block| block.id == block_id)
        .unwrap_or_else(|| panic!("op references unknown block {block_id}"))
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

/// Keeps every annotation consistent with a deletion of `start..end`.
///
/// Three cases, in order:
/// 1. A non-empty anchor lying wholly inside the deleted range has no text
///    left, so it is dropped (the delete really does remove the note; this is
///    the lossy part of a text delete — see the task-5 report).
/// 2. An anchor that straddles the range has its surviving endpoints remapped
///    one at a time: a prefix before `start` stays put, a suffix after `end`
///    closes up, and an endpoint inside the range collapses onto `start`.
///    `shift_anchors` cannot express this — it moves both endpoints by the same
///    delta, or neither — so the remap cannot be delegated to it.
/// 3. Survivors wholly after the range are rebased by `shift_anchors` with the
///    edit anchored at `end` (everything inside the range is already handled,
///    so `end` is where the remaining shift starts).
fn rebase_annotations(doc: &mut Document, block_id: BlockId, start: usize, end: usize) {
    if start == end {
        return;
    }

    doc.annotations.retain(|annotation| {
        let anchor = &annotation.anchor;
        !(anchor.block_id == block_id
            && anchor.start < anchor.end
            && anchor.start >= start
            && anchor.end <= end)
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
            }
        );
    }

    #[test]
    fn invert_delete_text_is_insert_text() {
        let id = Uuid::new_v4();
        let op = Op::DeleteText {
            block_id: id,
            start: 3,
            end: 5,
            text: "hi".into(),
        };
        assert_eq!(
            invert(&op),
            Op::InsertText {
                block_id: id,
                offset: 3,
                text: "hi".into(),
            }
        );
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

        let op = Op::DeleteText {
            block_id: id,
            start: 5,
            end: 11,
            text: " world".into(),
        };
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "hello");
        assert_eq!(d.annotations[0].anchor.start, 0);
        assert_eq!(d.annotations[0].anchor.end, 4);

        apply(&mut d, &invert(&op));
        assert_eq!(d, before);
    }

    #[test]
    fn delete_removes_annotation_wholly_inside_range() {
        let mut d = doc("hello world");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 6, 11)); // exactly the deleted range
        d.annotations.push(note(id, 7, 9)); // inside the deleted range
        let survivor = note(id, 0, 5);
        d.annotations.push(survivor.clone());

        apply(
            &mut d,
            &Op::DeleteText {
                block_id: id,
                start: 5,
                end: 11,
                text: " world".into(),
            },
        );
        assert_eq!(d.blocks[0].text, "hello");
        assert_eq!(d.annotations, vec![survivor]);
    }

    /// A delete that straddles a note's range must not silently corrupt the
    /// anchor: the endpoint that survives is rebased, the endpoint inside the
    /// deleted run collapses onto the edit start.
    #[test]
    fn delete_straddling_annotation_start_keeps_prefix_and_rebases_end() {
        let mut d = doc("abcdefgh");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 1, 8)); // covers "bcdefgh"

        apply(
            &mut d,
            &Op::DeleteText {
                block_id: id,
                start: 4,
                end: 6,
                text: "ef".into(),
            },
        );
        assert_eq!(d.blocks[0].text, "abcdgh");
        assert_eq!(d.annotations[0].anchor.start, 1); // "b" survives before the edit
        assert_eq!(d.annotations[0].anchor.end, 6); // "gh" closes the range
    }

    #[test]
    fn delete_rebases_annotation_whose_start_lies_inside_range() {
        let mut d = doc("abcdefgh");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 4, 8)); // covers "efgh", "ef" is deleted

        apply(
            &mut d,
            &Op::DeleteText {
                block_id: id,
                start: 2,
                end: 6,
                text: "cdef".into(),
            },
        );
        assert_eq!(d.blocks[0].text, "abgh");
        assert_eq!(d.annotations[0].anchor.start, 2); // collapses onto the edit start
        assert_eq!(d.annotations[0].anchor.end, 4); // "gh" closed up
    }

    #[test]
    fn delete_leaves_other_block_anchors_alone() {
        let mut d = doc("abc");
        let id = d.blocks[0].id;
        let other = Uuid::new_v4();
        d.annotations.push(note(other, 1, 2));

        apply(
            &mut d,
            &Op::DeleteText {
                block_id: id,
                start: 0,
                end: 2,
                text: "ab".into(),
            },
        );
        assert_eq!(d.annotations[0].anchor.start, 1);
        assert_eq!(d.annotations[0].anchor.end, 2);
    }

    #[test]
    fn delete_block_removes_block_and_its_annotations_and_undo_restores_both() {
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

        let op = Op::DeleteBlock {
            index: 1,
            block: second.clone(),
            annotations: vec![attached.clone()],
        };
        apply(&mut d, &op);
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.annotations, vec![elsewhere.clone()]);

        apply_inverse(&mut d, &op);
        assert_eq!(d.blocks.len(), 2);
        assert_eq!(d.blocks[1], second);
        assert!(d.annotations.contains(&attached));

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

    /// `SetBlockKind` carries only the new kind, so undo falls back to the
    /// document default. Documented plan-level limitation (task-5 report).
    #[test]
    fn set_block_kind_undo_falls_back_to_paragraph() {
        let mut d = doc("text");
        let op = Op::SetBlockKind {
            block_id: d.blocks[0].id,
            kind: BlockKind::ListItem { ordered: true },
        };
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].kind, BlockKind::ListItem { ordered: true });

        apply_inverse(&mut d, &op);
        assert_eq!(d.blocks[0].kind, BlockKind::Paragraph);
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
        };
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(serde_json::from_str::<Op>(&json).unwrap(), op);
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
}
