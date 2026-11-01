//! Headless annotation layout: where margin notes and footnotes sit on the page.
//!
//! Positions are in block/page pixel space with Y growing downward and the
//! body measure starting at x = 0, so a caller places a margin note at
//! `(body_left + x, block_top + y)` and a footnote at `(body_left, block_top + y)`.

use std::borrow::Borrow;

use crate::layout::text::{anchor_to_y, BlockLayout, LayoutConfig};
use crate::model::{Annotation, AnnotationId, AnnotationKind, Block};

/// Vertical gap between the last line of the body and the first footnote,
/// leaving room for the rule that separates prose from apparatus.
pub const FOOTNOTE_GAP: f32 = 24.0;
/// Baseline-to-baseline advance between consecutive footnote entries.
pub const FOOTNOTE_LINE_HEIGHT: f32 = 18.0;
/// Clear space between one margin body's last line and the next placement
/// below it.
pub const MARGIN_GAP: f32 = 6.0;

/// A margin-column position for one marginal or note annotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarginalPlacement {
    pub annotation_id: AnnotationId,
    pub x: f32,
    pub y: f32,
}

/// A footnote entry number and its Y at the foot of the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FootnotePlacement {
    pub annotation_id: AnnotationId,
    pub number: usize,
    pub y: f32,
}

/// Place every `Marginal` and `Note` annotation in the margin column, at the Y
/// of the body line its anchor starts on.
///
/// All placements share one x: the margin column begins one `margin_size`
/// gutter to the right of the body measure. Annotations are returned in column
/// order (ascending Y, ties in document order). When a note would land inside
/// the previous note's rendered body, it is pushed down below that body, so
/// crowded anchors stack downward instead of overprinting.
///
/// The advance is the height the previous body actually occupies plus
/// [`MARGIN_GAP`], floored by [`LayoutConfig::body_line_height`] and
/// `margin_size`. A one-line advance is not enough: a margin body can wrap to
/// half a dozen lines in a narrow gutter, and the next placement would then
/// print on top of its last lines. `body_heights` carries the shaped height of
/// each annotation's rendered body, keyed by annotation id, as measured by the
/// caller; annotations missing from it (a `Note`, whose body lives in a popover,
/// or a body that has not been shaped yet) fall back to the one-line floor.
///
/// Annotations whose anchor does not resolve to a line (unknown block, empty
/// block, offset past the text) are omitted rather than guessed at.
///
/// Y is relative to the top of the anchor's own block (`anchor_to_y` works in
/// block space), so a caller adds the block's page offset. Collision stacking
/// therefore compares block-relative Y: notes anchored in different blocks that
/// happen to sit at the same in-block Y are separated even if the blocks are far
/// apart on the page. That is conservative — it avoids overprinting for notes
/// in one block but is only approximate across blocks — and the v1 document
/// flow keeps the error small.
pub fn place_marginalia<L: Borrow<BlockLayout>>(
    annotations: &[Annotation],
    blocks: &[Block],
    layouts: &[L],
    body_heights: &[(AnnotationId, f32)],
    config: &LayoutConfig,
) -> Vec<MarginalPlacement> {
    let x = config.body_width + config.margin_size;

    // (document index, anchor Y) for every margin annotation whose anchor
    // resolves to a line.
    let mut anchored: Vec<(usize, f32)> = annotations
        .iter()
        .enumerate()
        .filter(|(_, annotation)| is_margin(&annotation.kind))
        .filter_map(|(index, annotation)| {
            let y = anchor_to_y(
                blocks,
                layouts,
                annotation.anchor.block_id,
                annotation.anchor.start,
            )?;
            Some((index, y))
        })
        .collect();

    // Column order: top to bottom, ties in document order so stacking is
    // deterministic rather than dependent on sort stability.
    anchored.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));

    // Floors for the advance: the body line height is the typical height of the
    // note text, and a non-positive margin_size cannot separate notes on its
    // own. Whichever is larger wins, then the previous body's real height.
    let floor = config.margin_size.max(config.body_line_height);
    let body_height = |id: AnnotationId| {
        body_heights
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, height)| *height)
            .unwrap_or(0.0)
            .max(0.0)
    };

    let mut placements = Vec::with_capacity(anchored.len());
    // Y and body height of the placement above this one.
    let mut previous: Option<(f32, f32)> = None;
    for (index, anchor_y) in anchored {
        let id = annotations[index].id;
        let height = body_height(id);
        let y = match previous {
            Some((previous_y, previous_height)) => {
                let advance = floor.max(previous_height + MARGIN_GAP);
                if anchor_y < previous_y + advance {
                    previous_y + advance
                } else {
                    anchor_y
                }
            }
            None => anchor_y,
        };
        previous = Some((y, height));
        placements.push(MarginalPlacement {
            annotation_id: id,
            x,
            y,
        });
    }
    placements
}

/// Kinds that live in the margin column: stand-alone marginalia and the margin
/// marker for a note on a body range.
fn is_margin(kind: &AnnotationKind) -> bool {
    matches!(kind, AnnotationKind::Marginal | AnnotationKind::Note)
}

/// Number every `Footnote` in document order (1-based) and stack the entries at
/// the foot of the page, starting [`FOOTNOTE_GAP`] below `doc_height` and
/// advancing by [`FOOTNOTE_LINE_HEIGHT`] per entry.
///
/// `annotations` is read in document order; only `Footnote`s take a number, so
/// interleaved marginalia and notes do not leave gaps in the numbering. Callers
/// must pass annotations in reading order — see [`reading_order`]; the stored
/// vector is in creation order, which is not the same thing.
pub fn place_footnotes(annotations: &[Annotation], doc_height: f32) -> Vec<FootnotePlacement> {
    annotations
        .iter()
        .filter(|annotation| annotation.kind == AnnotationKind::Footnote)
        .enumerate()
        .map(|(index, annotation)| FootnotePlacement {
            annotation_id: annotation.id,
            number: index + 1,
            y: doc_height + FOOTNOTE_GAP + index as f32 * FOOTNOTE_LINE_HEIGHT,
        })
        .collect()
}

/// Copy `annotations` into reading order: by block position in the document,
/// then by anchor start.
///
/// `Document::annotations` is in creation order, so a footnote added to an
/// earlier paragraph after one below it would otherwise take the higher number.
/// An annotation whose block is gone sorts last (it cannot be laid out).
pub fn reading_order(annotations: &[Annotation], blocks: &[Block]) -> Vec<Annotation> {
    let block_index = |id| {
        blocks
            .iter()
            .position(|block| block.id == id)
            .unwrap_or(usize::MAX)
    };
    let mut ordered: Vec<Annotation> = annotations.to_vec();
    ordered.sort_by_key(|annotation| {
        (
            block_index(annotation.anchor.block_id),
            annotation.anchor.start,
            annotation.anchor.end,
        )
    });
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::text::layout_block;
    use crate::model::{Anchor, BlockKind};
    use cosmic_text::FontSystem;
    use std::path::PathBuf;
    use uuid::Uuid;

    const PARA: &str = "Typography is the craft of endowing human language with a durable \
visual form, and thus with an independent existence. Its heartwood is calligraphy: the \
dance, on a page, of pen, eye and hand.";

    fn font_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fonts")
            .join(name)
    }

    fn load_fonts(fs: &mut FontSystem) {
        for file in [
            "EBGaramond-Regular.otf",
            "EBGaramond-Bold.otf",
            "EBGaramond-Italic.otf",
            "CrimsonPro-Regular.ttf",
        ] {
            fs.db_mut()
                .load_font_file(font_path(file))
                .unwrap_or_else(|e| panic!("load {file}: {e}"));
        }
    }

    fn config() -> LayoutConfig {
        LayoutConfig {
            body_width: 300.0,
            body_size: 16.0,
            body_line_height: 24.0,
            heading_size: 28.0,
            margin_width: 120.0,
            margin_size: 12.0,
        }
    }

    /// One shaped paragraph and its layout, in the block order callers pass.
    fn laid_out(text: &str) -> (Vec<Block>, Vec<BlockLayout>) {
        let mut fs = FontSystem::new();
        load_fonts(&mut fs);
        let block = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: text.to_string(),
            spans: Vec::new(),
        };
        let layout = layout_block(&mut fs, &block, &config());
        (vec![block], vec![layout])
    }

    fn annotation(kind: AnnotationKind, block_id: Uuid, start: usize) -> Annotation {
        Annotation {
            id: Uuid::new_v4(),
            kind,
            anchor: Anchor {
                block_id,
                start,
                end: start,
            },
            body: "a note".to_string(),
        }
    }

    fn marginal(block_id: Uuid, start: usize) -> Annotation {
        annotation(AnnotationKind::Marginal, block_id, start)
    }

    fn note(block_id: Uuid, start: usize) -> Annotation {
        annotation(AnnotationKind::Note, block_id, start)
    }

    fn footnote(block_id: Uuid, start: usize) -> Annotation {
        annotation(AnnotationKind::Footnote, block_id, start)
    }

    /// Byte offset in the middle of `line`, guaranteed to map back to it.
    fn offset_in(layouts: &[BlockLayout], index: usize) -> usize {
        let line = layouts[0].lines[index];
        (line.start + line.end) / 2
    }

    #[test]
    fn marginalia_place_at_anchor_y() {
        let (blocks, layouts) = laid_out(PARA);
        let config = config();
        assert!(layouts[0].lines.len() >= 3, "paragraph should wrap");

        let ann = marginal(blocks[0].id, offset_in(&layouts, 1));
        let placed = place_marginalia(&[ann.clone()], &blocks, &layouts, &[], &config);

        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].annotation_id, ann.id);
        assert_eq!(
            placed[0].y, layouts[0].lines[1].y,
            "marginalia must sit on its anchor line"
        );
        assert!(
            placed[0].x >= config.body_width,
            "marginalia belong in the margin, right of the text measure"
        );
        assert_eq!(placed[0].x, config.body_width + config.margin_size);
    }

    #[test]
    fn colliding_marginalia_stack_downward() {
        let (blocks, layouts) = laid_out(PARA);
        let config = config();
        let offset = offset_in(&layouts, 1);
        let first = marginal(blocks[0].id, offset);
        let second = marginal(blocks[0].id, offset);

        let placed = place_marginalia(&[first.clone(), second.clone()], &blocks, &layouts, &[], &config);
        assert_eq!(placed.len(), 2);

        let a = placed
            .iter()
            .find(|p| p.annotation_id == first.id)
            .expect("first marginal placed");
        let b = placed
            .iter()
            .find(|p| p.annotation_id == second.id)
            .expect("second marginal placed");

        assert_eq!(
            a.y, layouts[0].lines[1].y,
            "the first note keeps its anchor line"
        );
        assert!(b.y > a.y, "a colliding note must be pushed down");
        assert!(
            (b.y - a.y - config.body_line_height).abs() < 0.01,
            "stacking advance is max(margin_size, body_line_height), got {}",
            b.y - a.y
        );
        assert_eq!(a.x, b.x, "marginalia share one column");
    }

    #[test]
    fn a_wrapped_body_pushes_the_next_marginal_past_all_of_its_lines() {
        let (blocks, layouts) = laid_out(PARA);
        let config = config();
        let offset = offset_in(&layouts, 1);
        let first = marginal(blocks[0].id, offset);
        let second = marginal(blocks[0].id, offset);

        // A six-line body in the narrow gutter: the one-line advance that used
        // to be the only clearance would print the second note over the last
        // five lines of the first.
        let six_lines = config.body_line_height * 6.0;
        assert!(six_lines > config.body_line_height);
        let heights = [(first.id, six_lines)];

        let placed = place_marginalia(
            &[first.clone(), second.clone()],
            &blocks,
            &layouts,
            &heights,
            &config,
        );
        let a = placed
            .iter()
            .find(|p| p.annotation_id == first.id)
            .expect("first marginal placed");
        let b = placed
            .iter()
            .find(|p| p.annotation_id == second.id)
            .expect("second marginal placed");

        assert_eq!(
            a.y, layouts[0].lines[1].y,
            "the first note still keeps its anchor line"
        );
        assert!(
            b.y - a.y >= six_lines + MARGIN_GAP - 0.01,
            "the next note must clear the first body's six lines plus the gap; advance was {}",
            b.y - a.y
        );

        // A body shorter than one line must not shrink the advance below the
        // one-line floor.
        let short = [(first.id, 4.0)];
        let placed = place_marginalia(
            &[first.clone(), second],
            &blocks,
            &layouts,
            &short,
            &config,
        );
        assert!(
            (placed[1].y - placed[0].y - config.body_line_height).abs() < 0.01,
            "a short body still advances by a full line, got {}",
            placed[1].y - placed[0].y
        );
    }

    #[test]
    fn stacking_advance_uses_the_larger_of_gutter_and_line() {
        let (blocks, layouts) = laid_out(PARA);
        let mut config = config();
        config.margin_size = 40.0;
        let offset = offset_in(&layouts, 1);
        let first = marginal(blocks[0].id, offset);
        let second = marginal(blocks[0].id, offset);

        let placed = place_marginalia(&[first, second], &blocks, &layouts, &[], &config);
        assert_eq!(placed.len(), 2);
        assert!(
            (placed[1].y - placed[0].y - 40.0).abs() < 0.01,
            "a gutter wider than a line sets the advance"
        );
    }

    #[test]
    fn footnotes_numbered_in_order() {
        let block_id = Uuid::new_v4();
        let first = footnote(block_id, 0);
        let second = footnote(block_id, 10);
        let doc_height = 800.0;

        let placed = place_footnotes(&[first.clone(), second.clone()], doc_height);
        assert_eq!(placed.len(), 2);

        let numbers: Vec<usize> = placed.iter().map(|p| p.number).collect();
        assert_eq!(numbers, vec![1, 2]);
        assert_eq!(placed[0].annotation_id, first.id);
        assert_eq!(placed[1].annotation_id, second.id);

        assert!(
            placed.iter().all(|p| p.y > doc_height),
            "footnotes sit below the body"
        );
        assert!(placed[1].y > placed[0].y, "entries march down the foot");
    }

    #[test]
    fn notes_share_the_margin_column() {
        let (blocks, layouts) = laid_out(PARA);
        let config = config();
        let n = note(blocks[0].id, offset_in(&layouts, 0));

        let placed = place_marginalia(&[n.clone()], &blocks, &layouts, &[], &config);
        assert_eq!(placed.len(), 1, "a Note also marks the margin");
        assert_eq!(placed[0].annotation_id, n.id);
        assert_eq!(placed[0].x, config.body_width + config.margin_size);
    }

    #[test]
    fn marginalia_skip_footnotes_and_unknown_blocks() {
        let (blocks, layouts) = laid_out(PARA);
        let config = config();
        let foot = footnote(blocks[0].id, 0);
        let orphan = marginal(Uuid::new_v4(), 0);

        let placed = place_marginalia(&[foot, orphan], &blocks, &layouts, &[], &config);
        assert!(
            placed.is_empty(),
            "only resolvable margin annotations place"
        );
    }

    #[test]
    fn far_apart_marginalia_keep_their_anchor_lines() {
        let (blocks, layouts) = laid_out(PARA);
        let config = config();
        let lines = &layouts[0].lines;
        assert!(lines.len() >= 3);
        let top = marginal(blocks[0].id, offset_in(&layouts, 0));
        let bottom = marginal(blocks[0].id, offset_in(&layouts, lines.len() - 1));

        let placed = place_marginalia(&[top.clone(), bottom.clone()], &blocks, &layouts, &[], &config);
        assert_eq!(placed.len(), 2);
        assert_eq!(placed[0].annotation_id, top.id);
        assert_eq!(placed[0].y, lines[0].y);
        assert_eq!(placed[1].annotation_id, bottom.id);
        assert_eq!(
            placed[1].y,
            lines[lines.len() - 1].y,
            "notes that do not crowd keep their anchor Y"
        );
    }

    #[test]
    fn footnote_numbering_counts_only_footnotes() {
        let block_id = Uuid::new_v4();
        let anns = vec![
            marginal(block_id, 0),
            footnote(block_id, 5),
            note(block_id, 6),
            footnote(block_id, 12),
        ];
        let placed = place_footnotes(&anns, 500.0);
        assert_eq!(placed.len(), 2);
        assert_eq!(placed[0].number, 1);
        assert_eq!(placed[1].number, 2);
        assert_eq!(placed[0].annotation_id, anns[1].id);
        assert_eq!(placed[1].annotation_id, anns[3].id);
    }

    #[test]
    fn reading_order_sorts_by_block_then_anchor() {
        let first_block = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "first paragraph".to_string(),
            spans: Vec::new(),
        };
        let second_block = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "second paragraph".to_string(),
            spans: Vec::new(),
        };
        let blocks = vec![first_block, second_block];

        // Created bottom-first: creation order is second block, then first.
        let below = footnote(blocks[1].id, 0);
        let above = footnote(blocks[0].id, 4);
        let ordered = reading_order(&[below.clone(), above.clone()], &blocks);

        assert_eq!(ordered[0].id, above.id, "the earlier paragraph reads first");
        let placed = place_footnotes(&ordered, 400.0);
        assert_eq!(placed[0].annotation_id, above.id);
        assert_eq!(placed[0].number, 1);
        assert_eq!(placed[1].annotation_id, below.id);
        assert_eq!(placed[1].number, 2);

        // Within one block, ascending anchor start wins over creation order.
        let later = footnote(blocks[0].id, 10);
        let earlier = footnote(blocks[0].id, 2);
        let ordered = reading_order(&[later.clone(), earlier.clone()], &blocks);
        assert_eq!(ordered[0].id, earlier.id);
        assert_eq!(ordered[1].id, later.id);
    }

    #[test]
    fn no_annotations_place_nothing() {
        let (blocks, layouts) = laid_out(PARA);
        assert!(place_marginalia(&[], &blocks, &layouts, &[], &config()).is_empty());
        assert!(place_footnotes(&[], 400.0).is_empty());
    }
}
