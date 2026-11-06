//! A file whose spans and anchors don't fit their text, as a damaged or hand-made one can be,
//! opened and edited as the app and tools do. kame's own edits never write one, so each file is
//! written through `Db` directly, which stores whatever it's given.

use std::path::{Path, PathBuf};

use kame_core::edit::{Cursor, Op};
use kame_core::model::{
    Anchor, Annotation, AnnotationKind, Block, BlockKind, Document, Span, SpanStyle,
};
use kame_core::references::References;
use kame_core::session::Session;
use kame_core::storage::{Db, Position};
use uuid::Uuid;

/// "é" is bytes 3..5. The text is 13 bytes long.
const CAFE: &str = "Café au lait";
/// "🙂" is bytes 3..7. The text is 13 bytes long.
const SMILE: &str = "hi 🙂 there";

fn file(name: &str) -> PathBuf {
    kame_test_dir::folder(name).join("damaged.kame")
}

fn span(start: usize, end: usize, style: SpanStyle) -> Span {
    Span { start, end, style }
}

fn annotation(kind: AnnotationKind, block: &Block, start: usize, end: usize) -> Annotation {
    Annotation {
        id: Uuid::new_v4(),
        kind,
        anchor: Anchor::range(block.id, start, end),
        body: "a note".to_string(),
    }
}

fn paragraph(text: &str, spans: Vec<Span>) -> Block {
    Block {
        id: Uuid::new_v4(),
        kind: BlockKind::Paragraph,
        text: text.to_string(),
        spans,
    }
}

/// A document with every kind of offset that doesn't fit, one to a block so that each can be
/// told apart.
fn damaged() -> Document {
    let mut doc = Document::new("Damaged");
    doc.blocks = vec![
        paragraph(CAFE, vec![span(5, 40, SpanStyle::Bold)]),
        paragraph(CAFE, vec![span(8, 3, SpanStyle::Italic)]),
        paragraph(CAFE, vec![span(0, 4, SpanStyle::Code)]),
        paragraph(
            SMILE,
            vec![span(
                5,
                9,
                SpanStyle::Link {
                    url: "https://example.com".to_string(),
                },
            )],
        ),
        paragraph(CAFE, Vec::new()),
        paragraph(SMILE, Vec::new()),
    ];
    let (notes, footnotes) = (&doc.blocks[4], &doc.blocks[5]);
    doc.annotations = vec![
        annotation(AnnotationKind::Note, notes, 5, 40),
        annotation(AnnotationKind::Note, notes, 4, 8),
        annotation(AnnotationKind::Note, notes, 8, 3),
        annotation(AnnotationKind::Footnote, notes, 40, 40),
        annotation(AnnotationKind::Footnote, footnotes, 5, 5),
    ];
    doc
}

/// What `damaged` fits to.
fn spans_fitted() -> Vec<Vec<(usize, usize)>> {
    vec![
        vec![(5, 13)],
        vec![],
        vec![(0, 3)],
        vec![(3, 9)],
        vec![],
        vec![],
    ]
}

fn anchors_fitted() -> Vec<(usize, usize)> {
    vec![(5, 13), (3, 8), (8, 8), (13, 13), (3, 3)]
}

fn spans_of(doc: &Document) -> Vec<Vec<(usize, usize)>> {
    doc.blocks
        .iter()
        .map(|block| block.spans.iter().map(|s| (s.start, s.end)).collect())
        .collect()
}

fn anchors_of(doc: &Document) -> Vec<(usize, usize)> {
    doc.annotations
        .iter()
        .map(|annotation| annotation.anchor.byte_range().unwrap())
        .collect()
}

fn write(path: &Path, doc: &Document) {
    Db::open(path).unwrap().save_document(doc).unwrap();
}

/// The head as it's stored, without reading it as a document.
fn stored_head(path: &Path) -> String {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn stored_document(path: &Path) -> Document {
    serde_json::from_str(&stored_head(path)).unwrap()
}

#[test]
fn a_damaged_head_opens_with_its_offsets_fitted_and_is_left_as_it_was() {
    let path = file("open");
    write(&path, &damaged());
    let before = stored_head(&path);

    let session = Session::open(&path).unwrap();

    assert_eq!(spans_of(session.document()), spans_fitted());
    assert_eq!(anchors_of(session.document()), anchors_fitted());
    assert_eq!(session.last_error, None);
    drop(session);
    assert_eq!(stored_head(&path), before, "nothing is written on opening");
}

#[test]
fn a_damaged_head_exports_what_fits() {
    let path = file("export");
    let mut doc = Document::new("Damaged");
    doc.blocks = vec![paragraph(CAFE, vec![span(6, 40, SpanStyle::Bold)])];
    doc.annotations = vec![annotation(AnnotationKind::Footnote, &doc.blocks[0], 40, 40)];
    write(&path, &doc);
    let mut session = Session::open(&path).unwrap();
    let folder = path.parent().unwrap();

    let markdown = session
        .export_markdown_to(&folder.join("damaged.md"), &References::default())
        .unwrap();
    let markdown = std::fs::read_to_string(markdown).unwrap();
    assert!(markdown.contains("Café **au lait**[^1]"), "{markdown}");

    let html = session
        .export_html_to(&folder.join("damaged.html"), &References::default())
        .unwrap();
    let html = std::fs::read_to_string(html).unwrap();
    assert!(html.contains("Café <strong>au lait</strong><sup"), "{html}");
}

#[test]
fn every_kind_of_damage_exports_without_panicking() {
    let path = file("export-all");
    write(&path, &damaged());
    let mut session = Session::open(&path).unwrap();
    let folder = path.parent().unwrap();
    let references = References::default();
    assert!(session
        .export_markdown_to(&folder.join("all.md"), &references)
        .is_some());
    assert!(session
        .export_html_to(&folder.join("all.html"), &references)
        .is_some());
}

#[test]
fn typing_and_deleting_around_damage_writes_the_fitted_offsets() {
    let path = file("edit");
    write(&path, &damaged());
    let mut session = Session::open(&path).unwrap();
    let first = session.document().blocks[0].id;
    let notes = session.document().blocks[4].id;

    // Before and after the bold that ran past the end, which now ends at the end.
    session.set_caret(Cursor {
        block_id: first,
        offset: 0,
    });
    assert!(session.insert("x"));
    assert_eq!(spans_of(&stored_document(&path))[0], [(6, 14)]);
    session.set_caret(Cursor {
        block_id: first,
        offset: 14,
    });
    assert!(session.insert("y"));
    assert_eq!(session.document().blocks[0].text, "xCafé au laity");
    assert_eq!(spans_of(&stored_document(&path))[0], [(6, 14)]);

    // Across the notes: those inside go with the words, and the one that ran past the end is cut
    // short.
    session.set_caret(Cursor {
        block_id: notes,
        offset: 1,
    });
    session.select_to(Cursor {
        block_id: notes,
        offset: 12,
    });
    assert!(session.delete_selected());
    assert_eq!(session.document().blocks[4].text, "Ct");
    let stored = stored_document(&path);
    assert_eq!(stored.blocks[4].text, "Ct");
    assert_eq!(anchors_of(&stored), [(1, 2), (2, 2), (3, 3)]);
    assert_eq!(anchors_of(&stored), anchors_of(session.document()));

    for _ in 0..3 {
        assert!(session.undo(), "{:?}", session.last_error);
    }
    assert_eq!(spans_of(session.document()), spans_fitted());
    let mut anchors = anchors_of(session.document());
    anchors.sort_unstable();
    let mut fitted = anchors_fitted();
    fitted.sort_unstable();
    assert_eq!(
        anchors, fitted,
        "undo brings back the notes as they were read"
    );
    for _ in 0..3 {
        assert!(session.redo(), "{:?}", session.last_error);
    }
    assert_eq!(session.document().blocks[4].text, "Ct");
    assert_eq!(&stored_document(&path), session.document());
}

#[test]
fn a_caret_left_past_the_end_or_inside_a_character_lands_on_the_text() {
    for (offset, landed) in [(999, CAFE.len()), (4, 3)] {
        let path = file("caret");
        let doc = damaged();
        let block_id = doc.blocks[4].id;
        write(&path, &doc);
        Db::open(&path)
            .unwrap()
            .save_position(&Position {
                block_id,
                offset,
                scroll: 0.0,
            })
            .unwrap();
        let mut session = Session::open(&path).unwrap();
        assert_eq!(
            session.valid_caret(),
            Cursor {
                block_id,
                offset: landed
            }
        );
        assert!(session.insert("!"));
    }
}

#[test]
fn a_damaged_snapshot_is_restored_fitted() {
    let path = file("snapshot");
    write(&path, &Document::new("Damaged"));
    let id = Db::open(&path)
        .unwrap()
        .create_snapshot(&damaged(), None)
        .unwrap();
    let mut session = Session::open(&path).unwrap();

    assert!(session.restore(id));

    assert_eq!(spans_of(session.document()), spans_fitted());
    assert_eq!(anchors_of(session.document()), anchors_fitted());
    assert_eq!(&stored_document(&path), session.document());
}

#[test]
fn a_damaged_document_in_the_history_is_fitted_when_undone_or_redone_to() {
    let path = file("replaced");
    let old = damaged();
    let mut new = damaged();
    new.blocks[1].text.push_str(" again");
    let op = Op::ReplaceDocument {
        old: Box::new(old),
        new: Box::new(new.clone()),
    };
    Db::open(&path).unwrap().record_edit(&new, &op).unwrap();
    let mut session = Session::open(&path).unwrap();

    assert!(session.undo());
    assert_eq!(spans_of(session.document()), spans_fitted());
    assert_eq!(anchors_of(session.document()), anchors_fitted());
    assert!(session.redo());
    assert_eq!(session.document().blocks[1].text, "Café au lait again");
    assert_eq!(spans_of(session.document()), spans_fitted());
}

/// Writes a file whose head is `now` and whose log holds `step`, recorded against `was` by the
/// program that damaged it. Undoing the step brings back what it took, fitted to the text it took
/// it from, and after reading the file again redo still finds what it expects.
fn undo_reopen_and_redo(name: &str, was: &Document, step: Op, now: &Document) {
    let path = file(name);
    Db::open(&path).unwrap().record_edit(now, &step).unwrap();

    let mut session = Session::open(&path).unwrap();
    assert!(session.undo(), "{:?}", session.last_error);
    let mut fitted = was.clone();
    fitted.fit_offsets();
    assert_eq!(session.document().blocks, fitted.blocks);
    assert_eq!(anchors_of(session.document()), [(3, 8)]);
    drop(session);

    let mut session = Session::open(&path).unwrap();
    assert_eq!(anchors_of(session.document()), [(3, 8)]);
    assert!(session.redo(), "{:?}", session.last_error);
    assert_eq!(session.document(), now);
    assert!(session.insert("!"), "editing carries on");
}

/// The note a delete took started inside "é".
#[test]
fn a_delete_of_damaged_text_undoes_and_redoes_after_reading_it_again() {
    let mut was = Document::new("Damaged");
    was.blocks = vec![paragraph(CAFE, Vec::new())];
    let note = annotation(AnnotationKind::Note, &was.blocks[0], 4, 8);
    was.annotations = vec![note.clone()];
    let block_id = was.blocks[0].id;
    let delete = Op::delete_text(&was, block_id, 1, CAFE.len());
    let Op::DeleteText { removed, .. } = &delete else {
        unreachable!()
    };
    assert_eq!(removed, std::slice::from_ref(&note));
    let mut now = was.clone();
    now.blocks[0].text = "C".to_string();
    now.annotations.clear();
    undo_reopen_and_redo("deleted-text", &was, delete, &now);
}

/// The note on a deleted block started inside "é".
#[test]
fn a_deleted_damaged_block_undoes_and_redoes_after_reading_it_again() {
    let mut was = Document::new("Damaged");
    was.blocks = vec![
        paragraph("Before.", Vec::new()),
        paragraph(CAFE, Vec::new()),
    ];
    was.annotations = vec![annotation(AnnotationKind::Note, &was.blocks[1], 4, 8)];
    let delete = Op::delete_block(&was, 1);
    let mut now = was.clone();
    now.blocks.truncate(1);
    now.annotations.clear();
    undo_reopen_and_redo("deleted-block", &was, delete, &now);
}
