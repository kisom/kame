//! A document holding a kind, a style or another value that only a newer kame knows. It opens
//! read-only, showing what this build can, and nothing is written over it. Each file is written as
//! that newer build would have written it, by changing the head's JSON through SQL.

use std::path::{Path, PathBuf};

use kame_core::model::{
    Anchor, Annotation, AnnotationKind, Block, BlockKind, Document, ImageAlign, Metadata, Span,
    SpanStyle,
};
use kame_core::session::Session;
use kame_core::storage::{Db, NEWER_PART};
use uuid::Uuid;

/// The blocks of the document every file here starts from, by id.
struct Ids {
    bold: Uuid,
    image: Uuid,
    last: Uuid,
}

fn document() -> (Document, Ids) {
    let ids = Ids {
        bold: Uuid::new_v4(),
        image: Uuid::new_v4(),
        last: Uuid::new_v4(),
    };
    let mut doc = Document::new("t");
    doc.blocks = vec![
        Block {
            id: ids.bold,
            kind: BlockKind::Paragraph,
            text: "Some bold words.".to_string(),
            spans: vec![Span {
                start: 5,
                end: 9,
                style: SpanStyle::Bold,
            }],
        },
        Block {
            id: ids.image,
            kind: BlockKind::Image {
                path: "a.png".to_string(),
                link: None,
                align: ImageAlign::Center,
            },
            text: String::new(),
            spans: Vec::new(),
        },
        Block {
            id: ids.last,
            kind: BlockKind::Paragraph,
            text: "The last words.".to_string(),
            spans: Vec::new(),
        },
    ];
    doc.annotations = vec![Annotation {
        id: Uuid::new_v4(),
        kind: AnnotationKind::Note,
        anchor: Anchor::range(ids.bold, 0, 4),
        body: "a note".to_string(),
    }];
    (doc, ids)
}

fn head_of(path: &Path) -> String {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// The document, written as a newer build would have, with `edit` made to the head's JSON.
fn written_by_a_newer_build(name: &str, edit: impl Fn(&str) -> String) -> (PathBuf, Ids) {
    let path = kame_test_dir::folder(name).join("doc.kame");
    let (doc, ids) = document();
    {
        let mut db = Db::open(&path).unwrap();
        db.save_document(&doc).unwrap();
        db.save_metadata(&Metadata {
            title: "Kept".to_string(),
            ..Metadata::default()
        })
        .unwrap();
        db.create_snapshot(&doc, Some("before")).unwrap();
    }
    let head = head_of(&path);
    let newer = edit(&head);
    assert_ne!(newer, head, "the edit changed nothing");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("UPDATE documents SET json = ?1 WHERE id = 1", [&newer])
        .unwrap();
    (path, ids)
}

/// Everything in the file, as it is on the disk: head, metadata, meta, ops and snapshots.
fn everything_in(path: &Path) -> Vec<String> {
    let connection = rusqlite::Connection::open(path).unwrap();
    let mut all = Vec::new();
    for sql in [
        "SELECT json FROM documents",
        "SELECT json FROM metadata",
        "SELECT key || '=' || value FROM meta ORDER BY key",
        "SELECT json FROM ops ORDER BY seq",
        "SELECT id || ' ' || json FROM snapshots ORDER BY id",
    ] {
        let mut statement = connection.prepare(sql).unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap();
        all.push(format!("{sql}:"));
        all.extend(rows.map(Result::unwrap));
    }
    all
}

/// Every way an open document is written to, tried on one from a newer version. Each is refused,
/// and the file is as it was, through to closing it.
fn nothing_is_written(path: &Path) {
    let before = everything_in(path);
    let mut session = Session::open(path).unwrap();
    assert!(!session.insert("Typed here."));
    assert!(!session.split());
    assert!(!session.undo());
    assert!(!session.redo());
    assert!(!session.retry_or_dismiss());
    session.set_metadata(Metadata {
        title: "Changed".to_string(),
        ..Metadata::default()
    });
    session.apply_default_author("Someone");
    assert_eq!(session.snapshot(Some("lossy")), None);
    assert!(!session.restore(1));
    session.save_position(40.0);
    session.set_sync("revision", "place");
    session.set_sync_state("revision");
    session.set_sync_place("place");
    assert!(session.compact_history().is_err());
    let elsewhere = path.with_file_name("elsewhere.kame");
    assert!(session.save_as(&elsewhere, None, |_, _| false).is_err());
    assert!(!elsewhere.exists());
    assert!(session.save_as(path, None, |_, _| false).is_err());
    assert!(session.newer, "still read-only");
    drop(session);
    assert_eq!(everything_in(path), before);
}

fn says_it_is_from_a_newer_version(session: &Session) {
    assert!(session.newer);
    assert!(session.lossy);
    assert!(session.save_failed, "edits are refused");
    assert!(!session.would_lose_work(), "nothing unsaved is at stake");
    let said = session.last_error.clone().unwrap_or_default();
    assert!(
        said.contains("changes from a newer version of kame"),
        "{said:?}"
    );
    assert!(said.contains("read-only"), "{said:?}");
}

#[test]
fn a_block_of_a_kind_this_build_does_not_know_says_a_newer_version_wrote_it() {
    let (path, ids) = written_by_a_newer_build("newer-kind", |head| {
        head.replacen(
            r#""kind":"Paragraph","text":"The last words.""#,
            r#""kind":{"Table":{"rows":2}},"text":"The last words.""#,
            1,
        )
    });
    assert!(Db::open(&path).unwrap().holds_newer());
    let session = Session::open(&path).unwrap();
    says_it_is_from_a_newer_version(&session);
    let blocks = &session.document().blocks;
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[2].id, ids.last, "it keeps its id");
    assert_eq!(blocks[2].kind, BlockKind::Paragraph);
    assert_eq!(blocks[2].text, NEWER_PART);
    assert!(blocks[2].spans.is_empty());
    assert_eq!(blocks[0].text, "Some bold words.", "the rest is as it was");
    assert_eq!(blocks[0].spans.len(), 1);
    assert_eq!(session.document().annotations.len(), 1);
    assert_eq!(session.metadata.title, "Kept");
    drop(session);
    nothing_is_written(&path);
}

#[test]
fn a_style_this_build_does_not_know_is_left_out() {
    let (path, ids) = written_by_a_newer_build("newer-style", |head| {
        head.replacen(r#""style":"Bold""#, r#""style":"Strike""#, 1)
    });
    let session = Session::open(&path).unwrap();
    says_it_is_from_a_newer_version(&session);
    let block = &session.document().blocks[0];
    assert_eq!(block.id, ids.bold);
    assert_eq!(block.text, "Some bold words.", "the words are all there");
    assert!(block.spans.is_empty());
    drop(session);
    nothing_is_written(&path);
}

#[test]
fn a_note_of_a_kind_this_build_does_not_know_is_left_out() {
    let (path, _) = written_by_a_newer_build("newer-note", |head| {
        head.replacen(r#""kind":"Note""#, r#""kind":"Comment""#, 1)
    });
    let session = Session::open(&path).unwrap();
    says_it_is_from_a_newer_version(&session);
    assert!(session.document().annotations.is_empty());
    assert_eq!(session.document().blocks.len(), 3);
    drop(session);
    nothing_is_written(&path);
}

#[test]
fn a_value_this_build_does_not_know_falls_back_to_its_default() {
    let (path, ids) = written_by_a_newer_build("newer-value", |head| {
        head.replacen(r#""align":"Center""#, r#""align":"Diagonal""#, 1)
    });
    let session = Session::open(&path).unwrap();
    says_it_is_from_a_newer_version(&session);
    let image = &session.document().blocks[1];
    assert_eq!(image.id, ids.image);
    assert_eq!(
        image.kind,
        BlockKind::Image {
            path: "a.png".to_string(),
            link: None,
            align: ImageAlign::Left,
        }
    );
    drop(session);
    nothing_is_written(&path);
}

/// A newer kind can bring new shapes with it. The kind is what says it's newer, whatever else the
/// note or block holds and in whatever order its fields come.
#[test]
fn a_note_of_a_kind_this_build_does_not_know_is_left_out_whatever_its_anchor() {
    let (path, _) = written_by_a_newer_build("newer-note-anchor", |head| {
        head.replacen(r#""kind":"Note""#, r#""kind":"Bookmark""#, 1)
            .replacen(r#""start":0,"end":4}"#, r#""at":3}"#, 1)
    });
    let session = Session::open(&path).unwrap();
    says_it_is_from_a_newer_version(&session);
    assert!(session.document().annotations.is_empty());
    drop(session);
    nothing_is_written(&path);
}

#[test]
fn a_block_of_a_kind_this_build_does_not_know_is_shown_as_newer_whatever_its_spans() {
    let (path, ids) = written_by_a_newer_build("newer-kind-spans", |head| {
        head.replacen(
            r#""kind":"Paragraph","text":"The last words.","spans":[]"#,
            r#""kind":"Table","text":"The last words.","spans":[{"start":[0,1],"end":[0,2],"style":"Bold"}]"#,
            1,
        )
    });
    let session = Session::open(&path).unwrap();
    says_it_is_from_a_newer_version(&session);
    let block = &session.document().blocks[2];
    assert_eq!(block.id, ids.last);
    assert_eq!(block.text, NEWER_PART);
    drop(session);
    nothing_is_written(&path);
}

/// The value is looked for where it was, not wherever the same words are.
#[test]
fn a_value_falls_back_to_its_default_whatever_else_says_the_same() {
    let (path, ids) = written_by_a_newer_build("newer-value-twice", |head| {
        head.replacen(
            r#""align":"Center"}},"text":"""#,
            r#""align":"Diagonal"}},"text":"Diagonal""#,
            1,
        )
    });
    let session = Session::open(&path).unwrap();
    says_it_is_from_a_newer_version(&session);
    let image = &session.document().blocks[1];
    assert_eq!(image.id, ids.image);
    assert!(
        matches!(
            image.kind,
            BlockKind::Image {
                align: ImageAlign::Left,
                ..
            }
        ),
        "{:?}",
        image.kind
    );
    drop(session);
    nothing_is_written(&path);
}

#[test]
fn several_things_this_build_does_not_know_are_all_shown_as_it_can() {
    let (path, ids) = written_by_a_newer_build("newer-several", |head| {
        head.replacen(r#""style":"Bold""#, r#""style":"Strike""#, 1)
            .replacen(r#""align":"Center""#, r#""align":"Diagonal""#, 1)
            .replacen(r#""kind":"Note""#, r#""kind":"Comment""#, 1)
            .replacen(
                r#""kind":"Paragraph","text":"The last words.""#,
                r#""kind":"Table","text":"The last words.""#,
                1,
            )
    });
    let session = Session::open(&path).unwrap();
    says_it_is_from_a_newer_version(&session);
    let doc = session.document();
    assert!(doc.blocks[0].spans.is_empty());
    assert!(matches!(
        doc.blocks[1].kind,
        BlockKind::Image {
            align: ImageAlign::Left,
            ..
        }
    ));
    assert_eq!(doc.blocks[2].id, ids.last);
    assert_eq!(doc.blocks[2].text, NEWER_PART);
    assert!(doc.annotations.is_empty());
    drop(session);
    nothing_is_written(&path);
}

#[test]
fn missing_metadata_is_not_filled_in_for_a_document_from_a_newer_version() {
    let (path, _) = written_by_a_newer_build("newer-no-metadata", |head| {
        head.replacen(r#""kind":"Note""#, r#""kind":"Comment""#, 1)
    });
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("DELETE FROM metadata", [])
        .unwrap();
    let before = everything_in(&path);
    let session = Session::open(&path).unwrap();
    assert!(session.newer);
    drop(session);
    assert_eq!(everything_in(&path), before);
}

/// A change made to a head's JSON.
type Edit = fn(&str) -> String;

/// Damage isn't taken for something newer: a document that can't be read for any other reason is
/// refused as before, and left as it was.
#[test]
fn a_damaged_head_is_still_refused() {
    let damage: [(&str, Edit); 5] = [
        ("damaged-json", |head| head[..head.len() / 2].to_string()),
        ("damaged-type", |head| {
            head.replacen(r#""text":"The last words.""#, r#""text":5"#, 1)
        }),
        ("damaged-missing", |head| {
            head.replacen(r#""title":"t","#, "", 1)
        }),
        ("damaged-kind", |head| {
            head.replacen(r#""kind":"Paragraph""#, r#""kind":7"#, 1)
        }),
        // Something newer beside damage is still damage.
        ("damaged-and-newer", |head| {
            head.replacen(r#""style":"Bold""#, r#""style":"Strike""#, 1)
                .replacen(r#""text":"The last words.""#, r#""text":5"#, 1)
        }),
    ];
    for (name, edit) in damage {
        let (path, _) = written_by_a_newer_build(name, edit);
        let before = everything_in(&path);
        assert!(!Db::open(&path).unwrap().holds_newer(), "{name}");
        let Err(err) = Session::open(&path) else {
            panic!("{name} opened");
        };
        let said = err.to_string();
        assert!(
            said.contains("the document is corrupt or was written by a different version of kame"),
            "{name}: {said}"
        );
        assert_eq!(everything_in(&path), before, "{name}");
    }
}

/// A snapshot a newer build took can't be restored here, since what this build would put back
/// leaves out what it doesn't know. The document is otherwise this build's own to edit.
#[test]
fn a_snapshot_from_a_newer_version_is_not_restored() {
    let path = kame_test_dir::folder("newer-snapshot").join("doc.kame");
    let (doc, _) = document();
    {
        let mut db = Db::open(&path).unwrap();
        db.save_document(&doc).unwrap();
        db.create_snapshot(&doc, Some("ours")).unwrap();
    }
    let connection = rusqlite::Connection::open(&path).unwrap();
    let ours: String = connection
        .query_row("SELECT json FROM snapshots WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let kind = ours.replacen(r#""kind":"Note""#, r#""kind":"Comment""#, 1);
    let field = ours.replacen(r#""blocks":"#, r#""future":1,"blocks":"#, 1);
    for json in [&kind, &field] {
        assert_ne!(json, &ours);
        connection
            .execute(
                "INSERT INTO snapshots (timestamp, label, json) VALUES (2, 'newer', ?1)",
                [json],
            )
            .unwrap();
    }
    drop(connection);

    let mut session = Session::open(&path).unwrap();
    assert!(!session.newer, "a snapshot doesn't make the document newer");
    assert!(session.insert("Typed. "));
    let before = everything_in(&path);
    for id in [2, 3] {
        assert!(!session.restore(id), "snapshot {id}");
        let said = session.last_error.take().unwrap_or_default();
        assert!(said.contains("newer version of kame"), "{said:?}");
    }
    assert_eq!(everything_in(&path), before);
    assert!(session.restore(1), "one of its own is restored as ever");
}
