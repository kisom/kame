//! Making the offsets in a document read from a file fit its text. kame's own edits keep every
//! span and anchor in range and on a character boundary, but a damaged or hand-made file needn't,
//! and slicing text at such an offset panics.

use std::collections::HashMap;

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
    /// Makes every span and range anchor fit its block's text, for a document just read from a
    /// file. Spans are as [`Block::fit_spans`] has them. An anchor is brought into its block's text
    /// and down to character boundaries, and one left reversed becomes a point at its start, as a
    /// footnote's is. An anchor whose block is gone, a whole-block anchor and anything that already
    /// fits are left as they are, so a document kame wrote comes back unchanged.
    pub fn fit_offsets(&mut self) {
        for block in &mut self.blocks {
            block.fit_spans();
        }
        if self.annotations.is_empty() {
            return;
        }
        // Lookups take the first block with an id, which matters only in a file that repeats one.
        let mut texts: HashMap<BlockId, &str> = HashMap::new();
        for block in &self.blocks {
            texts.entry(block.id).or_insert(block.text.as_str());
        }
        for annotation in &mut self.annotations {
            let Anchor::Range {
                block_id,
                start,
                end,
            } = &mut annotation.anchor
            else {
                continue;
            };
            let Some(text) = texts.get(block_id) else {
                continue;
            };
            if fits(text, *start, *end) {
                continue;
            }
            *start = fitted(text, *start);
            *end = fitted(text, *end).max(*start);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{Anchor, Annotation, AnnotationKind, Block, BlockKind, Document, Span};
    use crate::model::{BlockId, SpanStyle};
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
        doc.fit_offsets();
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
        doc.fit_offsets();
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
        doc.fit_offsets();
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
        doc.fit_offsets();
        assert_eq!(doc.annotations[0].anchor.byte_range(), Some((3, 5)));
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
        doc.fit_offsets();
        assert_eq!(doc, before);
    }
}
