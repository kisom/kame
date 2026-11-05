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

use crate::model::{
    shift_anchors, Anchor, Annotation, AnnotationId, Block, BlockId, BlockKind, Document, Span,
};

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
        /// The block's spans before the delete, which [`apply_inverse`] puts back: a
        /// delete drops or trims the spans it touches, and re-inserting the text can't
        /// tell what they were. Rows written before this field existed have none, and
        /// undo keeps whatever spans are left.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        old_spans: Option<Vec<Span>>,
        /// Notes the delete cut short, as they were, which [`apply_inverse`] puts back: a cut
        /// note keeps no record of how far it reached. [`crate::edit::Editor::apply_edit`] fills
        /// this in. Rows written before this field existed have none, and undo leaves those
        /// notes short, which can stop later steps of the history from applying.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        clipped: Vec<Annotation>,
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
    /// Replaces a block's inline spans. `old_spans` is what the document holds
    /// now, so [`invert`] restores them exactly. This is the authoring path for
    /// bold and italic emphasis (typed markdown, and a future format toolbar).
    SetSpans {
        block_id: BlockId,
        old_spans: Vec<Span>,
        spans: Vec<Span>,
    },
    AddAnnotation {
        annotation: Annotation,
    },
    RemoveAnnotation {
        annotation: Annotation,
    },
    /// Replaces one note's body.
    ///
    /// `old_body` is what the document holds now, so [`invert`] can put it back
    /// and undo restores the exact prior prose. A body is a whole value rather
    /// than a removal, so there is no separate restore payload — the way there
    /// is for a deleted block or text run.
    SetAnnotationBody {
        annotation_id: AnnotationId,
        old_body: String,
        new_body: String,
    },
    SetTitle {
        old: String,
        new: String,
    },
    /// Replaces the whole document, as merging another version does: one undo step, carrying
    /// both versions, since a merge can touch anything.
    ReplaceDocument {
        old: Box<Document>,
        new: Box<Document>,
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
    ///
    /// Applying a batch is atomic: [`apply`] stages the members on a copy and
    /// commits it only when all of them have applied, so a member that panics
    /// leaves the document untouched rather than partially edited.
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
            old_spans: Some(block.spans.clone()),
            clipped: Vec::new(),
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

    /// A [`Op::SetSpans`] that records the block's current spans as `old_spans`,
    /// so undo restores them exactly.
    ///
    /// # Panics
    ///
    /// Panics if the block is unknown.
    pub fn set_spans(doc: &Document, block_id: BlockId, spans: Vec<Span>) -> Op {
        Op::SetSpans {
            block_id,
            old_spans: block(doc, block_id).spans.clone(),
            spans,
        }
    }
}

/// `previous` and `next` as one edit, if `next` carries on the same word.
///
/// Nearly all of a document's history would otherwise be single keystrokes, at about 1 KB per
/// finished word, and undo would take back one letter at a time. So letters join up into
/// words, each keeping the spaces after it, and only a single character joins on: a paste
/// stays its own step, as does anything that isn't plain typing.
pub fn coalesce(previous: &Op, next: &Op) -> Option<Op> {
    fn new_word(before: &str, after: &str) -> bool {
        let spaced = before.chars().next_back().is_some_and(char::is_whitespace);
        spaced && !after.chars().next().is_some_and(char::is_whitespace)
    }
    fn typed(text: &str) -> bool {
        let mut chars = text.chars();
        chars.next().is_some() && chars.next().is_none() && text != "\n"
    }
    match (previous, next) {
        (
            Op::InsertText {
                block_id,
                offset,
                text,
            },
            Op::InsertText {
                block_id: next_block,
                offset: next_offset,
                text: next_text,
            },
        ) if block_id == next_block
            && *next_offset == offset + text.len()
            && typed(next_text)
            && !text.ends_with('\n')
            && !new_word(text, next_text) =>
        {
            Some(Op::InsertText {
                block_id: *block_id,
                offset: *offset,
                text: format!("{text}{next_text}"),
            })
        }
        (
            Op::DeleteText {
                block_id,
                start,
                end,
                text,
                removed,
                old_spans,
                clipped,
            },
            Op::DeleteText {
                block_id: next_block,
                start: next_start,
                end: next_end,
                text: next_text,
                removed: next_removed,
                clipped: next_clipped,
                ..
            },
        ) if block_id == next_block
            && removed.is_empty()
            && next_removed.is_empty()
            // What it says about a note is in terms of the text as it was by then.
            && next_clipped.is_empty()
            && typed(next_text) =>
        {
            // Backspacing runs back from where it began, and deleting forward stays put.
            let (start, end, joined) = if next_end == start {
                if new_word(next_text, text) {
                    return None;
                }
                (*next_start, *end, format!("{next_text}{text}"))
            } else if next_start == start {
                if new_word(text, next_text) {
                    return None;
                }
                (*start, end + next_text.len(), format!("{text}{next_text}"))
            } else {
                return None;
            };
            Some(Op::DeleteText {
                block_id: *block_id,
                start,
                end,
                text: joined,
                removed: Vec::new(),
                old_spans: old_spans.clone(),
                clipped: clipped.clone(),
            })
        }
        (
            Op::SetAnnotationBody {
                annotation_id,
                old_body,
                new_body,
            },
            Op::SetAnnotationBody {
                annotation_id: next_annotation,
                new_body: next_body,
                ..
            },
        ) if annotation_id == next_annotation
            && !new_word(new_body, next_body_tail(new_body, next_body)) =>
        {
            Some(Op::SetAnnotationBody {
                annotation_id: *annotation_id,
                old_body: old_body.clone(),
                new_body: next_body.clone(),
            })
        }
        _ => None,
    }
}

/// What `after` adds to `before`, for telling whether a note's edit starts a new word.
fn next_body_tail<'a>(before: &str, after: &'a str) -> &'a str {
    after.strip_prefix(before).unwrap_or("")
}

/// `ops` with everything [`coalesce`] would have joined joined, for a log saved before typing
/// coalesced.
pub fn compact(ops: &[Op]) -> Vec<Op> {
    let mut joined: Vec<Op> = Vec::with_capacity(ops.len());
    for op in ops {
        match joined.last().and_then(|last| coalesce(last, op)) {
            Some(merged) => *joined.last_mut().expect("it was just read") = merged,
            None => joined.push(op.clone()),
        }
    }
    joined
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
            old_spans: None,
            clipped: Vec::new(),
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
        Op::SetSpans {
            block_id,
            old_spans,
            spans,
        } => Op::SetSpans {
            block_id: *block_id,
            old_spans: spans.clone(),
            spans: old_spans.clone(),
        },
        Op::AddAnnotation { annotation } => Op::RemoveAnnotation {
            annotation: annotation.clone(),
        },
        Op::RemoveAnnotation { annotation } => Op::AddAnnotation {
            annotation: annotation.clone(),
        },
        Op::SetAnnotationBody {
            annotation_id,
            old_body,
            new_body,
        } => Op::SetAnnotationBody {
            annotation_id: *annotation_id,
            old_body: new_body.clone(),
            new_body: old_body.clone(),
        },
        Op::ReplaceDocument { old, new } => Op::ReplaceDocument {
            old: new.clone(),
            new: old.clone(),
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
    if let Op::DeleteText {
        block_id,
        old_spans: Some(spans),
        ..
    } = op
    {
        block_mut(doc, *block_id).spans = spans.clone();
    }
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
    if let Op::DeleteText { clipped, .. } = op {
        for was in clipped {
            if let Some(annotation) = doc.annotations.iter_mut().find(|note| note.id == was.id) {
                annotation.anchor = was.anchor.clone();
            }
        }
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
            // An insertion strictly inside a note's range must widen the note,
            // otherwise the note silently drops coverage of the inserted text
            // (`shift_anchors` only moves anchors whose start is at or after the
            // edit, which would leave such an anchor's end stale). Block-scoped
            // anchors have no range, so they are skipped here.
            let delta = text.len();
            for annotation in doc.annotations.iter_mut() {
                let Anchor::Range {
                    block_id: anchor_block_id,
                    start,
                    end,
                } = &mut annotation.anchor
                else {
                    continue;
                };
                if *anchor_block_id == *block_id && *start < *offset && *offset < *end {
                    *end = end.checked_add(delta).expect(
                        "anchor end overflowed usize: insert length inconsistent with document text",
                    );
                }
            }
            shift_anchors(
                &mut doc.annotations,
                *block_id,
                *offset,
                text.len() as isize,
            );
            // Spans (bold/italic/link) move and widen exactly like anchors, so
            // a link stays on the text it points at as surrounding text shifts.
            {
                let block = block_mut(doc, *block_id);
                for span in block.spans.iter_mut() {
                    if span.start < *offset && *offset < span.end {
                        span.end = span
                            .end
                            .checked_add(delta)
                            .expect("span end overflowed usize");
                    }
                }
                shift_spans(block, *offset, delta as isize);
            }
        }
        Op::DeleteText {
            block_id,
            start,
            end,
            text,
            removed,
            old_spans: _,
            clipped: _,
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
            rebase_spans(block_mut(doc, *block_id), *start, *end);
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
                .retain(|annotation| annotation.anchor.block_id() != block.id);
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
        // Unlike the removal payloads, nothing is lost if `old_spans` is out of step:
        // spans are only formatting, and history written by older versions can drift
        // from them. That isn't worth stopping the editor for.
        Op::SetSpans {
            block_id, spans, ..
        } => {
            block_mut(doc, *block_id).spans = spans.clone();
        }
        Op::AddAnnotation { annotation } => doc.annotations.push(annotation.clone()),
        Op::RemoveAnnotation { annotation } => {
            doc.annotations
                .retain(|existing| existing.id != annotation.id);
        }
        Op::SetAnnotationBody {
            annotation_id,
            old_body,
            new_body,
        } => {
            let annotation = doc
                .annotations
                .iter_mut()
                .find(|annotation| annotation.id == *annotation_id)
                .unwrap_or_else(|| panic!("op references unknown annotation {annotation_id}"));
            assert_eq!(
                &annotation.body, old_body,
                "SetAnnotationBody payload old_body does not match the document"
            );
            annotation.body = new_body.clone();
        }
        Op::ReplaceDocument { new, .. } => {
            *doc = (**new).clone();
        }
        Op::SetTitle { old, new } => {
            assert_eq!(
                &doc.title, old,
                "SetTitle payload does not match the document"
            );
            doc.title = new.clone();
        }
        // Stage-and-commit: the members are applied to a clone and the clone is
        // swapped in only once every member has succeeded. A member that panics
        // (bad payload, unknown block) therefore leaves `doc` exactly as it was,
        // instead of half-applying the batch with no undo entry to show for it.
        Op::Batch { ops } => {
            let mut staged = doc.clone();
            for op in ops {
                apply(&mut staged, op);
            }
            *doc = staged;
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
/// - `SetAnnotationBody`: an empty `old_body` against a note that already has
///   prose is filled in from the document, on the same "empty means missing"
///   rule the text delete uses; a non-empty `old_body` is left alone and
///   [`apply`] panics if it disagrees.
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
            old_spans,
            clipped,
        } => {
            let Some(current) = doc.blocks.iter().find(|b| b.id == block_id) else {
                return Op::DeleteText {
                    block_id,
                    start,
                    end,
                    text,
                    removed,
                    old_spans,
                    clipped,
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
                    old_spans,
                    clipped,
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
                old_spans,
                clipped,
            }
        }
        // An empty `old_body` is the only way a caller can say "I did not
        // record the previous body", so it is filled in — but only when the
        // note's current body is non-empty (an empty body round-trips as
        // itself and needs no repair). A non-empty `old_body` is left alone:
        // `apply` rejects it if it disagrees, rather than silently adopting
        // the document's value and losing the caller's mistake.
        Op::SetAnnotationBody {
            annotation_id,
            old_body,
            new_body,
        } => {
            let current = doc
                .annotations
                .iter()
                .find(|annotation| annotation.id == annotation_id)
                .map(|annotation| annotation.body.clone());
            let old_body = match current {
                Some(body) if old_body.is_empty() => body,
                _ => old_body,
            };
            Op::SetAnnotationBody {
                annotation_id,
                old_body,
                new_body,
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

/// `op` with what each delete in it cuts short written down, going by the document as it will be
/// when that delete applies, which for a member of a batch isn't the document it was built from.
pub(crate) fn with_clipped(doc: &Document, op: Op) -> Op {
    match op {
        Op::DeleteText {
            block_id,
            start,
            end,
            text,
            removed,
            old_spans,
            ..
        } => Op::DeleteText {
            clipped: annotations_cut_short(doc, block_id, start, end),
            block_id,
            start,
            end,
            text,
            removed,
            old_spans,
        },
        Op::Batch { ops } => {
            let mut staged = doc.clone();
            let ops = ops
                .into_iter()
                .map(|member| {
                    let member = with_clipped(&staged, member);
                    apply(&mut staged, &member);
                    member
                })
                .collect();
            Op::Batch { ops }
        }
        other => other,
    }
}

/// Every range-scoped note in `block_id` that deleting `start..end` cuts into without removing.
fn annotations_cut_short(
    doc: &Document,
    block_id: BlockId,
    start: usize,
    end: usize,
) -> Vec<Annotation> {
    if start == end {
        return Vec::new();
    }
    doc.annotations
        .iter()
        .filter(|annotation| {
            let Some((anchor_start, anchor_end)) = annotation.anchor.byte_range() else {
                return false;
            };
            annotation.anchor.block_id() == block_id
                && anchor_start < end
                && start < anchor_end
                && !anchor_wholly_inside(anchor_start, anchor_end, start, end)
        })
        .cloned()
        .collect()
}

fn block(doc: &Document, block_id: BlockId) -> &Block {
    doc.blocks
        .iter()
        .find(|block| block.id == block_id)
        .unwrap_or_else(|| panic!("op references unknown block {block_id}"))
}

fn block_mut(doc: &mut Document, block_id: BlockId) -> &mut Block {
    doc.blocks
        .iter_mut()
        .find(|block| block.id == block_id)
        .unwrap_or_else(|| panic!("op references unknown block {block_id}"))
}

/// Every note anchored in `block_id`, in document order.
fn annotations_in_block(doc: &Document, block_id: BlockId) -> Vec<Annotation> {
    doc.annotations
        .iter()
        .filter(|annotation| annotation.anchor.block_id() == block_id)
        .cloned()
        .collect()
}

/// Whether an anchor spanning `anchor_start..anchor_end` is dropped by deleting
/// `start..end`.
///
/// A non-empty range is dropped when its whole span lies inside the deletion.
/// A zero-width (point) anchor is dropped when its position is a deleted byte,
/// i.e. inside the half-open range `[start, end)`: a point at `start` is the
/// first byte deleted, but a point at `end` is the first byte that *survives*
/// and shifts left — so deleting the space before a footnote re-anchors it
/// rather than dropping it.
pub(crate) fn anchor_wholly_inside(
    anchor_start: usize,
    anchor_end: usize,
    start: usize,
    end: usize,
) -> bool {
    if anchor_start == anchor_end {
        anchor_start >= start && anchor_start < end
    } else {
        anchor_start >= start && anchor_end <= end
    }
}

/// Every range-scoped note anchored in `block_id` wholly inside `start..end`.
fn annotations_wholly_inside(
    doc: &Document,
    block_id: BlockId,
    start: usize,
    end: usize,
) -> Vec<Annotation> {
    doc.annotations
        .iter()
        .filter(|annotation| {
            let Some((anchor_start, anchor_end)) = annotation.anchor.byte_range() else {
                return false; // block-scoped anchors are removed by block deletion, not text deletion
            };
            annotation.anchor.block_id() == block_id
                && anchor_wholly_inside(anchor_start, anchor_end, start, end)
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
/// 1. An anchor lying wholly inside the deleted range — a non-empty span, or a
///    zero-width point anchor whose position is inside it — has no text left,
///    so it is dropped and returned (the caller's op payload carries it, so
///    undo can put it back). A point anchor is treated like a range here so
///    that a move-shaped edit (a split, which deletes then re-inserts the tail
///    in a new block) can carry it instead of collapsing it onto the deletion
///    point, which would leave it anchored to the wrong block.
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
        let Some((anchor_start, anchor_end)) = annotation.anchor.byte_range() else {
            return true; // block-scoped anchors survive a text deletion
        };
        let wholly_inside = annotation.anchor.block_id() == block_id
            && anchor_wholly_inside(anchor_start, anchor_end, start, end);
        if wholly_inside {
            dropped.push(annotation.clone());
        }
        !wholly_inside
    });

    for annotation in doc.annotations.iter_mut() {
        let Anchor::Range {
            block_id: anchor_block_id,
            start: anchor_start,
            end: anchor_end,
        } = &mut annotation.anchor
        else {
            continue; // block-scoped anchors have no offsets to remap
        };
        if *anchor_block_id != block_id || *anchor_end <= start || *anchor_start >= end {
            continue; // no overlap with the deleted range
        }
        *anchor_start = map_deleted_offset(*anchor_start, start, end);
        *anchor_end = map_deleted_offset(*anchor_end, start, end);
    }

    shift_anchors(
        &mut doc.annotations,
        block_id,
        end,
        -((end - start) as isize),
    );
    dropped
}

/// Shifts every span whose `start` is at or after `edit_start` by `delta` bytes
/// — the span-level twin of [`shift_anchors`], so bold/italic/link spans move
/// with their text when text is inserted or deleted around them.
fn shift_spans(block: &mut Block, edit_start: usize, delta: isize) {
    if delta == 0 {
        return;
    }
    for span in block.spans.iter_mut() {
        if span.start < edit_start {
            continue;
        }
        span.start = span.start.checked_add_signed(delta).expect(
            "span start overflowed usize: edit_start/delta inconsistent with document text",
        );
        span.end = span
            .end
            .checked_add_signed(delta)
            .expect("span end overflowed usize: edit_start/delta inconsistent with document text");
    }
}

/// Keeps a block's spans consistent with a deletion of `start..end`, mirroring
/// [`rebase_annotations`]: spans wholly inside the range are dropped, straddling
/// spans have their endpoints remapped onto the deletion point, and survivors
/// after the range shift left.
fn rebase_spans(block: &mut Block, start: usize, end: usize) {
    if start == end {
        return;
    }
    block
        .spans
        .retain(|span| !(span.start >= start && span.end <= end));
    for span in block.spans.iter_mut() {
        if span.end <= start || span.start >= end {
            continue;
        }
        span.start = map_deleted_offset(span.start, start, end);
        span.end = map_deleted_offset(span.end, start, end);
    }
    // A straddling span whose surviving text ran out collapses to empty; drop
    // it rather than keep a zero-width span.
    block.spans.retain(|span| span.start < span.end);
    shift_spans(block, end, -((end - start) as isize));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Anchor, AnnotationKind, Span, SpanStyle};
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
            anchor: Anchor::range(block_id, start, end),
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
                old_spans: None,
                clipped: Vec::new(),
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

    /// Inline spans are a whole-value replacement: the inverse swaps the two
    /// span sets, and apply + apply_inverse restore the exact prior spans.
    #[test]
    fn set_spans_invert_swaps_old_and_new() {
        let mut d = doc("bold text");
        let id = d.blocks[0].id;
        let bold = vec![Span {
            start: 0,
            end: 4,
            style: SpanStyle::Bold,
        }];
        let op = Op::set_spans(&d, id, bold.clone());
        assert_eq!(
            op,
            Op::SetSpans {
                block_id: id,
                old_spans: vec![],
                spans: bold.clone(),
            }
        );

        let undone = invert(&op);
        assert_eq!(
            undone,
            Op::SetSpans {
                block_id: id,
                old_spans: bold.clone(),
                spans: vec![],
            }
        );
        assert_eq!(invert(&undone), op, "invert must be involutive");

        apply(&mut d, &op);
        assert_eq!(d.blocks[0].spans, bold);
        apply_inverse(&mut d, &op);
        assert!(d.blocks[0].spans.is_empty());
    }

    /// A body edit is a whole-value replacement, so its inverse swaps the two
    /// bodies; there is no separate restore payload to lose.
    #[test]
    fn invert_set_annotation_body_swaps_old_and_new() {
        let id = Uuid::new_v4();
        let op = Op::SetAnnotationBody {
            annotation_id: id,
            old_body: "before".into(),
            new_body: "after".into(),
        };
        assert_eq!(
            invert(&op),
            Op::SetAnnotationBody {
                annotation_id: id,
                old_body: "after".into(),
                new_body: "before".into(),
            }
        );
        assert_eq!(invert(&invert(&op)), op, "invert must be involutive");
    }

    #[test]
    fn set_annotation_body_round_trips_through_apply_and_invert() {
        let mut d = doc("hello");
        let id = d.blocks[0].id;
        let note = note(id, 0, 5);
        let note_id = note.id;
        d.annotations.push(note);
        let before = d.clone();

        let op = Op::SetAnnotationBody {
            annotation_id: note_id,
            old_body: "note".into(),
            new_body: "a longer note".into(),
        };
        apply(&mut d, &op);
        assert_eq!(d.annotations[0].body, "a longer note");

        apply(&mut d, &invert(&op));
        assert_eq!(d, before);
    }

    /// A body edit whose recorded `old_body` disagrees with the document is a
    /// caller bug: undoing it would restore prose that never existed.
    #[test]
    #[should_panic(expected = "SetAnnotationBody payload old_body does not match")]
    fn set_annotation_body_with_a_wrong_old_body_panics() {
        let mut d = doc("hello");
        let id = d.blocks[0].id;
        let note = note(id, 0, 5);
        let note_id = note.id;
        d.annotations.push(note);
        apply(
            &mut d,
            &Op::SetAnnotationBody {
                annotation_id: note_id,
                old_body: "not the body".into(),
                new_body: "x".into(),
            },
        );
    }

    /// An op that left `old_body` out is repaired at the normalize chokepoint,
    /// the same way a payload-less text delete is.
    #[test]
    fn normalize_fills_a_missing_old_body() {
        let mut d = doc("hello");
        let id = d.blocks[0].id;
        let note = note(id, 0, 5);
        let note_id = note.id;
        d.annotations.push(note);

        let repaired = normalize(
            &d,
            Op::SetAnnotationBody {
                annotation_id: note_id,
                old_body: String::new(),
                new_body: "fresh".into(),
            },
        );
        assert_eq!(
            repaired,
            Op::SetAnnotationBody {
                annotation_id: note_id,
                old_body: "note".into(),
                new_body: "fresh".into(),
            }
        );
        apply(&mut d, &repaired);
        assert_eq!(d.annotations[0].body, "fresh");
    }

    /// A non-empty `old_body` is never rewritten by normalize: a wrong payload
    /// stays wrong and `apply` rejects it.
    #[test]
    fn normalize_leaves_a_non_empty_old_body_alone() {
        let mut d = doc("hello");
        let id = d.blocks[0].id;
        let note = note(id, 0, 5);
        let note_id = note.id;
        d.annotations.push(note);

        let op = Op::SetAnnotationBody {
            annotation_id: note_id,
            old_body: "wrong".into(),
            new_body: "fresh".into(),
        };
        assert_eq!(normalize(&d, op.clone()), op);
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
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().0, 4);
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().1, 6);
    }

    /// An insertion strictly inside a note's range must extend the note's end.
    /// `shift_anchors` skips anchors that start before the edit, so without the
    /// insert-side straddle case a note on "world" would silently keep covering
    /// only "wXorl".
    #[test]
    fn insert_inside_annotation_range_extends_its_end() {
        let mut d = doc("hello world");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 6, 11)); // covers "world"
        let op = Op::InsertText {
            block_id: id,
            offset: 7, // strictly inside 6..11
            text: "X".into(),
        };
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "hello wXorld");
        assert_eq!(
            d.annotations[0].anchor.byte_range().unwrap().0,
            6,
            "start must not move"
        );
        assert_eq!(
            d.annotations[0].anchor.byte_range().unwrap().1,
            12,
            "end must grow by the insert"
        );
        assert_eq!(&d.blocks[0].text[6..12], "wXorld");
    }

    /// The same case with a multi-byte payload: the extension is the byte length.
    #[test]
    fn insert_inside_annotation_range_extends_end_by_byte_len() {
        let mut d = doc("hello world");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 6, 11));
        let op = Op::InsertText {
            block_id: id,
            offset: 8,
            text: "éé".into(), // four bytes
        };
        apply(&mut d, &op);
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().0, 6);
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().1, 15);
    }

    /// Inserting at or before a note's start still shifts both ends (unchanged
    /// behaviour): the insert-side straddle case must not swallow this one.
    #[test]
    fn insert_at_or_before_annotation_start_shifts_both_ends() {
        let mut d = doc("hello world");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 6, 11));

        apply(
            &mut d,
            &Op::InsertText {
                block_id: id,
                offset: 6, // exactly at the note's start
                text: "X".into(),
            },
        );
        assert_eq!(d.blocks[0].text, "hello Xworld");
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().0, 7);
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().1, 12);

        apply(
            &mut d,
            &Op::InsertText {
                block_id: id,
                offset: 0, // well before the note
                text: ">> ".into(),
            },
        );
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().0, 10);
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().1, 15);
    }

    /// Inserting after a note's end leaves it untouched.
    #[test]
    fn insert_after_annotation_end_leaves_it_unchanged() {
        let mut d = doc("hello world");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 0, 5)); // covers "hello"
        let op = Op::InsertText {
            block_id: id,
            offset: 8,
            text: "X".into(),
        };
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "hello woXrld");
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().0, 0);
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().1, 5);
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
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().0, 0);
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().1, 4);

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
        assert_eq!(
            d.annotations.iter().find(|a| a.id == inside.id),
            Some(&inside)
        );
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
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().0, 1); // "b" survives before the edit
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().1, 6); // "gh" closes the range
    }

    #[test]
    fn delete_rebases_annotation_whose_start_lies_inside_range() {
        let mut d = doc("abcdefgh");
        let id = d.blocks[0].id;
        d.annotations.push(note(id, 4, 8)); // covers "efgh", "ef" is deleted

        let op = Op::delete_text(&d, id, 2, 6);
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "abgh");
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().0, 2); // collapses onto the edit start
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().1, 4); // "gh" closed up
    }

    fn link_span(start: usize, end: usize) -> Span {
        Span {
            start,
            end,
            style: SpanStyle::Link {
                url: "http://x.com".into(),
            },
        }
    }

    #[test]
    fn inserting_text_before_a_link_shifts_it_right() {
        let mut d = doc("click here");
        let id = d.blocks[0].id;
        d.blocks[0].spans = vec![link_span(0, 5)];

        apply(
            &mut d,
            &Op::InsertText {
                block_id: id,
                offset: 0,
                text: "!".into(),
            },
        );

        assert_eq!(d.blocks[0].text, "!click here");
        assert_eq!(d.blocks[0].spans[0], link_span(1, 6));
    }

    #[test]
    fn inserting_text_inside_a_link_widens_it() {
        let mut d = doc("click here");
        let id = d.blocks[0].id;
        d.blocks[0].spans = vec![link_span(0, 5)];

        apply(
            &mut d,
            &Op::InsertText {
                block_id: id,
                offset: 3,
                text: "x".into(),
            },
        );

        assert_eq!(d.blocks[0].text, "clixck here");
        assert_eq!(d.blocks[0].spans[0], link_span(0, 6));
    }

    #[test]
    fn deleting_text_before_a_link_shifts_it_left() {
        let mut d = doc("a click");
        let id = d.blocks[0].id;
        d.blocks[0].spans = vec![link_span(2, 7)];

        let op = Op::delete_text(&d, id, 0, 1);
        apply(&mut d, &op);

        assert_eq!(d.blocks[0].text, " click");
        assert_eq!(d.blocks[0].spans[0], link_span(1, 6));
    }

    #[test]
    fn deleting_a_wholly_linked_range_removes_the_link() {
        let mut d = doc("click here");
        let id = d.blocks[0].id;
        d.blocks[0].spans = vec![link_span(0, 5)];

        let op = Op::delete_text(&d, id, 0, 5);
        apply(&mut d, &op);

        assert_eq!(d.blocks[0].text, " here");
        assert!(d.blocks[0].spans.is_empty(), "the link has no text left");
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
        assert_eq!(
            d.annotations[0], before,
            "note before the range is untouched"
        );
        assert_eq!(
            d.annotations[1].anchor.byte_range().unwrap().0,
            4,
            "\"ghi\" closed up by 2"
        );
        assert_eq!(d.annotations[1].anchor.byte_range().unwrap().1, 7);
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

    /// A zero-width (point) anchor inside the deleted range is dropped like a
    /// range anchor, so a move-shaped edit can carry it, and undo restores it.
    #[test]
    fn delete_drops_a_point_anchor_inside_the_range() {
        let mut d = doc("abcdef");
        let id = d.blocks[0].id;
        let point = note(id, 3, 3);
        d.annotations.push(point.clone());

        let op = Op::delete_text(&d, id, 1, 5); // deletes "bcde"
        apply(&mut d, &op);
        assert!(
            d.annotations.is_empty(),
            "a point inside the deleted text is dropped"
        );

        apply_inverse(&mut d, &op);
        assert_eq!(d.blocks[0].text, "abcdef");
        assert!(
            d.annotations.contains(&point),
            "undo restores the point anchor"
        );
    }

    /// A point anchor sitting *on* the deletion boundary survives and shifts
    /// with the text: deleting the space before a footnote re-anchors the
    /// footnote, not dropping it.
    #[test]
    fn delete_reanchors_a_point_anchor_at_the_ranges_end() {
        let mut d = doc("ab c");
        let id = d.blocks[0].id;
        let point = note(id, 3, 3); // at the start of "c", after the space
        d.annotations.push(point.clone());

        let op = Op::delete_text(&d, id, 2, 3); // delete the space
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "abc");
        assert_eq!(
            d.annotations.len(),
            1,
            "the footnote survives the space delete"
        );
        assert_eq!(
            d.annotations[0].anchor.byte_range(),
            Some((2, 2)),
            "the point anchor closes up by one"
        );

        apply_inverse(&mut d, &op);
        assert_eq!(d.blocks[0].text, "ab c");
        assert!(
            d.annotations.contains(&point),
            "undo restores the exact anchor"
        );
    }

    /// A block-scoped anchor (marginalia) survives interior text edits; only
    /// deleting its whole block removes it.
    #[test]
    fn delete_spares_a_block_scoped_anchor() {
        let mut d = doc("abcdef");
        let id = d.blocks[0].id;
        let marginal = Annotation {
            id: Uuid::new_v4(),
            kind: AnnotationKind::Marginal,
            anchor: Anchor::block(id),
            body: "marginal".to_string(),
        };
        d.annotations.push(marginal.clone());

        let op = Op::delete_text(&d, id, 1, 4); // deletes "bcd"
        apply(&mut d, &op);
        assert_eq!(d.blocks[0].text, "aef");
        assert_eq!(
            d.annotations,
            vec![marginal.clone()],
            "the marginal survives an interior delete"
        );

        let del_block = Op::delete_block(&d, 0);
        apply(&mut d, &del_block);
        assert!(
            d.annotations.is_empty(),
            "deleting the block removes its marginal"
        );
    }

    #[test]
    fn delete_leaves_other_block_anchors_alone() {
        let mut d = doc("abc");
        let id = d.blocks[0].id;
        let other = Uuid::new_v4();
        d.annotations.push(note(other, 1, 2));

        let op = Op::delete_text(&d, id, 0, 2);
        apply(&mut d, &op);
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().0, 1);
        assert_eq!(d.annotations[0].anchor.byte_range().unwrap().1, 2);
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
                old_spans: None,
                clipped: Vec::new(),
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
        assert_eq!(
            op,
            Op::DeleteBlock {
                index: 1,
                block: second.clone(),
                annotations: vec![attached.clone()],
            }
        );
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

        let op = Op::set_block_kind(
            &d,
            id,
            BlockKind::ListItem {
                ordered: true,
                depth: 0,
            },
        );
        apply(&mut d, &op);
        assert_eq!(
            d.blocks[0].kind,
            BlockKind::ListItem {
                ordered: true,
                depth: 0
            }
        );

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
            old_spans: None,
            clipped: Vec::new(),
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
                old_spans: None,
                clipped: Vec::new(),
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
                kind: BlockKind::ListItem {
                    ordered: false,
                    depth: 0,
                },
            },
            Op::AddAnnotation {
                annotation: annotation.clone(),
            },
            Op::SetAnnotationBody {
                annotation_id: annotation.id,
                old_body: "a".into(),
                new_body: "b".into(),
            },
            Op::RemoveAnnotation { annotation },
            Op::SetTitle {
                old: "a".into(),
                new: "b".into(),
            },
        ];
        for op in ops {
            let json = serde_json::to_string(&op).unwrap();
            assert_eq!(
                serde_json::from_str::<Op>(&json).unwrap(),
                op,
                "json: {json}"
            );
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
            kind: original.kind,
            anchor: Anchor::range(
                first,
                d.blocks[0].text.len(),
                d.blocks[0].text.len() + second.text.len(),
            ),
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

    /// A batch is atomic: if a later member panics, the members before it must
    /// not remain applied, or the document would be half-edited with no undo
    /// entry that describes that state.
    #[test]
    fn batch_apply_leaves_the_document_untouched_when_a_member_panics() {
        let mut d = doc("one");
        d.blocks.push(Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "two".into(),
            spans: Vec::new(),
        });
        let first = d.blocks[0].id;
        let before = d.clone();

        // The first member would succeed; the second names a block that does
        // not exist, so `apply` panics after the first has already been staged.
        let batch = Op::Batch {
            ops: vec![
                Op::InsertText {
                    block_id: first,
                    offset: 3,
                    text: "X".into(),
                },
                Op::InsertText {
                    block_id: Uuid::new_v4(),
                    offset: 0,
                    text: "Y".into(),
                },
            ],
        };

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            apply(&mut d, &batch);
        }));
        assert!(outcome.is_err(), "the bad member must panic");
        assert_eq!(
            d, before,
            "a failed batch commits nothing: no member's edit survives"
        );
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

    /// History written before deletes kept their spans can drift from the document, and
    /// has to undo without stopping the editor.
    #[test]
    fn set_spans_with_stale_old_spans_still_applies() {
        let mut d = doc("abc");
        let id = d.blocks[0].id;
        let span = Span {
            start: 0,
            end: 1,
            style: SpanStyle::Bold,
        };
        apply(
            &mut d,
            &Op::SetSpans {
                block_id: id,
                old_spans: vec![span.clone()],
                spans: Vec::new(),
            },
        );
        assert!(d.blocks[0].spans.is_empty());
    }

    fn insert(id: Uuid, offset: usize, text: &str) -> Op {
        Op::InsertText {
            block_id: id,
            offset,
            text: text.to_string(),
        }
    }

    fn removal(id: Uuid, start: usize, text: &str) -> Op {
        Op::DeleteText {
            block_id: id,
            start,
            end: start + text.len(),
            text: text.to_string(),
            removed: Vec::new(),
            old_spans: None,
            clipped: Vec::new(),
        }
    }

    fn body(id: Uuid, old: &str, new: &str) -> Op {
        Op::SetAnnotationBody {
            annotation_id: id,
            old_body: old.to_string(),
            new_body: new.to_string(),
        }
    }

    #[test]
    fn typed_letters_join_up() {
        let id = Uuid::new_v4();
        assert_eq!(
            coalesce(&insert(id, 3, "ab"), &insert(id, 5, "c")),
            Some(insert(id, 3, "abc"))
        );
    }

    #[test]
    fn a_space_stays_with_the_word_before_it() {
        let id = Uuid::new_v4();
        assert_eq!(
            coalesce(&insert(id, 0, "ab"), &insert(id, 2, " ")),
            Some(insert(id, 0, "ab "))
        );
        assert_eq!(
            coalesce(&insert(id, 0, "ab "), &insert(id, 3, " ")),
            Some(insert(id, 0, "ab  "))
        );
    }

    #[test]
    fn a_new_word_starts_a_new_entry() {
        let id = Uuid::new_v4();
        assert_eq!(coalesce(&insert(id, 0, "ab "), &insert(id, 3, "c")), None);
    }

    #[test]
    fn typing_elsewhere_or_in_another_block_starts_a_new_entry() {
        let id = Uuid::new_v4();
        assert_eq!(coalesce(&insert(id, 0, "ab"), &insert(id, 7, "c")), None);
        assert_eq!(
            coalesce(&insert(id, 0, "ab"), &insert(Uuid::new_v4(), 2, "c")),
            None
        );
    }

    #[test]
    fn offsets_count_bytes() {
        let id = Uuid::new_v4();
        let joined = coalesce(&insert(id, 0, "é"), &insert(id, 2, "t"));
        assert_eq!(joined, Some(insert(id, 0, "ét")));
    }

    #[test]
    fn only_a_single_typed_character_joins_on() {
        let id = Uuid::new_v4();
        assert_eq!(
            coalesce(&insert(id, 0, "ab"), &insert(id, 2, "pasted")),
            None
        );
    }

    #[test]
    fn a_line_break_starts_a_new_entry() {
        let id = Uuid::new_v4();
        assert_eq!(coalesce(&insert(id, 0, "ab"), &insert(id, 2, "\n")), None);
        assert_eq!(coalesce(&insert(id, 0, "ab\n"), &insert(id, 3, " ")), None);
    }

    #[test]
    fn backspaces_join_up_back_to_the_word_before() {
        let id = Uuid::new_v4();
        assert_eq!(
            coalesce(&removal(id, 5, "c"), &removal(id, 4, "b")),
            Some(removal(id, 4, "bc"))
        );
        assert_eq!(
            coalesce(&removal(id, 4, "bc"), &removal(id, 3, " ")),
            None,
            "the space belongs to the word before it"
        );
    }

    #[test]
    fn forward_deletes_join_up() {
        let id = Uuid::new_v4();
        assert_eq!(
            coalesce(&removal(id, 4, "b"), &removal(id, 4, "c")),
            Some(removal(id, 4, "bc"))
        );
    }

    #[test]
    fn a_joined_delete_keeps_the_spans_from_before_it_began() {
        let id = Uuid::new_v4();
        let span = Span {
            start: 0,
            end: 6,
            style: SpanStyle::Bold,
        };
        let with_spans = |start: usize, text: &str, spans: Vec<Span>| Op::DeleteText {
            block_id: id,
            start,
            end: start + text.len(),
            text: text.to_string(),
            removed: Vec::new(),
            old_spans: Some(spans),
            clipped: Vec::new(),
        };
        let joined = coalesce(
            &with_spans(5, "c", vec![span.clone()]),
            &with_spans(4, "b", Vec::new()),
        );
        assert_eq!(joined, Some(with_spans(4, "bc", vec![span])));
    }

    #[test]
    fn a_delete_that_took_a_note_with_it_stays_alone() {
        let id = Uuid::new_v4();
        let note = Annotation {
            id: Uuid::new_v4(),
            kind: AnnotationKind::Note,
            anchor: Anchor::Range {
                block_id: id,
                start: 4,
                end: 5,
            },
            body: String::new(),
        };
        let took_note = Op::DeleteText {
            block_id: id,
            start: 4,
            end: 5,
            text: "b".to_string(),
            removed: vec![note],
            old_spans: None,
            clipped: Vec::new(),
        };
        assert_eq!(coalesce(&took_note, &removal(id, 3, "a")), None);
        assert_eq!(coalesce(&removal(id, 5, "c"), &took_note), None);
    }

    #[test]
    fn note_edits_join_up_word_by_word() {
        let id = Uuid::new_v4();
        assert_eq!(
            coalesce(&body(id, "", "a"), &body(id, "a", "ab")),
            Some(body(id, "", "ab"))
        );
        assert_eq!(
            coalesce(&body(id, "", "ab "), &body(id, "ab ", "ab c")),
            None
        );
        assert_eq!(
            coalesce(&body(id, "", "a"), &body(Uuid::new_v4(), "", "b")),
            None
        );
    }

    #[test]
    fn different_kinds_of_edit_stay_apart() {
        let id = Uuid::new_v4();
        assert_eq!(coalesce(&insert(id, 0, "a"), &removal(id, 0, "a")), None);
    }

    #[test]
    fn compacting_joins_what_typing_now_joins() {
        let id = Uuid::new_v4();
        let ops = vec![
            insert(id, 0, "a"),
            insert(id, 1, "b"),
            insert(id, 2, " "),
            insert(id, 3, "c"),
            removal(id, 3, "c"),
        ];
        assert_eq!(
            compact(&ops),
            vec![
                insert(id, 0, "ab "),
                insert(id, 3, "c"),
                removal(id, 3, "c")
            ]
        );
    }

    #[test]
    fn compacting_nothing_is_nothing() {
        assert!(compact(&[]).is_empty());
    }
}
