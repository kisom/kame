mod annotations;
mod text;

pub use annotations::{
    place_footnotes, place_marginalia, reading_order, FootnotePlacement, MarginalPlacement,
    FOOTNOTE_GAP, FOOTNOTE_LINE_HEIGHT,
};
pub use text::{
    layout_block, BlockLayout, LayoutConfig, LayoutLine, DEFAULT_BODY_SIZE,
};
