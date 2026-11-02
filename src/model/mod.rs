mod anchor;
mod document;

pub use anchor::shift_anchors;
// `Span` is named only in `#[cfg(test)]` code (the layout and export tests
// build spans); non-test code reaches spans through `Block.spans` without
// naming the type. Keep it re-exported for the tests.
#[allow(unused_imports)]
pub use document::{
    Anchor, Annotation, AnnotationId, AnnotationKind, Block, BlockId, BlockKind, Document, Span,
    SpanStyle,
};
