//! Three-way merge of whole documents, by block.
//!
//! Blocks and annotations have ids that survive edits, which makes most of this simple
//! bookkeeping: what one side changed and the other did not is taken. The rest follows three
//! rules. Nothing is lost: a deleted block that the other side edited stays, and a sentence that
//! lost a conflict goes in a note. The result does not depend on which side is passed first. And
//! the same inputs give the same bytes, so two machines reach the same document separately.

use std::collections::{HashMap, HashSet};

use kame_core::model::{
    Anchor, Annotation, AnnotationId, AnnotationKind, Block, BlockId, BlockKind, Document, Span,
};
use uuid::Uuid;

use crate::digest;
use crate::text3::{merge3, Merged3};

/// One of the versions being merged. `when` is when it was written, and the later one's words
/// are the ones left in the text when a sentence conflicts.
#[derive(Clone, Copy, Debug)]
pub struct Side<'a> {
    pub doc: &'a Document,
    pub label: &'a str,
    pub when: i64,
}

#[derive(Clone, Debug)]
pub struct Merged {
    pub doc: Document,
    /// The notes this merge added, each marking something for the author to decide.
    pub notes: Vec<AnnotationId>,
}

pub fn document_digest(doc: &Document) -> String {
    digest(&serde_json::to_vec(doc).expect("a document serializes"))
}

/// An id that two machines making the same note arrive at separately.
fn derived_id(parts: &[&str]) -> Uuid {
    let joined = parts.join("\u{0}");
    let hex = digest(joined.as_bytes());
    let mut bytes = [0u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).expect("hex");
    }
    Uuid::from_bytes(bytes)
}

fn three_way<T: PartialEq + Clone>(base: Option<&T>, ours: &T, theirs: &T) -> T {
    if base == Some(ours) {
        theirs.clone()
    } else {
        ours.clone()
    }
}

/// Merges two versions against `base`, the latest version both descend from, or `None` if there
/// is none known. Then only what they already agree on counts as shared.
pub fn merge(base: Option<&Document>, one: Side, other: Side) -> Merged {
    // Offsets are mapped through the merged text, which takes them fitting their text. What's read
    // from a file or a log is fitted already, but a document on the screen can hold what an undo
    // brought back from its history as it was recorded.
    let fit = |doc: &Document| {
        let mut doc = doc.clone();
        doc.fit();
        doc
    };
    let (base, one_doc, other_doc) = (base.map(fit), fit(one.doc), fit(other.doc));
    let base = base.as_ref();
    let one = Side {
        doc: &one_doc,
        ..one
    };
    let other = Side {
        doc: &other_doc,
        ..other
    };
    let (one_digest, other_digest) = (document_digest(one.doc), document_digest(other.doc));
    if one_digest == other_digest {
        return unchanged(one.doc);
    }
    if let Some(base) = base {
        let base_digest = document_digest(base);
        if base_digest == one_digest {
            return unchanged(other.doc);
        }
        if base_digest == other_digest {
            return unchanged(one.doc);
        }
    }
    // Later first, and the digest settles a tie, so either order of arguments gives one result.
    let one_first = (one.when, &one_digest) > (other.when, &other_digest);
    let (ours, theirs) = if one_first {
        (one, other)
    } else {
        (other, one)
    };

    let index = |doc: &'_ Document| -> HashMap<BlockId, usize> {
        doc.blocks
            .iter()
            .enumerate()
            .map(|(at, block)| (block.id, at))
            .collect()
    };
    let empty = Document {
        id: ours.doc.id,
        title: String::new(),
        blocks: Vec::new(),
        annotations: Vec::new(),
    };
    let base = base.unwrap_or(&empty);
    let (base_at, ours_at, theirs_at) = (index(base), index(ours.doc), index(theirs.doc));
    let base_block = |id: &BlockId| base_at.get(id).map(|at| &base.blocks[*at]);

    let mut blocks: HashMap<BlockId, Block> = HashMap::new();
    let mut maps: HashMap<BlockId, Merged3> = HashMap::new();
    let mut notes: Vec<Annotation> = Vec::new();

    let keep_one_sided =
        |block: &Block, kept_by: &Side, cut_by: &Side, notes: &mut Vec<Annotation>| {
            match base_block(&block.id) {
                // Only one side has it and never did the other: it is new.
                None => Some(block.clone()),
                // The other side cut it, and this side left it alone: the cut stands.
                Some(was) if was == block => None,
                // The other side cut what this side was working on.
                Some(_) => {
                    let body = format!(
                        "Cut on {}, kept because it changed on {}.",
                        cut_by.label, kept_by.label
                    );
                    let id = derived_id(&["cut", &block.id.to_string(), &block.text]);
                    notes.push(Annotation {
                        id,
                        kind: AnnotationKind::Note,
                        anchor: Anchor::Block(block.id),
                        body,
                    });
                    Some(block.clone())
                }
            }
        };

    for block in &ours.doc.blocks {
        let Some(at) = theirs_at.get(&block.id) else {
            if let Some(kept) = keep_one_sided(block, &ours, &theirs, &mut notes) {
                blocks.insert(block.id, kept);
            }
            continue;
        };
        let other_block = &theirs.doc.blocks[*at];
        let was = base_block(&block.id);
        let was_text = was.map_or("", |was| was.text.as_str());
        let text = merge3(was_text, &block.text, &other_block.text);
        for conflict in &text.conflicts {
            let id = derived_id(&[
                "conflict",
                &block.id.to_string(),
                &conflict.base,
                &text.text[conflict.range.clone()],
                &conflict.theirs,
            ]);
            let words = conflict.theirs.trim();
            let body = if words.is_empty() {
                format!("Cut on {}.", theirs.label)
            } else {
                format!("On {}: {words}", theirs.label)
            };
            let anchor = if !conflict.range.is_empty() {
                Anchor::Range {
                    block_id: block.id,
                    start: conflict.range.start,
                    end: conflict.range.end,
                }
            } else {
                Anchor::Block(block.id)
            };
            notes.push(Annotation {
                id,
                kind: AnnotationKind::Note,
                anchor,
                body,
            });
        }
        let kind: BlockKind = three_way(was.map(|was| &was.kind), &block.kind, &other_block.kind);
        let spans = merged_spans(was, block, other_block, &text);
        blocks.insert(
            block.id,
            Block {
                id: block.id,
                kind,
                text: text.text.clone(),
                spans,
            },
        );
        maps.insert(block.id, text);
    }
    for block in &theirs.doc.blocks {
        if !ours_at.contains_key(&block.id) {
            if let Some(kept) = keep_one_sided(block, &theirs, &ours, &mut notes) {
                blocks.insert(block.id, kept);
            }
        }
    }

    let order = merged_order(ours.doc, theirs.doc, &blocks);
    let mut merged_blocks: Vec<Block> = order
        .iter()
        .map(|id| blocks.remove(id).expect("ordered blocks exist"))
        .collect();
    if merged_blocks.is_empty() {
        // Each side cut what the other kept, and neither changed what it kept.
        merged_blocks.push(Block {
            id: derived_id(&["empty", &ours.doc.id.to_string()]),
            kind: BlockKind::Paragraph,
            text: String::new(),
            spans: Vec::new(),
        });
    }

    let annotations =
        merged_annotations(base, &ours, &theirs, &merged_blocks, &maps, notes.clone());
    let doc = Document {
        id: ours.doc.id,
        title: three_way(Some(&base.title), &ours.doc.title, &theirs.doc.title),
        blocks: merged_blocks,
        annotations,
    };
    let kept: HashSet<AnnotationId> = doc.annotations.iter().map(|note| note.id).collect();
    let mut note_ids: Vec<AnnotationId> = notes
        .iter()
        .map(|note| note.id)
        .filter(|id| kept.contains(id))
        .collect();
    note_ids.sort();
    note_ids.dedup();
    Merged {
        doc,
        notes: note_ids,
    }
}

fn unchanged(doc: &Document) -> Merged {
    Merged {
        doc: doc.clone(),
        notes: Vec::new(),
    }
}

/// A span's style as something comparable and sortable.
fn style_key(span: &Span) -> String {
    serde_json::to_string(&span.style).expect("a style serializes")
}

/// What one side changed about formatting wins: a span both have stays, one that one side added
/// is added, and one that one side removed is gone.
fn merged_spans(base: Option<&Block>, ours: &Block, theirs: &Block, text: &Merged3) -> Vec<Span> {
    type Key = (usize, usize, String);
    let keyed = |spans: &[Span], map: &crate::text3::OffsetMap| -> HashMap<Key, Span> {
        spans
            .iter()
            .filter_map(|span| {
                let range = map.span(span.start..span.end)?;
                let moved = Span {
                    start: range.start,
                    end: range.end,
                    style: span.style.clone(),
                };
                Some(((range.start, range.end, style_key(span)), moved))
            })
            .collect()
    };
    let was = base
        .map(|block| keyed(&block.spans, &text.base_map))
        .unwrap_or_default();
    let mine = keyed(&ours.spans, &text.ours_map);
    let other = keyed(&theirs.spans, &text.theirs_map);
    let mut kept: Vec<(Key, Span)> = Vec::new();
    for (key, span) in mine.iter() {
        if other.contains_key(key) || !was.contains_key(key) {
            kept.push((key.clone(), span.clone()));
        }
    }
    for (key, span) in other.iter() {
        if !mine.contains_key(key) && !was.contains_key(key) {
            kept.push((key.clone(), span.clone()));
        }
    }
    kept.sort_by(|a, b| a.0.cmp(&b.0));
    // A span moved through an edit can land beside its old self, so join what touches.
    let mut joined: Vec<(Key, Span)> = Vec::new();
    for (key, span) in kept {
        let same_style = joined.iter_mut().rev().find(|(found, _)| found.2 == key.2);
        match same_style {
            Some((found, into)) if span.start <= into.end => {
                into.end = into.end.max(span.end);
                found.1 = into.end;
            }
            _ => joined.push((key, span)),
        }
    }
    joined.into_iter().map(|(_, span)| span).collect()
}

/// The first side's order, with what only the other side has put after whatever it followed.
fn merged_order(
    ours: &Document,
    theirs: &Document,
    kept: &HashMap<BlockId, Block>,
) -> Vec<BlockId> {
    let mut order: Vec<BlockId> = ours
        .blocks
        .iter()
        .map(|block| block.id)
        .filter(|id| kept.contains_key(id))
        .collect();
    let mut after: Option<usize> = None;
    for block in &theirs.blocks {
        if !kept.contains_key(&block.id) {
            continue;
        }
        match order.iter().position(|id| *id == block.id) {
            Some(at) => after = Some(at),
            None => {
                let at = after.map_or(0, |at| at + 1);
                order.insert(at, block.id);
                after = Some(at);
            }
        }
    }
    order
}

/// An anchor moved into the merged text, or `None` if its block is gone. `side` is the document
/// the anchor is from.
fn moved_anchor(
    anchor: &Anchor,
    side: &Document,
    side_is_ours: bool,
    blocks: &HashMap<BlockId, &Block>,
    maps: &HashMap<BlockId, Merged3>,
) -> Option<Anchor> {
    match anchor {
        Anchor::Block(id) => blocks.contains_key(id).then_some(Anchor::Block(*id)),
        Anchor::Range {
            block_id,
            start,
            end,
        } => {
            let block = blocks.get(block_id)?;
            let Some(text) = maps.get(block_id) else {
                // Only one side has the block, and its text is that side's. A note can name a
                // block its own side doesn't have, and its offsets are into no text there is.
                let own = side.blocks.iter().any(|block| block.id == *block_id);
                return own.then(|| anchor.clone());
            };
            let map = if side_is_ours {
                &text.ours_map
            } else {
                &text.theirs_map
            };
            let start_at = map.start(*start);
            // A point stays a point: a footnote's marker has no extent to widen.
            let end_at = if start == end {
                start_at
            } else {
                map.end(*end).max(start_at)
            };
            debug_assert!(
                block.text.is_char_boundary(start_at) && block.text.is_char_boundary(end_at)
            );
            Some(Anchor::Range {
                block_id: *block_id,
                start: start_at,
                end: end_at,
            })
        }
    }
}

fn merged_annotations(
    base: &Document,
    ours: &Side,
    theirs: &Side,
    blocks: &[Block],
    maps: &HashMap<BlockId, Merged3>,
    notes: Vec<Annotation>,
) -> Vec<Annotation> {
    let by_id = |doc: &'_ Document| -> HashMap<AnnotationId, Annotation> {
        doc.annotations
            .iter()
            .map(|note| (note.id, note.clone()))
            .collect()
    };
    let (was, mine, other) = (by_id(base), by_id(ours.doc), by_id(theirs.doc));
    let placed: HashMap<BlockId, &Block> = blocks.iter().map(|block| (block.id, block)).collect();
    let mut merged: Vec<Annotation> = Vec::new();

    // A note kept when its block is gone goes on the block before it on the side that had it. One
    // whose block is there, kept by the side that doesn't have the note, goes on that block, as its
    // offsets are into the other side's text.
    let rehome = |anchor: &Anchor, doc: &Document| -> Anchor {
        let id = match anchor {
            Anchor::Block(id) => *id,
            Anchor::Range { block_id, .. } => *block_id,
        };
        if placed.contains_key(&id) {
            return Anchor::Block(id);
        }
        let at = doc
            .blocks
            .iter()
            .position(|block| block.id == id)
            .unwrap_or(0);
        let home = doc.blocks[..at]
            .iter()
            .rev()
            .find(|block| placed.contains_key(&block.id))
            .map_or(blocks[0].id, |block| block.id);
        Anchor::Block(home)
    };

    for (id, note) in &mine {
        let base_note = was.get(id);
        if let Some(their_note) = other.get(id) {
            let body = match base_note {
                Some(old) if old.body == note.body => their_note.body.clone(),
                Some(old) if old.body == their_note.body => note.body.clone(),
                _ if note.body == their_note.body => note.body.clone(),
                _ => format!("{}\n\nOn {}: {}", note.body, theirs.label, their_note.body),
            };
            let anchor = moved_anchor(&note.anchor, ours.doc, true, &placed, maps)
                .or_else(|| moved_anchor(&their_note.anchor, theirs.doc, false, &placed, maps))
                .unwrap_or_else(|| rehome(&note.anchor, ours.doc));
            merged.push(Annotation {
                id: *id,
                kind: note.kind,
                anchor,
                body,
            });
        } else if base_note.is_none_or(|old| old.body != note.body) {
            let anchor = moved_anchor(&note.anchor, ours.doc, true, &placed, maps)
                .unwrap_or_else(|| rehome(&note.anchor, ours.doc));
            merged.push(Annotation {
                id: *id,
                kind: note.kind,
                anchor,
                body: note.body.clone(),
            });
        }
    }
    for (id, note) in &other {
        if mine.contains_key(id) {
            continue;
        }
        if was.get(id).is_none_or(|old| old.body != note.body) {
            let anchor = moved_anchor(&note.anchor, theirs.doc, false, &placed, maps)
                .unwrap_or_else(|| rehome(&note.anchor, theirs.doc));
            merged.push(Annotation {
                id: *id,
                kind: note.kind,
                anchor,
                body: note.body.clone(),
            });
        }
    }
    for note in notes {
        if placed.contains_key(&anchor_block(&note.anchor)) {
            merged.push(note);
        }
    }
    merged.sort_by_key(|note| note.id);
    merged.dedup_by_key(|note| note.id);
    merged
}

fn anchor_block(anchor: &Anchor) -> BlockId {
    match anchor {
        Anchor::Block(id) => *id,
        Anchor::Range { block_id, .. } => *block_id,
    }
}
