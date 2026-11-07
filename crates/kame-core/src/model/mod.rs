mod date;
pub use date::Date;
mod anchor;
mod document;
mod offsets;

pub use anchor::shift_anchors;
pub(crate) use offsets::fit_range;
// `Span` is named only in `#[cfg(test)]` code (the layout and export tests
// build spans); non-test code reaches spans through `Block.spans` without
// naming the type. Keep it re-exported for the tests.
#[allow(unused_imports)]
pub use document::{
    bare_link_ranges, fitted_on_reading, has_unknown_fields, has_unknown_fields_held_at,
    holds_newer_values, is_script_url, link_ranges, link_style, list_labels, list_positions,
    AcceptedMark, Anchor, Annotation, AnnotationId, AnnotationKind, Block, BlockId, BlockKind,
    Document, ImageAlign, ListPosition, Metadata, Span, SpanStyle, HEADING_LEVELS, MAX_LIST_DEPTH,
};
