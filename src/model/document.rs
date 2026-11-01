use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type BlockId = Uuid;
pub type AnnotationId = Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BlockKind {
    Paragraph,
    Heading { level: u8 },
    ListItem { ordered: bool },
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AnnotationKind {
    Marginal,
    Note,
    Footnote,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub block_id: BlockId,
    pub start: usize,
    pub end: usize,
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
            annotations: vec![Annotation {
                id: Uuid::new_v4(),
                kind: AnnotationKind::Marginal,
                anchor: Anchor {
                    block_id: Uuid::new_v4(),
                    start: 0,
                    end: 3,
                },
                body: "note".to_string(),
            }],
        };
        let json = serde_json::to_string(&d).unwrap();
        let d2: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(d, d2);
    }
}
