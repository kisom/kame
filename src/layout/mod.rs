mod annotations;
mod text;

pub use annotations::{
    place_footnotes, place_marginalia, reading_order, FootnotePlacement, MarginalPlacement,
    FOOTNOTE_GAP, FOOTNOTE_LINE_HEIGHT,
};
pub use text::{
    heading_size_for, layout_block_marked, layout_block_with_align, link_ranges, BlockLayout,
    LayoutConfig, LayoutLine, DEFAULT_BODY_SIZE, FOOTNOTE_MARK_EM,
};

#[cfg(test)]
pub use text::layout_block;
