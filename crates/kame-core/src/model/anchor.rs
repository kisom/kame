use crate::model::document::{Anchor, Annotation, BlockId};

/// Shift every range-scoped anchor in `block_id` whose `start` is at or after
/// `edit_start` by `delta` bytes. Block-scoped anchors are left untouched:
/// they point at a whole block, so text edits within it do not move them.
///
/// `edit_start` and `delta` are **byte** offsets/lengths, matching `Anchor`
/// fields and `String`/`str` indexing. An insertion (`delta > 0`) at or before
/// an anchor's start moves the anchor right with the inserted text; a deletion
/// (`delta < 0`) moves it left. Anchors strictly before `edit_start` are left
/// alone, and anchors in other blocks are never touched.
///
/// Whole anchors falling inside a deleted range must be removed by the delete
/// operation itself; this function only rebases surviving anchors.
///
/// # Panics
///
/// Panics if the shift would overflow `usize`, which means the caller passed an
/// `edit_start`/`delta` pair inconsistent with the document text.
pub fn shift_anchors(
    annotations: &mut [Annotation],
    block_id: BlockId,
    edit_start: usize,
    delta: isize,
) {
    if delta == 0 {
        return;
    }

    for annotation in annotations.iter_mut() {
        let Anchor::Range {
            block_id: anchor_block_id,
            start,
            end,
        } = &mut annotation.anchor
        else {
            continue; // block-scoped anchors do not shift on text edits
        };
        if *anchor_block_id != block_id || *start < edit_start {
            continue;
        }
        *start = start.checked_add_signed(delta).expect(
            "anchor start overflowed usize: edit_start/delta inconsistent with document text",
        );
        *end = end.checked_add_signed(delta).expect(
            "anchor end overflowed usize: edit_start/delta inconsistent with document text",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::document::{Anchor, AnnotationKind};
    use uuid::Uuid;

    fn annotation(block_id: BlockId, start: usize, end: usize) -> Annotation {
        Annotation {
            id: Uuid::new_v4(),
            kind: AnnotationKind::Marginal,
            anchor: Anchor::range(block_id, start, end),
            body: "note".to_string(),
        }
    }

    #[test]
    fn block_scoped_anchors_do_not_shift() {
        let b = Uuid::new_v4();
        let mut anns = vec![Annotation {
            id: Uuid::new_v4(),
            kind: AnnotationKind::Marginal,
            anchor: Anchor::block(b),
            body: "marginal".to_string(),
        }];
        shift_anchors(&mut anns, b, 0, 4);
        assert_eq!(
            anns[0].anchor.byte_range(),
            None,
            "a block anchor ignores the edit"
        );
    }

    #[test]
    fn insert_before_anchor_shifts_it_by_byte_len() {
        let b = Uuid::new_v4();
        let mut anns = vec![annotation(b, 10, 12)];
        shift_anchors(&mut anns, b, 5, 4); // insert 4 bytes at offset 5
        assert_eq!(anns[0].anchor.byte_range().unwrap().0, 14);
        assert_eq!(anns[0].anchor.byte_range().unwrap().1, 16);
    }

    #[test]
    fn insert_after_anchor_does_not_shift() {
        let b = Uuid::new_v4();
        let mut anns = vec![annotation(b, 10, 12)];
        shift_anchors(&mut anns, b, 20, 4); // edit starts after the anchor
        assert_eq!(anns[0].anchor.byte_range().unwrap().0, 10);
        assert_eq!(anns[0].anchor.byte_range().unwrap().1, 12);
    }

    #[test]
    fn delete_before_anchor_shifts_it_back() {
        let b = Uuid::new_v4();
        let mut anns = vec![annotation(b, 10, 12)];
        shift_anchors(&mut anns, b, 2, -3); // delete 3 bytes at offset 2
        assert_eq!(anns[0].anchor.byte_range().unwrap().0, 7);
        assert_eq!(anns[0].anchor.byte_range().unwrap().1, 9);
    }

    #[test]
    fn other_block_anchors_untouched() {
        let b = Uuid::new_v4();
        let other = Uuid::new_v4();
        let mut anns = vec![annotation(other, 10, 12)];
        shift_anchors(&mut anns, b, 0, 4);
        assert_eq!(anns[0].anchor.byte_range().unwrap().0, 10);
        assert_eq!(anns[0].anchor.byte_range().unwrap().1, 12);
    }

    #[test]
    fn multibyte_insert_shifts_by_byte_len() {
        let b = Uuid::new_v4();
        let mut anns = vec![annotation(b, 10, 12)];
        let inserted = "éé";
        assert_eq!(inserted.len(), 4); // two chars, four bytes
        shift_anchors(&mut anns, b, 0, inserted.len() as isize);
        assert_eq!(anns[0].anchor.byte_range().unwrap().0, 14);
        assert_eq!(anns[0].anchor.byte_range().unwrap().1, 16);
    }
}
