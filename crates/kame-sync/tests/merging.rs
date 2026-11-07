//! The merge, driven by kame's own editing commands so that what it meets is what kame writes.

use kame_core::edit::{Cursor, Editor, Selection};
use kame_core::export::markdown::to_markdown;
use kame_core::export::to_html;
use kame_core::model::{Anchor, AnnotationKind, Block, BlockKind, Document, Span, SpanStyle};
use kame_core::session::Session;
use kame_sync::merge::{document_digest, merge, Merged, Side};
use uuid::Uuid;

fn block(id: u128, text: &str) -> Block {
    Block {
        id: Uuid::from_u128(id),
        kind: BlockKind::Paragraph,
        text: text.to_string(),
        spans: Vec::new(),
    }
}

fn document(blocks: Vec<Block>) -> Document {
    Document {
        id: Uuid::from_u128(99),
        title: "Test".to_string(),
        blocks,
        annotations: Vec::new(),
    }
}

/// A session editing a copy of `doc`, in memory.
fn editing(doc: &Document) -> Session {
    let mut session = Session::anonymous().unwrap();
    session.editor = Editor::new(doc.clone());
    session.caret = Cursor {
        block_id: doc.blocks[0].id,
        offset: 0,
    };
    session
}

fn at(session: &mut Session, block: usize, offset: usize) {
    let block_id = session.document().blocks[block].id;
    session.set_caret(Cursor { block_id, offset });
}

fn merged(base: &Document, one: &Document, other: &Document) -> Merged {
    let result = merge(
        Some(base),
        Side {
            doc: one,
            label: "talos",
            when: 2,
        },
        Side {
            doc: other,
            label: "titan",
            when: 1,
        },
    );
    check_valid(&result.doc);
    result
}

/// Everything kame relies on about a document.
fn check_valid(doc: &Document) {
    let mut ids: Vec<_> = doc.blocks.iter().map(|block| block.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), doc.blocks.len(), "block ids repeat");
    assert!(!doc.blocks.is_empty());
    for block in &doc.blocks {
        for span in &block.spans {
            assert!(
                span.start < span.end && span.end <= block.text.len(),
                "{span:?} in {:?}",
                block.text
            );
            assert!(
                block.text.is_char_boundary(span.start) && block.text.is_char_boundary(span.end)
            );
        }
    }
    for note in &doc.annotations {
        match &note.anchor {
            Anchor::Block(id) => {
                assert!(doc.blocks.iter().any(|block| block.id == *id), "{note:?}")
            }
            Anchor::Range {
                block_id,
                start,
                end,
            } => {
                let block = doc
                    .blocks
                    .iter()
                    .find(|block| block.id == *block_id)
                    .expect("anchored block");
                assert!(
                    start <= end && *end <= block.text.len(),
                    "{note:?} in {:?}",
                    block.text
                );
                assert!(block.text.is_char_boundary(*start) && block.text.is_char_boundary(*end));
            }
        }
    }
    assert_eq!(to_markdown(doc), to_markdown(doc));
    assert_eq!(to_html(doc), to_html(doc));
}

fn all_words(doc: &Document) -> String {
    let mut words = String::new();
    for block in &doc.blocks {
        words.push_str(&block.text);
        words.push('\n');
    }
    for note in &doc.annotations {
        words.push_str(&note.body);
        words.push('\n');
    }
    words
}

#[test]
fn different_paragraphs_merge_silently() {
    let base = document(vec![
        block(1, "First paragraph."),
        block(2, "Second paragraph."),
    ]);
    let mut one = editing(&base);
    at(&mut one, 0, "First".len());
    one.insert(" good");
    let mut other = editing(&base);
    at(&mut other, 1, "Second".len());
    other.insert(" fine");
    let result = merged(&base, one.document(), other.document());
    assert_eq!(result.doc.blocks[0].text, "First good paragraph.");
    assert_eq!(result.doc.blocks[1].text, "Second fine paragraph.");
    assert!(result.notes.is_empty());
}

#[test]
fn either_order_of_arguments_gives_the_same_document() {
    let base = document(vec![block(1, "One sentence here. Another one there.")]);
    let mut one = editing(&base);
    at(&mut one, 0, 3);
    one.insert(" more");
    let mut other = editing(&base);
    at(&mut other, 0, 7);
    other.insert(" less");
    let forwards = merge(
        Some(&base),
        Side {
            doc: one.document(),
            label: "a",
            when: 5,
        },
        Side {
            doc: other.document(),
            label: "b",
            when: 5,
        },
    );
    let backwards = merge(
        Some(&base),
        Side {
            doc: other.document(),
            label: "b",
            when: 5,
        },
        Side {
            doc: one.document(),
            label: "a",
            when: 5,
        },
    );
    assert_eq!(forwards.doc, backwards.doc);
    assert_eq!(
        document_digest(&forwards.doc),
        document_digest(&backwards.doc)
    );
    assert_eq!(forwards.notes.len(), 1);
}

#[test]
fn a_conflict_keeps_the_later_words_and_notes_the_earlier() {
    let base = document(vec![block(1, "The cat sat. The dog ran.")]);
    let mut later = editing(&base);
    at(&mut later, 0, "The cat".len());
    later.insert(" quietly");
    let mut earlier = editing(&base);
    at(&mut earlier, 0, "The cat sat".len());
    earlier.insert(" down");
    let result = merge(
        Some(&base),
        Side {
            doc: earlier.document(),
            label: "titan",
            when: 1,
        },
        Side {
            doc: later.document(),
            label: "talos",
            when: 2,
        },
    );
    assert_eq!(
        result.doc.blocks[0].text,
        "The cat quietly sat. The dog ran."
    );
    assert_eq!(result.notes.len(), 1);
    let note = &result.doc.annotations[0];
    assert_eq!(note.body, "On titan: The cat sat down.");
    assert_eq!(note.kind, AnnotationKind::Note);
    let Anchor::Range { start, end, .. } = note.anchor else {
        panic!("{:?}", note.anchor)
    };
    assert_eq!(
        &result.doc.blocks[0].text[start..end],
        "The cat quietly sat. "
    );
}

#[test]
fn a_block_cut_on_one_side_and_edited_on_the_other_stays() {
    let base = document(vec![block(1, "Keep."), block(2, "Cut or edit.")]);
    let mut cutter = editing(&base);
    let (keep, cut) = (
        cutter.document().blocks[0].id,
        cutter.document().blocks[1].id,
    );
    cutter.delete_range(
        Selection {
            start: Cursor {
                block_id: keep,
                offset: 5,
            },
            end: Cursor {
                block_id: cut,
                offset: "Cut or edit.".len(),
            },
        },
        Cursor {
            block_id: keep,
            offset: 5,
        },
    );
    let mut editor = editing(&base);
    at(&mut editor, 1, 3);
    editor.insert(" deeply");
    let result = merged(&base, cutter.document(), editor.document());
    assert!(all_words(&result.doc).contains("Cut deeply or edit."));
    assert_eq!(result.notes.len(), 1);
}

#[test]
fn a_block_cut_on_one_side_and_left_alone_on_the_other_goes() {
    let base = document(vec![block(1, "Keep."), block(2, "Going.")]);
    let mut cut = base.clone();
    cut.blocks.remove(1);
    let result = merged(&base, &cut, &base);
    assert_eq!(result.doc.blocks.len(), 1);
}

#[test]
fn new_blocks_on_both_sides_land_where_they_were_written() {
    let base = document(vec![block(1, "A."), block(2, "B.")]);
    let mut one = base.clone();
    one.blocks.insert(1, block(10, "After A, here."));
    let mut other = base.clone();
    other.blocks.insert(2, block(20, "After B, there."));
    let result = merged(&base, &one, &other);
    let texts: Vec<&str> = result
        .doc
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect();
    assert_eq!(texts, ["A.", "After A, here.", "B.", "After B, there."]);
}

fn list_item(id: u128, text: &str) -> Block {
    Block {
        kind: BlockKind::ListItem {
            ordered: false,
            depth: 0,
        },
        ..block(id, text)
    }
}

fn texts(doc: &Document) -> Vec<&str> {
    doc.blocks.iter().map(|block| block.text.as_str()).collect()
}

/// A list `a b c` and a paragraph after it, with `b` moved up on one side and the paragraph
/// edited on the other. The move's step is in the mover's history, and what it left is merged.
fn moved_and_edited() -> (Document, Session, Session) {
    let mut base = document(vec![
        list_item(1, "a"),
        list_item(2, "b"),
        list_item(3, "c"),
        block(4, "After."),
    ]);
    base.annotations.push(kame_core::model::Annotation {
        id: Uuid::from_u128(50),
        kind: AnnotationKind::Footnote,
        anchor: Anchor::range(Uuid::from_u128(2), 1, 1),
        body: "On b.".to_string(),
    });
    let mut mover = editing(&base);
    at(&mut mover, 1, 0);
    assert!(mover.move_list_item(true));
    assert_eq!(mover.history(), (1, 0));
    let mut editor = editing(&base);
    at(&mut editor, 3, "After".len());
    editor.insert(" all");
    (base, mover, editor)
}

#[test]
fn a_move_on_the_later_side_stands_beside_the_other_sides_edits() {
    let (base, mover, editor) = moved_and_edited();
    let result = merged(&base, mover.document(), editor.document());
    assert_eq!(texts(&result.doc), ["b", "a", "c", "After all."]);
    assert_eq!(
        result.doc.annotations, base.annotations,
        "the footnote is still on b"
    );
    assert!(result.notes.is_empty());
}

/// A known weak point (merging.md section 9): order follows the later side, so a move goes
/// back when the other machine wrote later, even if it only edited words.
#[test]
fn a_move_on_the_earlier_side_goes_back_where_it_was() {
    let (base, mover, editor) = moved_and_edited();
    let result = merged(&base, editor.document(), mover.document());
    assert_eq!(texts(&result.doc), ["a", "b", "c", "After all."]);
    assert_eq!(result.doc.annotations, base.annotations);
    assert!(
        result.notes.is_empty(),
        "no words are lost, so there's no note"
    );
}

#[test]
fn splitting_a_paragraph_while_the_other_side_edits_it_loses_nothing() {
    let base = document(vec![block(1, "Before the break. After the break.")]);
    let mut splitter = editing(&base);
    at(&mut splitter, 0, "Before the break. ".len());
    splitter.split();
    let mut editor = editing(&base);
    at(&mut editor, 0, "Before the break. After".len());
    editor.insert(" all");
    let result = merged(&base, splitter.document(), editor.document());
    assert!(
        all_words(&result.doc).contains("After all the break."),
        "{:#?}",
        result.doc.blocks
    );
}

#[test]
fn notes_follow_their_words_through_the_other_sides_edits() {
    let mut base = document(vec![block(1, "One sentence. Marked words here.")]);
    let mut noted = editing(&base);
    let block_id = base.blocks[0].id;
    let start = "One sentence. ".len();
    noted.selection = Some(Selection {
        start: Cursor {
            block_id,
            offset: start,
        },
        end: Cursor {
            block_id,
            offset: start + "Marked".len(),
        },
    });
    let note = noted.add_annotation(AnnotationKind::Note).unwrap();
    noted.set_annotation_body(note, "a thought");
    base.title = "Test".to_string();
    let mut other = editing(&base);
    at(&mut other, 0, 0);
    other.insert("New opening. ");
    let result = merged(&base, noted.document(), other.document());
    let found = result
        .doc
        .annotations
        .iter()
        .find(|found| found.id == note)
        .unwrap();
    assert_eq!(found.body, "a thought");
    let Anchor::Range { start, end, .. } = found.anchor else {
        panic!()
    };
    assert_eq!(&result.doc.blocks[0].text[start..end], "Marked");
}

/// Fuzzing found this (`fuzz_logs.rs`, damaged merges, seed 0). A note can name a block its own
/// side no longer has, as an older file's can, and when the other side kept that block by editing
/// it, the note's offsets were into text that isn't there, past its end.
#[test]
fn a_note_on_a_block_only_the_other_side_kept_takes_that_sides_place() {
    let note = |start: usize, end: usize| kame_core::model::Annotation {
        id: Uuid::from_u128(50),
        kind: AnnotationKind::Footnote,
        anchor: Anchor::range(Uuid::from_u128(2), start, end),
        body: "A footnote.".to_string(),
    };
    let mut base = document(vec![
        block(1, "Kept."),
        block(2, "A long sentence that goes on."),
    ]);
    base.annotations = vec![note(20, 29)];
    // The later side cut the block and kept its note, as a file from an older kame can.
    let mut cut = base.clone();
    cut.blocks.remove(1);
    // The earlier side shortened it, so the note moved with its words.
    let mut shortened = base.clone();
    shortened.blocks[1].text = "Short now.".to_string();
    shortened.annotations = vec![note(6, 9)];

    let result = merged(&base, &cut, &shortened);

    assert_eq!(result.doc.blocks[1].text, "Short now.");
    let found = &result
        .doc
        .annotations
        .iter()
        .find(|found| found.id == Uuid::from_u128(50));
    assert_eq!(
        found.unwrap().anchor,
        Anchor::range(Uuid::from_u128(2), 6, 9)
    );
}

/// A note only the side without its block has, where the other side kept the block, goes on that
/// block, not on the document's first.
#[test]
fn a_note_whose_block_only_the_other_side_kept_goes_on_that_block() {
    let note = |body: &str| kame_core::model::Annotation {
        id: Uuid::from_u128(50),
        kind: AnnotationKind::Note,
        anchor: Anchor::range(Uuid::from_u128(2), 20, 29),
        body: body.to_string(),
    };
    let mut base = document(vec![
        block(1, "Kept."),
        block(2, "A long sentence that goes on."),
    ]);
    base.annotations = vec![note("A thought.")];
    // The later side cut the block, kept its note and wrote more in it.
    let mut cut = base.clone();
    cut.blocks.remove(1);
    cut.annotations = vec![note("A thought, and more.")];
    // The earlier side edited the block and took the note out.
    let mut edited = base.clone();
    edited.blocks[1].text = "Short now.".to_string();
    edited.annotations.clear();

    let result = merged(&base, &cut, &edited);

    let found = result
        .doc
        .annotations
        .iter()
        .find(|found| found.id == Uuid::from_u128(50))
        .expect("the note is kept");
    assert_eq!(found.body, "A thought, and more.");
    assert_eq!(found.anchor, Anchor::block(Uuid::from_u128(2)));
}

/// Fuzzing found this (`fuzz_logs.rs`, damaged merges, seed 514). Two notes sharing an id, as a
/// damaged file can hold, were matched by id, so a merge kept one of them and lost the other.
#[test]
fn notes_sharing_an_id_both_come_through_a_merge() {
    let mut one = document(vec![block(1, "Both share this one."), block(2, "Mine.")]);
    let note = |kind, anchor, body: &str| kame_core::model::Annotation {
        id: Uuid::from_u128(50),
        kind,
        anchor,
        body: body.to_string(),
    };
    one.annotations = vec![
        note(
            AnnotationKind::Footnote,
            Anchor::range(Uuid::from_u128(1), 5, 10),
            "A footnote that travels.",
        ),
        note(
            AnnotationKind::Marginal,
            Anchor::block(Uuid::from_u128(1)),
            "A marginal that travels.",
        ),
    ];
    let mut other = one.clone();
    other.blocks[1].text = "Yours.".to_string();

    let result = merge(
        None,
        Side {
            doc: &one,
            label: "a",
            when: 2,
        },
        Side {
            doc: &other,
            label: "b",
            when: 1,
        },
    );

    check_valid(&result.doc);
    let words = all_words(&result.doc);
    assert!(words.contains("A footnote that travels."), "{words}");
    assert!(words.contains("A marginal that travels."), "{words}");
}

#[test]
fn formatting_follows_and_removal_sticks() {
    let mut base = document(vec![block(1, "One bold word. Second one plain.")]);
    let bold = |start: usize, end: usize| Span {
        start,
        end,
        style: SpanStyle::Bold,
    };
    base.blocks[0].spans = vec![bold(4, 8), bold(15, 21)];
    let mut one = editing(&base);
    at(&mut one, 0, 0);
    one.insert("Lead. ");
    let mut other = base.clone();
    other.blocks[0].spans.remove(1);
    other.blocks[0].text.push_str(" End.");
    let result = merged(&base, one.document(), &other);
    let text = &result.doc.blocks[0].text;
    assert_eq!(text, "Lead. One bold word. Second one plain. End.");
    let spans = &result.doc.blocks[0].spans;
    assert_eq!(spans.len(), 1, "{spans:?}");
    assert_eq!(&text[spans[0].start..spans[0].end], "bold");
}

#[test]
fn no_base_keeps_what_both_agree_on_and_flags_the_rest() {
    let one = document(vec![block(1, "Same."), block(2, "Mine.")]);
    let other = document(vec![block(1, "Same."), block(2, "Yours.")]);
    let result = merge(
        None,
        Side {
            doc: &one,
            label: "a",
            when: 2,
        },
        Side {
            doc: &other,
            label: "b",
            when: 1,
        },
    );
    check_valid(&result.doc);
    assert_eq!(result.doc.blocks[0].text, "Same.");
    assert_eq!(result.doc.blocks[1].text, "Mine.");
    assert!(all_words(&result.doc).contains("Yours."));
}

#[test]
fn fast_forwards_change_nothing_else() {
    let base = document(vec![block(1, "A.")]);
    let mut one = base.clone();
    one.blocks[0].text.push_str(" B.");
    assert_eq!(merged(&base, &one, &base).doc, one);
    assert_eq!(merged(&base, &base, &one).doc, one);
    assert_eq!(merged(&base, &one, &one).doc, one);
}

/// Small enough to reproduce a failure from its seed.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit.max(1) as u64) as usize
    }
}

fn boundary(text: &str, wanted: usize) -> usize {
    let mut at = wanted.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Applies random edits through kame's commands, returning the unique words it typed.
fn random_edits(
    session: &mut Session,
    random: &mut Random,
    tag: &str,
    edits: usize,
) -> Vec<String> {
    let mut typed = Vec::new();
    for edit in 0..edits {
        let count = session.document().blocks.len();
        let which = random.below(count);
        let len = session.document().blocks[which].text.len();
        let offset = boundary(
            &session.document().blocks[which].text,
            random.below(len + 1),
        );
        at(session, which, offset);
        match random.below(10) {
            0..=3 => {
                let word = format!(" zq{tag}{edit}q ");
                session.insert(&word);
                typed.push(word.trim().to_string());
            }
            4 => {
                let word = format!("Zq{tag}{edit}q sentence. ");
                session.insert(&word);
                typed.push(format!("Zq{tag}{edit}q"));
            }
            5 => {
                session.split();
            }
            6 => {
                let text = &session.document().blocks[which].text;
                let end = boundary(text, offset + 1 + random.below(30));
                if end > offset {
                    let block_id = session.document().blocks[which].id;
                    session.delete_range(
                        Selection {
                            start: Cursor { block_id, offset },
                            end: Cursor {
                                block_id,
                                offset: end,
                            },
                        },
                        Cursor { block_id, offset },
                    );
                }
            }
            7 => {
                if count > 1 {
                    at(session, which, 0);
                    session.backspace();
                }
            }
            8 => {
                let block_id = session.document().blocks[which].id;
                session.set_block_kind(
                    block_id,
                    BlockKind::Heading {
                        level: 2,
                        centered: false,
                    },
                );
            }
            _ => {
                let text = &session.document().blocks[which].text;
                let end = boundary(text, offset + 1 + random.below(10));
                if end > offset {
                    let block_id = session.document().blocks[which].id;
                    session.selection = Some(Selection {
                        start: Cursor { block_id, offset },
                        end: Cursor {
                            block_id,
                            offset: end,
                        },
                    });
                    if let Some(note) = session.add_annotation(AnnotationKind::Note) {
                        let word = format!("zn{tag}{edit}q");
                        session.set_annotation_body(note, &word);
                        typed.push(word);
                    }
                }
            }
        }
    }
    let words = all_words(session.document());
    typed.retain(|word| words.contains(word.as_str()));
    typed
}

fn sample() -> Document {
    let mut doc = document(vec![
        block(
            1,
            "Typography is a craft. Its heartwood is calligraphy. The dance of pen, eye and hand.",
        ),
        block(2, "Un été à Paris. Déjà vu, “naïveté” — and so on. Ça va?"),
        block(3, "Short."),
        block(4, ""),
        block(5, "let a = 1;\nlet b = 2;\nlet c = 3;\n"),
        block(
            6,
            "A closing thought, with a clause; another clause. And one more sentence here.",
        ),
    ]);
    doc.blocks[4].kind = BlockKind::Code { language: None };
    doc.blocks[0].spans = vec![Span {
        start: 0,
        end: 10,
        style: SpanStyle::Italic,
    }];
    doc
}

/// Hundreds of random divergences: whatever was typed on either side must come through, in the
/// text or in a note, and the result must not depend on who merges.
#[test]
fn random_divergence_loses_no_words_and_agrees_with_itself() {
    let base = sample();
    let mut total_notes = 0;
    for seed in 1..=400u64 {
        let mut random = Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut one = editing(&base);
        let mut other = editing(&base);
        let edits = 1 + random.below(12);
        let one_typed = random_edits(&mut one, &mut random, "a", edits);
        let edits = 1 + random.below(12);
        let other_typed = random_edits(&mut other, &mut random, "b", edits);
        let (one_doc, other_doc) = (one.document().clone(), other.document().clone());
        let when = (random.below(3) as i64, random.below(3) as i64);
        let forwards = merge(
            Some(&base),
            Side {
                doc: &one_doc,
                label: "a",
                when: when.0,
            },
            Side {
                doc: &other_doc,
                label: "b",
                when: when.1,
            },
        );
        let backwards = merge(
            Some(&base),
            Side {
                doc: &other_doc,
                label: "b",
                when: when.1,
            },
            Side {
                doc: &one_doc,
                label: "a",
                when: when.0,
            },
        );
        assert_eq!(forwards.doc, backwards.doc, "seed {seed}");
        check_valid(&forwards.doc);
        let words = all_words(&forwards.doc);
        for word in one_typed.iter().chain(&other_typed) {
            assert!(
                words.contains(word.as_str()),
                "seed {seed} lost {word}: {words}"
            );
        }
        total_notes += forwards.notes.len();
        // The result is still a document kame can edit.
        let mut after = editing(&forwards.doc);
        after.insert("still editable");
        assert!(!after.save_failed);
    }
    assert!(
        total_notes > 100,
        "the test should exercise conflicts, saw {total_notes}"
    );
}
