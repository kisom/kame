//! Headless rasterization: laid-out blocks painted into RGBA bitmaps.

mod atlas;
pub mod egui_render;

pub use atlas::{GlyphAtlas, Painting, Patch};
