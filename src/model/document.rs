use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type BlockId = Uuid;
pub type AnnotationId = Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKind {
    Paragraph,
    Heading {
        level: u8,
    },
    ListItem {
        ordered: bool,
    },
    /// An indented, italicized quotation.
    BlockQuote,
    /// An image, sized to the body width. `path` is where the file lives on
    /// disk; the block's `text` is empty (the image has no prose).
    Image {
        path: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SpanStyle {
    Bold,
    Italic,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub style: SpanStyle,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub id: BlockId,
    pub kind: BlockKind,
    pub text: String,
    pub spans: Vec<Span>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnnotationKind {
    Marginal,
    Note,
    Footnote,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Anchor {
    /// Anchored to a whole block — used by marginalia. Serializes as the bare
    /// block id (a string), which keeps it distinguishable from a range anchor
    /// and lets older files (which wrote range anchors as
    /// `{block_id, start, end}`) still deserialize as ranges.
    Block(BlockId),
    /// Anchored to a byte range within a block — used by notes (a highlighted
    /// passage) and footnotes (a numbered marker at a position).
    Range {
        block_id: BlockId,
        start: usize,
        end: usize,
    },
}

impl Anchor {
    pub fn block(block_id: BlockId) -> Anchor {
        Anchor::Block(block_id)
    }

    pub fn range(block_id: BlockId, start: usize, end: usize) -> Anchor {
        Anchor::Range {
            block_id,
            start,
            end,
        }
    }

    /// The block this anchor belongs to, whether block- or range-scoped.
    pub fn block_id(&self) -> BlockId {
        match self {
            Anchor::Block(block_id) | Anchor::Range { block_id, .. } => *block_id,
        }
    }

    /// The byte range for range-scoped anchors, `None` for block-scoped ones.
    pub fn byte_range(&self) -> Option<(usize, usize)> {
        match self {
            Anchor::Range { start, end, .. } => Some((*start, *end)),
            Anchor::Block(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub id: AnnotationId,
    pub kind: AnnotationKind,
    pub anchor: Anchor,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub id: Uuid,
    pub title: String,
    pub blocks: Vec<Block>,
    pub annotations: Vec<Annotation>,
}

impl Document {
    pub fn new(title: &str) -> Document {
        Document {
            id: Uuid::new_v4(),
            title: title.to_string(),
            blocks: vec![Block {
                id: Uuid::new_v4(),
                kind: BlockKind::Paragraph,
                text: String::new(),
                spans: Vec::new(),
            }],
            annotations: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_new_has_one_empty_paragraph() {
        let d = Document::new("t");
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].kind, BlockKind::Paragraph);
        assert_eq!(d.blocks[0].text, "");
    }

    #[test]
    fn serde_round_trip_preserves_document() {
        let d = Document {
            id: Uuid::new_v4(),
            title: "Round Trip".to_string(),
            blocks: vec![
                Block {
                    id: Uuid::new_v4(),
                    kind: BlockKind::Heading { level: 1 },
                    text: "Title".to_string(),
                    spans: vec![Span {
                        start: 0,
                        end: 5,
                        style: SpanStyle::Bold,
                    }],
                },
                Block {
                    id: Uuid::new_v4(),
                    kind: BlockKind::ListItem { ordered: true },
                    text: "First".to_string(),
                    spans: Vec::new(),
                },
            ],
            annotations: vec![
                Annotation {
                    id: Uuid::new_v4(),
                    kind: AnnotationKind::Marginal,
                    anchor: Anchor::block(Uuid::new_v4()),
                    body: "note".to_string(),
                },
                Annotation {
                    id: Uuid::new_v4(),
                    kind: AnnotationKind::Note,
                    anchor: Anchor::range(Uuid::new_v4(), 0, 3),
                    body: "range note".to_string(),
                },
            ],
        };
        let json = serde_json::to_string(&d).unwrap();
        let d2: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(d, d2);
    }

    /// Files written before block-anchored marginalia stored every anchor as
    /// `{block_id, start, end}` with no `scope` field. Those must still read
    /// as range anchors, and the newer block anchor reads as a bare id string.
    #[test]
    fn anchor_reads_legacy_range_and_new_block_forms() {
        let id = Uuid::new_v4();

        // Legacy range form (a plain struct, no discriminant).
        let legacy = format!(r#"{{"block_id":"{id}","start":1,"end":4}}"#);
        let anchor: Anchor = serde_json::from_str(&legacy).unwrap();
        assert_eq!(anchor, Anchor::range(id, 1, 4));

        // New block form serializes as the bare id string and reads back.
        let block_json = serde_json::to_string(&Anchor::block(id)).unwrap();
        assert_eq!(block_json, format!(r#""{id}""#));
        let round: Anchor = serde_json::from_str(&block_json).unwrap();
        assert_eq!(round, Anchor::block(id));
    }
}
