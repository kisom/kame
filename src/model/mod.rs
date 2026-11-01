mod anchor;
mod document;

pub use anchor::shift_anchors;
pub use document::{
    Anchor, Annotation, AnnotationId, AnnotationKind, Block, BlockId, BlockKind, Document, Span,
    SpanStyle,
};
