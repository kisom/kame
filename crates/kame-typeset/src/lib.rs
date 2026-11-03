//! How a kame document looks: shaping blocks into lines, placing marginalia and footnotes,
//! and rasterizing the glyphs for egui. It reads documents and never changes them.

use kame_core::model;

pub mod layout;
pub mod render;
