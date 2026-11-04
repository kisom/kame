//! How evenly kame sets the paragraphs of some documents, as counts only, so the numbers in
//! `layout/breaks.rs` can be checked against new writing:
//!
//! ```sh
//! cargo run --release -p kame-typeset --example spacing -- ESSAY.kame [BODY_SIZE [WIDTH]]
//! ```
//!
//! The width follows the window in the app, and 565 is the width at its first window size.
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use cosmic_text::{Align, FontSystem};
use kame_core::model::{AnnotationKind, Block, BlockKind};
use kame_core::storage::Db;
use kame_typeset::layout::{
    layout_block_marked, layout_block_with_align, BlockLayout, LayoutConfig,
};

#[derive(Default)]
struct Tally {
    spaces: Vec<f32>,
    lone_words: usize,
    lines: usize,
    hyphens: usize,
}

impl Tally {
    fn line(&mut self, shares: &[f32], ends_short: bool) {
        self.lines += 1;
        if shares.is_empty() && ends_short {
            self.lone_words += 1;
        }
        // Every space on a line shares its stretch, so a line counts once.
        if let Some(share) = shares.first() {
            self.spaces.push(*share);
        }
    }

    fn show(&self, name: &str) {
        let count = self.spaces.len().max(1) as f32;
        let over = |x: f32| 100.0 * self.spaces.iter().filter(|s| **s > x).count() as f32 / count;
        let tight = 100.0 * self.spaces.iter().filter(|s| **s < 0.9).count() as f32 / count;
        let widest = self.spaces.iter().fold(0.0_f32, |m, s| m.max(*s));
        let least = self.spaces.iter().fold(9.0_f32, |m, s| m.min(*s));
        println!(
            "  {name:<6} {:>4} lines  over 1.5x {:>5.1}%  over 2x {:>4.1}%  widest {widest:.2}x  \
             tightest {least:.2}x  under 0.9x {tight:>4.1}%  words alone {}  hyphens {}",
            self.lines,
            over(1.5),
            over(2.0),
            self.lone_words,
            self.hyphens,
        );
    }
}

fn is_space(block: &Block, base: usize, start: usize, end: usize) -> bool {
    block.text[base + start..base + end].trim().is_empty()
}

fn tally_set(tally: &mut Tally, block: &Block, layout: &BlockLayout, natural: f32, width: f32) {
    let count = layout.runs.len();
    for (index, run) in layout.runs.iter().enumerate() {
        if index + 1 == count
            || layout.runs[index + 1].line_i != run.line_i
            || run.glyphs.is_empty()
        {
            continue;
        }
        let base = layout.line_start(run.line_i);
        tally.hyphens += run.glyphs.iter().filter(|g| g.start == g.end).count();
        let shares: Vec<f32> = run
            .glyphs
            .iter()
            .filter(|g| g.start < g.end && is_space(block, base, g.start, g.end))
            .map(|g| g.w / natural)
            .collect();
        let last = run.glyphs.last().unwrap();
        tally.line(&shares, last.x + last.w < width - 1.0);
    }
}

/// What first fit makes of a block, from the ragged lines the engine sets.
fn tally_today(
    fs: &mut FontSystem,
    tally: &mut Tally,
    block: &Block,
    config: &LayoutConfig,
    natural: f32,
) {
    let width = config.body_width;
    let layout = layout_block_with_align(fs, block, config, Align::Left);
    let count = layout.runs.len();
    for (index, run) in layout.runs.iter().enumerate() {
        if index + 1 == count
            || layout.runs[index + 1].line_i != run.line_i
            || run.glyphs.is_empty()
        {
            continue;
        }
        let base = layout.line_start(run.line_i);
        let spaces = run
            .glyphs
            .iter()
            .filter(|g| is_space(block, base, g.start, g.end))
            .count();
        let last = run.glyphs.last().unwrap();
        let end = last.x + last.w;
        let share = 1.0 + (width - end) / (spaces.max(1) as f32 * natural);
        let shares = if spaces == 0 { Vec::new() } else { vec![share] };
        tally.line(&shares, end < width - 1.0);
    }
}

fn starts(layout: &BlockLayout) -> Vec<usize> {
    layout.lines.iter().map(|line| line.start).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: spacing DOC.kame [BODY_SIZE]");
    let size: f32 = args
        .get(2)
        .map_or(16.0, |s| s.parse().expect("a body size"));
    let db = Db::open_read_only(Path::new(path)).expect("a kame document");
    let doc = db.load_document().expect("a kame document");

    let mut fs = kame_typeset::render::egui_render::font_system();
    let mut config = LayoutConfig::with_body_size(size);
    if let Some(width) = args.get(3) {
        config.body_width = width.parse().expect("a width");
    }
    let width = config.body_width;

    let mut marks: HashMap<_, Vec<usize>> = HashMap::new();
    for annotation in &doc.annotations {
        if annotation.kind == AnnotationKind::Footnote {
            if let Some((_, end)) = annotation.anchor.byte_range() {
                marks
                    .entry(annotation.anchor.block_id())
                    .or_default()
                    .push(end);
            }
        }
    }

    for offsets in marks.values_mut() {
        offsets.sort_unstable();
    }

    let space_block = Block {
        kind: BlockKind::Paragraph,
        text: "a b".to_string(),
        ..doc.blocks.first().expect("a block").clone()
    };
    let natural =
        layout_block_with_align(&mut fs, &space_block, &config, Align::Left).runs[0].glyphs[1].w;

    let paragraphs: Vec<&Block> = doc
        .blocks
        .iter()
        .filter(|block| matches!(block.kind, BlockKind::Paragraph) && !block.text.is_empty())
        .collect();
    let words: usize = paragraphs
        .iter()
        .map(|b| b.text.split_whitespace().count())
        .sum();
    println!(
        "{} paragraphs, {words} words, type {size}, width {width}",
        paragraphs.len()
    );

    let (mut today, mut set) = (Tally::default(), Tally::default());
    let (mut slowest, mut slowest_engine) = (0.0_f64, 0.0_f64);
    for block in &paragraphs {
        let block_marks = marks.get(&block.id).map_or(&[][..], Vec::as_slice);
        let began = Instant::now();
        tally_today(&mut fs, &mut today, block, &config, natural);
        slowest_engine = slowest_engine.max(began.elapsed().as_secs_f64());
        let began = Instant::now();
        let layout = layout_block_marked(&mut fs, block, &config, block_marks);
        slowest = slowest.max(began.elapsed().as_secs_f64());
        tally_set(&mut set, block, &layout, natural, width);
    }
    today.show("today");
    set.show("now");
    println!(
        "  slowest paragraph to lay out: {:.1} ms, against {:.1} ms for the engine",
        slowest * 1000.0,
        slowest_engine * 1000.0
    );

    // Typing word by word, and removing a word, without the lines nearest the edit.
    let (mut typed, mut typed_moved, mut removed, mut removed_moved) = (0, 0, 0, 0);
    for block in &paragraphs {
        let text = &block.text;
        let spaces: Vec<usize> = text.match_indices(' ').map(|(at, _)| at).collect();
        let mut before: Option<Vec<usize>> = None;
        for at in &spaces {
            let mut edited = (*block).clone();
            edited.text = text[..*at].to_string();
            edited.spans.clear();
            let now = starts(&layout_block_marked(&mut fs, &edited, &config, &[]));
            if let Some(before) = &before {
                typed += 1;
                let keep = before.len().saturating_sub(2);
                typed_moved += usize::from((0..keep).any(|i| before.get(i) != now.get(i)));
            }
            before = Some(now);
        }
        let mut plain = (*block).clone();
        plain.spans.clear();
        let whole = starts(&layout_block_marked(&mut fs, &plain, &config, &[]));
        for pair in spaces.windows(2) {
            let mut edited = plain.clone();
            edited.text = format!("{}{}", &text[..pair[0]], &text[pair[1]..]);
            let now = starts(&layout_block_marked(&mut fs, &edited, &config, &[]));
            let above = whole
                .iter()
                .filter(|start| **start <= pair[0])
                .count()
                .saturating_sub(1);
            removed += 1;
            removed_moved += usize::from((0..above).any(|i| whole.get(i) != now.get(i)));
        }
    }
    println!(
        "  words typed that move an earlier line: {typed_moved}/{typed}; \
         words removed that move a line above: {removed_moved}/{removed}"
    );
}
