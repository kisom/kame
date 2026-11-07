//! Making the offsets in a document read from a file fit its text. kame's own edits keep every
//! span and anchor in range and on a character boundary, but a damaged or hand-made file needn't,
//! and slicing text at such an offset panics.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::model::document::{Anchor, Block, BlockId, Document};

/// `offset` brought into `text` and back to the character boundary at or before it.
fn fitted(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Whether `start..end` is a range of `text` that can be sliced.
fn fits(text: &str, start: usize, end: usize) -> bool {
    start <= end && end <= text.len() && text.is_char_boundary(start) && text.is_char_boundary(end)
}

/// Fits an anchor's `start..end` to `text`: each brought into it and down to a character boundary,
/// and `end` up to `start` if that leaves it reversed. One that fits is left as it is.
pub(crate) fn fit_range(text: &str, start: &mut usize, end: &mut usize) {
    if fits(text, *start, *end) {
        return;
    }
    *start = fitted(text, *start);
    *end = fitted(text, *end).max(*start);
}

impl Block {
    /// Brings each span into the text, down to character boundaries, and drops one that's left
    /// empty or reversed. A span that already fits is left as it is.
    pub fn fit_spans(&mut self) {
        let text = &self.text;
        self.spans.retain_mut(|span| {
            if fits(text, span.start, span.end) {
                return true;
            }
            span.start = fitted(text, span.start);
            span.end = fitted(text, span.end);
            span.start < span.end
        });
    }
}

impl Document {
    /// Makes a document just read from a file fit the rules its readers rely on, for one that's
    /// damaged or hand-made. A document kame wrote comes back unchanged.
    ///
    /// - A block repeating an earlier block's id is given an id of its own ([`Document::fit_ids`]),
    ///   and so is an annotation repeating an earlier annotation's
    ///   ([`Document::fit_annotation_ids`]).
    /// - Spans are as [`Block::fit_spans`] has them.
    /// - An anchor is brought into its block's text and down to character boundaries, and one left
    ///   reversed becomes a point at its start, as a footnote's is. An anchor whose block is gone,
    ///   a whole-block anchor and one that already fits are left as they are.
    pub fn fit(&mut self) {
        self.fit_ids();
        self.fit_annotation_ids();
        for block in &mut self.blocks {
            block.fit_spans();
        }
        if self.annotations.is_empty() {
            return;
        }
        let texts: HashMap<BlockId, &str> = self
            .blocks
            .iter()
            .map(|block| (block.id, block.text.as_str()))
            .collect();
        for annotation in &mut self.annotations {
            let Anchor::Range {
                block_id,
                start,
                end,
            } = &mut annotation.anchor
            else {
                continue;
            };
            if let Some(text) = texts.get(block_id) {
                fit_range(text, start, end);
            }
        }
    }

    /// Gives each block that repeats an earlier block's id one of its own, so that every block,
    /// and its words, can be told apart: a merge keeps blocks by id. The first keeps the id, and
    /// with it the notes anchored there, as lookups take the first. The new id is worked out from
    /// the old one and which repeat of it the block is, first, second and so on, so every reading
    /// of the same document gives the same, and a block put in or taken out elsewhere, as on
    /// another machine, changes nothing.
    pub fn fit_ids(&mut self) {
        own_ids(self.blocks.iter_mut().map(|block| &mut block.id).collect());
    }

    /// Gives each annotation that repeats an earlier annotation's id one of its own, as
    /// [`Document::fit_ids`] does for blocks, so that taking one out, or a merge, which keeps notes
    /// by id, doesn't take the other with it.
    pub fn fit_annotation_ids(&mut self) {
        own_ids(
            self.annotations
                .iter_mut()
                .map(|annotation| &mut annotation.id)
                .collect(),
        );
    }
}

/// Gives each of `ids` that repeats an earlier one an id of its own, from the old one and which
/// repeat of it it is, and never one already among them.
fn own_ids(mut ids: Vec<&mut Uuid>) {
    let mut taken: HashSet<Uuid> = ids.iter().map(|id| **id).collect();
    if taken.len() == ids.len() {
        return;
    }
    let mut seen: HashSet<Uuid> = HashSet::with_capacity(ids.len());
    let mut repeats: HashMap<Uuid, u128> = HashMap::new();
    for id in ids.iter_mut() {
        if seen.insert(**id) {
            continue;
        }
        let repeat = repeats.entry(**id).or_insert(0);
        *repeat += 1;
        let mut fresh = id.as_u128().wrapping_add(*repeat);
        while taken.contains(&Uuid::from_u128(fresh)) {
            fresh = fresh.wrapping_add(1);
        }
        **id = Uuid::from_u128(fresh);
        taken.insert(**id);
        seen.insert(**id);
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{Anchor, Annotation, AnnotationKind, Block, BlockKind, Document, Span};
    use crate::model::{BlockId, SpanStyle};
    use std::collections::HashSet;
    use uuid::Uuid;

    fn document(text: &str) -> Document {
        let mut doc = Document::new("Damaged");
        doc.blocks[0].text = text.to_string();
        doc
    }

    fn span(start: usize, end: usize) -> Span {
        Span {
            start,
            end,
            style: SpanStyle::Bold,
        }
    }

    fn spans_after(text: &str, spans: Vec<Span>) -> Vec<(usize, usize)> {
        let mut doc = document(text);
        doc.blocks[0].spans = spans;
        doc.fit();
        doc.blocks[0]
            .spans
            .iter()
            .map(|span| (span.start, span.end))
            .collect()
    }

    fn note(block_id: BlockId, start: usize, end: usize) -> Annotation {
        Annotation {
            id: Uuid::new_v4(),
            kind: AnnotationKind::Note,
            anchor: Anchor::range(block_id, start, end),
            body: "a note".to_string(),
        }
    }

    fn anchor_after(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
        let mut doc = document(text);
        doc.annotations.push(note(doc.blocks[0].id, start, end));
        doc.fit();
        doc.annotations[0].anchor.byte_range()
    }

    #[test]
    fn a_span_past_the_end_stops_at_the_end() {
        assert_eq!(spans_after("Café au lait", vec![span(5, 40)]), [(5, 13)]);
    }

    #[test]
    fn a_span_wholly_past_the_end_is_dropped() {
        assert_eq!(spans_after("Café au lait", vec![span(20, 40)]), []);
    }

    #[test]
    fn a_reversed_span_is_dropped() {
        assert_eq!(spans_after("Café au lait", vec![span(8, 3)]), []);
    }

    #[test]
    fn a_span_inside_a_character_moves_back_to_its_start() {
        // "é" is bytes 3..5, and "🙂" bytes 3..7.
        assert_eq!(spans_after("Café au lait", vec![span(0, 4)]), [(0, 3)]);
        assert_eq!(spans_after("hi 🙂 there", vec![span(5, 9)]), [(3, 9)]);
        assert_eq!(spans_after("é", vec![span(1, 2)]), [(0, 2)]);
    }

    #[test]
    fn a_span_left_with_nothing_inside_a_character_is_dropped() {
        assert_eq!(spans_after("hi 🙂 there", vec![span(4, 6)]), []);
    }

    #[test]
    fn an_anchor_past_the_end_stops_at_the_end() {
        assert_eq!(anchor_after("Café au lait", 5, 40), Some((5, 13)));
        assert_eq!(anchor_after("Café au lait", 40, 40), Some((13, 13)));
    }

    #[test]
    fn an_anchor_inside_a_character_moves_back_to_its_start() {
        assert_eq!(anchor_after("Café au lait", 4, 8), Some((3, 8)));
        assert_eq!(anchor_after("hi 🙂 there", 5, 5), Some((3, 3)));
    }

    #[test]
    fn a_reversed_anchor_becomes_a_point_at_its_start() {
        assert_eq!(anchor_after("Café au lait", 8, 3), Some((8, 8)));
        assert_eq!(anchor_after("Café au lait", 40, 3), Some((13, 13)));
    }

    #[test]
    fn an_anchor_in_a_block_thats_gone_and_a_whole_block_anchor_are_left_alone() {
        let mut doc = document("Café au lait");
        doc.annotations.push(note(Uuid::new_v4(), 50, 60));
        doc.annotations.push(Annotation {
            anchor: Anchor::block(doc.blocks[0].id),
            ..note(doc.blocks[0].id, 0, 0)
        });
        let before = doc.clone();
        doc.fit();
        assert_eq!(doc, before);
    }

    /// Block ids should be unique, and where they aren't, a lookup takes the first.
    #[test]
    fn an_anchor_fits_the_first_block_with_its_id() {
        let mut doc = document("Café");
        let mut twin = doc.blocks[0].clone();
        twin.text = "Café au lait".to_string();
        doc.blocks.push(twin);
        doc.annotations.push(note(doc.blocks[0].id, 4, 8));
        doc.fit();
        assert_eq!(doc.annotations[0].anchor.byte_range(), Some((3, 5)));
    }

    #[test]
    fn a_block_repeating_an_id_gets_its_own_the_same_way_each_time() {
        let mut doc = document("One.");
        let id = doc.blocks[0].id;
        for text in ["Two.", "Three."] {
            let mut twin = doc.blocks[0].clone();
            twin.text = text.to_string();
            doc.blocks.push(twin);
        }
        doc.annotations.push(note(id, 0, 3));
        let mut again = doc.clone();
        doc.fit();
        again.fit();

        assert_eq!(doc, again);
        let ids: Vec<BlockId> = doc.blocks.iter().map(|block| block.id).collect();
        assert_eq!(ids[0], id);
        assert!(ids[1] != id && ids[2] != id && ids[1] != ids[2]);
        let texts: Vec<&str> = doc.blocks.iter().map(|block| block.text.as_str()).collect();
        assert_eq!(texts, ["One.", "Two.", "Three."]);
        assert_eq!(doc.annotations[0].anchor.block_id(), id);
    }

    #[test]
    fn a_repeats_new_id_is_the_same_with_a_block_put_in_elsewhere() {
        let mut doc = document("One.");
        let mut twin = doc.blocks[0].clone();
        twin.text = "Two.".to_string();
        doc.blocks.push(twin);
        let mut edited = doc.clone();
        let mut other = doc.blocks[0].clone();
        other.id = Uuid::new_v4();
        other.text = "Put in.".to_string();
        edited.blocks.insert(0, other);
        doc.fit();
        edited.fit();
        assert_eq!(edited.blocks[2].id, doc.blocks[1].id);
    }

    #[test]
    fn a_new_id_is_never_one_already_there() {
        let mut doc = document("One.");
        let id = doc.blocks[0].id;
        let mut twin = doc.blocks[0].clone();
        twin.text = "Two.".to_string();
        doc.blocks.push(twin);
        let mut next = doc.blocks[0].clone();
        // Where the first repeat would go first.
        let own = Uuid::from_u128(id.as_u128().wrapping_add(1));
        next.id = own;
        doc.blocks.push(next);
        doc.fit();
        assert_eq!(
            doc.blocks[2].id, own,
            "a block that didn't repeat keeps its id"
        );
        let ids: HashSet<BlockId> = doc.blocks.iter().map(|block| block.id).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn a_note_repeating_an_id_gets_its_own_the_same_way_each_time() {
        let mut doc = document("Café au lait");
        let block = doc.blocks[0].id;
        let first = note(block, 0, 4);
        let mut twin = note(block, 5, 7);
        twin.id = first.id;
        twin.kind = AnnotationKind::Footnote;
        let mut third = twin.clone();
        third.body = "a third".to_string();
        doc.annotations = vec![first.clone(), twin, third];
        let mut again = doc.clone();
        doc.fit();
        again.fit();

        assert_eq!(doc, again);
        let ids: Vec<Uuid> = doc.annotations.iter().map(|note| note.id).collect();
        assert_eq!(ids[0], first.id, "the first keeps its id");
        assert!(ids[1] != first.id && ids[2] != first.id && ids[1] != ids[2]);
        let bodies: Vec<&str> = doc
            .annotations
            .iter()
            .map(|note| note.body.as_str())
            .collect();
        assert_eq!(bodies, ["a note", "a note", "a third"]);
    }

    #[test]
    fn a_notes_new_id_is_never_another_notes() {
        let mut doc = document("Café au lait");
        let block = doc.blocks[0].id;
        let first = note(block, 0, 4);
        let mut twin = note(block, 5, 7);
        twin.id = first.id;
        // Where the repeat would go first.
        let mut next = note(block, 0, 0);
        next.id = Uuid::from_u128(first.id.as_u128().wrapping_add(1));
        doc.annotations = vec![first, twin, next.clone()];
        doc.fit();
        assert_eq!(
            doc.annotations[2].id, next.id,
            "a note that didn't repeat keeps its id"
        );
        let ids: HashSet<Uuid> = doc.annotations.iter().map(|note| note.id).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn a_document_that_fits_is_unchanged() {
        let mut doc = document("hi 🙂 there");
        let id = doc.blocks[0].id;
        doc.blocks[0].spans = vec![span(0, 2), span(3, 7), span(7, 7), span(13, 13)];
        doc.blocks.push(Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Rule,
            text: String::new(),
            spans: Vec::new(),
        });
        doc.annotations = vec![note(id, 3, 7), note(id, 13, 13), note(id, 0, 0)];
        let before = doc.clone();
        doc.fit();
        assert_eq!(doc, before);
    }
}
