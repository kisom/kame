//! A document holding a kind, a style or another value that only a newer kame knows. It opens
//! read-only, showing what this build can, and nothing is written over it. Each file is written as
//! that newer build would have written it, by changing the head's JSON through SQL.

use std::path::{Path, PathBuf};

use kame_core::edit::{Cursor, Op};
use kame_core::model::{
    Anchor, Annotation, AnnotationKind, Block, BlockKind, Document, ImageAlign, Metadata, Span,
    SpanStyle,
};
use kame_core::references::References;
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

/// What leaves the session, an export or a copy, leaves out the paragraphs standing for what a
/// newer version wrote, rather than saying that as if it were the document's.
#[test]
fn an_export_or_a_copy_leaves_out_what_a_newer_version_wrote() {
    let (path, _) = written_by_a_newer_build("newer-export", |head| {
        head.replacen(
            r#""kind":"Paragraph","text":"The last words.""#,
            r#""kind":"Table","text":"The last words.""#,
            1,
        )
    });
    let mut session = Session::open(&path).unwrap();
    assert!(session.lossy);
    let folder = path.parent().unwrap();
    let (markdown, html) = (folder.join("out.md"), folder.join("out.html"));
    assert!(session
        .export_markdown_to(&markdown, &References::new())
        .is_some());
    assert!(session.export_html_to(&html, &References::new()).is_some());
    for written in [&markdown, &html] {
        let written = std::fs::read_to_string(written).unwrap();
        assert!(written.contains("Some"), "{written}");
        assert!(!written.contains(NEWER_PART), "{written}");
    }
    let (start, end) = (
        session.document_edge(false).unwrap(),
        session.document_edge(true).unwrap(),
    );
    session.set_caret(start);
    session.select_to(end);
    let clipped = session.clipped().unwrap();
    assert!(clipped.text.contains("Some bold words."));
    assert!(!clipped.text.contains(NEWER_PART), "{:?}", clipped.text);
    assert!(clipped.left_out);
    drop(session);
    nothing_is_written(&path);
}

/// A selection lying wholly in what stands for a newer version's block takes nothing, so a copy of
/// it leaves the clipboard as it was.
#[test]
fn a_copy_of_only_what_a_newer_version_wrote_is_nothing() {
    let (path, ids) = written_by_a_newer_build("newer-copy-placeholder", |head| {
        head.replacen(
            r#""kind":"Paragraph","text":"The last words.""#,
            r#""kind":"Table","text":"The last words.""#,
            1,
        )
    });
    let mut session = Session::open(&path).unwrap();
    session.set_caret(Cursor {
        block_id: ids.last,
        offset: 2,
    });
    session.select_to(Cursor {
        block_id: ids.last,
        offset: 10,
    });
    assert!(session.clipped().is_none());
}

/// Notes on a block a newer version wrote go with it, out of every export.
#[test]
fn an_export_leaves_out_the_notes_on_what_a_newer_version_wrote() {
    let (path, ids) = written_by_a_newer_build("newer-export-notes", |head| {
        head.replacen(
            r#""kind":"Paragraph","text":"The last words.""#,
            r#""kind":"Table","text":"The last words.""#,
            1,
        )
    });
    let connection = rusqlite::Connection::open(&path).unwrap();
    let mut head: serde_json::Value = serde_json::from_str(&head_of(&path)).unwrap();
    let last = ids.last.to_string();
    head["annotations"].as_array_mut().unwrap().extend([
        serde_json::json!({"id": Uuid::new_v4(), "kind": "Footnote",
            "anchor": {"block_id": last, "start": 0, "end": 3}, "body": "A footnote on the table."}),
        serde_json::json!({"id": Uuid::new_v4(), "kind": "Marginal",
            "anchor": last, "body": "A marginal on the table."}),
    ]);
    connection
        .execute(
            "UPDATE documents SET json = ?1 WHERE id = 1",
            [head.to_string()],
        )
        .unwrap();
    drop(connection);
    let mut session = Session::open(&path).unwrap();
    assert!(session.lossy);
    let folder = path.parent().unwrap();
    let (markdown, html) = (folder.join("out.md"), folder.join("out.html"));
    session
        .export_markdown_to(&markdown, &References::new())
        .unwrap();
    session.export_html_to(&html, &References::new()).unwrap();
    for written in [&markdown, &html] {
        let written = std::fs::read_to_string(written).unwrap();
        assert!(!written.contains("on the table"), "{written}");
    }
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

/// Whatever reads a document without ever writing it, such as a site build or a notebook's search,
/// takes what this build can read of one from a newer version. What it doesn't know is left out,
/// with nothing standing in its place, and so are the notes on a block it doesn't know.
#[test]
fn a_reader_takes_what_it_can_and_nothing_in_place_of_the_rest() {
    let (path, ids) = written_by_a_newer_build("newer-reading", |head| {
        head.replacen(
            r#""kind":"Paragraph","text":"The last words.""#,
            r#""kind":"Table","text":"The last words.""#,
            1,
        )
        .replacen(r#""style":"Bold""#, r#""style":"Strike""#, 1)
    });
    let connection = rusqlite::Connection::open(&path).unwrap();
    let mut head: serde_json::Value = serde_json::from_str(&head_of(&path)).unwrap();
    head["annotations"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
        "id": Uuid::new_v4(), "kind": "Footnote",
        "anchor": {"block_id": ids.last.to_string(), "start": 0, "end": 3},
        "body": "A footnote on the table."}));
    connection
        .execute(
            "UPDATE documents SET json = ?1 WHERE id = 1",
            [head.to_string()],
        )
        .unwrap();
    drop(connection);
    let before = everything_in(&path);

    let db = Db::open_read_only(&path).unwrap();
    assert!(db.leaves_out_newer());
    let read = db.load_readable().unwrap();
    let blocks: Vec<Uuid> = read.blocks.iter().map(|block| block.id).collect();
    assert_eq!(blocks, [ids.bold, ids.image], "the table is left out");
    assert_eq!(read.blocks[0].text, "Some bold words.");
    assert!(read.blocks[0].spans.is_empty(), "so is the style");
    assert_eq!(read.annotations.len(), 1, "and the note on the table");
    assert_eq!(read.annotations[0].body, "a note");
    assert!(!format!("{read:?}").contains(NEWER_PART));
    drop(db);
    assert_eq!(everything_in(&path), before);
}

/// A field only a newer version knows is left out by reading, and a reader can tell, whether it's
/// in the document or in its properties.
#[test]
fn a_reader_can_tell_it_leaves_out_a_newer_field() {
    let (path, _) = written_by_a_newer_build("newer-reading-field", |head| {
        head.replacen(
            r#""text":"The last words.""#,
            r#""text":"The last words.","colour":"red""#,
            1,
        )
    });
    let db = Db::open_read_only(&path).unwrap();
    assert!(db.leaves_out_newer());
    let read = db.load_readable().unwrap();
    assert_eq!(read, db.load_document().unwrap());
    assert_eq!(read.blocks.len(), 3);

    let (path, _) = written_by_a_newer_build("newer-reading-metadata", |head| {
        head.replacen(r#""title":"t""#, r#""title":"u""#, 1)
    });
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE metadata SET json = replace(json, '\"title\":\"Kept\"', '\"title\":\"Kept\",\"mood\":\"calm\"')",
            [],
        )
        .unwrap();
    assert!(Db::open_read_only(&path).unwrap().leaves_out_newer());
}

/// A reader shows no history, so a step only a newer version can read leaves nothing out, and this
/// build's own document is read whole.
#[test]
fn a_reader_leaves_out_nothing_from_a_document_this_build_reads_whole() {
    let (path, _) = written_by_a_newer_build("newer-reading-op", |head| {
        head.replacen(r#""title":"t""#, r#""title":"u""#, 1)
    });
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            r#"INSERT INTO ops (json) VALUES ('{"Transpose":{"block_id":"x"}}')"#,
            [],
        )
        .unwrap();
    let db = Db::open_read_only(&path).unwrap();
    assert!(db.holds_newer(), "a newer step makes it read-only to open");
    assert!(!db.leaves_out_newer());
    assert_eq!(db.load_readable().unwrap(), db.load_document().unwrap());
}

/// Damage isn't taken for something newer by a reader either.
#[test]
fn a_reader_still_refuses_a_damaged_head() {
    let (path, _) = written_by_a_newer_build("newer-reading-damage", |head| {
        head.replacen(r#""style":"Bold""#, r#""style":"Strike""#, 1)
            .replacen(r#""text":"The last words.""#, r#""text":5"#, 1)
    });
    let db = Db::open_read_only(&path).unwrap();
    assert!(db.load_readable().is_err());
    assert!(!db.leaves_out_newer());
}

/// Every field this build leaves out at its default, written explicitly at that default, as
/// another program can write it: a heading's `centered`, a list item's `depth`, an image's `link`
/// and `align`, a code block's `language`, and a delete's `old_spans` and `clipped`.
fn at_explicit_defaults(json: &str) -> String {
    let written = json
        .replace(
            r#"{"Heading":{"level":1}}"#,
            r#"{"Heading":{"level":1,"centered":false}}"#,
        )
        .replace(
            r#"{"ListItem":{"ordered":true}}"#,
            r#"{"ListItem":{"ordered":true,"depth":0}}"#,
        )
        .replace(
            r#"{"Image":{"path":"a.png"}}"#,
            r#"{"Image":{"path":"a.png","link":null,"align":"Left"}}"#,
        )
        .replace(r#"{"Code":{}}"#, r#"{"Code":{"language":null}}"#)
        .replace(
            r#""removed":[]}"#,
            r#""removed":[],"old_spans":null,"clipped":[]}"#,
        );
    assert_ne!(written, json, "nothing was written at its default");
    written
}

/// A document of every kind of block with a field left out at its default, made by one step, and
/// cut short by a delete, with a snapshot of it, each row of which `edit` then changes.
fn written_at_defaults(name: &str, edit: impl Fn(&str) -> String) -> (PathBuf, Document, Document) {
    let path = kame_test_dir::folder(name).join("doc.kame");
    let block = |kind: BlockKind, text: &str| Block {
        id: Uuid::new_v4(),
        kind,
        text: text.to_string(),
        spans: Vec::new(),
    };
    let mut first = Document::new("t");
    first.blocks[0].text = "Kept words.".to_string();
    let blocks = vec![
        block(
            BlockKind::Heading {
                level: 1,
                centered: false,
            },
            "A heading",
        ),
        block(
            BlockKind::ListItem {
                ordered: true,
                depth: 0,
            },
            "An item",
        ),
        block(
            BlockKind::Image {
                path: "a.png".to_string(),
                link: None,
                align: ImageAlign::Left,
            },
            "",
        ),
        block(BlockKind::Code { language: None }, "let x = 1;"),
    ];
    let mut grown = first.clone();
    grown.blocks.extend(blocks.iter().cloned());
    let insert = Op::Batch {
        ops: blocks
            .into_iter()
            .enumerate()
            .map(|(at, block)| Op::InsertBlock {
                index: at + 1,
                block,
            })
            .collect(),
    };
    let mut cut = grown.clone();
    cut.blocks[0].text = "Kept.".to_string();
    let delete = Op::DeleteText {
        block_id: cut.blocks[0].id,
        start: 4,
        end: 10,
        text: " words".to_string(),
        removed: Vec::new(),
        old_spans: None,
        clipped: Vec::new(),
    };
    {
        let mut db = Db::open(&path).unwrap();
        db.save_document(&first).unwrap();
        db.record_edit(&grown, &insert).unwrap();
        db.record_edit(&cut, &delete).unwrap();
        db.create_snapshot(&cut, Some("cut")).unwrap();
    }
    let connection = rusqlite::Connection::open(&path).unwrap();
    for table in ["documents", "ops", "snapshots"] {
        let key = if table == "ops" { "seq" } else { "id" };
        let rows: Vec<(i64, String)> = {
            let mut statement = connection
                .prepare(&format!("SELECT {key}, json FROM {table}"))
                .unwrap();
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap();
            rows.map(Result::unwrap).collect()
        };
        for (at, json) in rows {
            connection
                .execute(
                    &format!("UPDATE {table} SET json = ?1 WHERE {key} = ?2"),
                    rusqlite::params![edit(&json), at],
                )
                .unwrap();
        }
    }
    (path, first, cut)
}

/// A field this build knows, written at the default it's left out at, is this build's own: the
/// document opens to be edited, its history undoes and redoes, and its snapshot restores.
#[test]
fn a_field_written_at_its_default_is_this_builds_own() {
    let (path, first, cut) = written_at_defaults("explicit-defaults", at_explicit_defaults);
    let db = Db::open_read_only(&path).unwrap();
    assert!(!db.holds_newer());
    assert!(!db.leaves_out_newer());
    assert_eq!(db.restore_snapshot(1).unwrap(), cut);
    assert_eq!(db.snapshot_documents().unwrap(), vec![(1, cut.clone())]);
    drop(db);

    let mut session = Session::open(&path).unwrap();
    assert!(!session.newer, "{:?}", session.last_error);
    assert!(!session.lossy);
    assert_eq!(session.document(), &cut);
    assert!(session.undo());
    assert!(session.undo());
    assert_eq!(session.document(), &first);
    assert!(session.redo());
    assert!(session.redo());
    assert_eq!(session.document(), &cut);
    assert!(session.restore(1));
    assert_eq!(session.document(), &cut);
    session.set_caret(Cursor {
        block_id: cut.blocks[0].id,
        offset: 0,
    });
    assert!(session.insert("Still "));
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert!(!reopened.newer);
    assert_eq!(reopened.document().blocks[0].text, "Still Kept.");
}

/// A field this build doesn't know is still from a newer version, even beside the defaults, and
/// even holding what looks like a default itself, in the head, a step of history or a snapshot.
#[test]
fn a_field_this_build_does_not_know_beside_the_defaults_is_still_newer() {
    let unknown = |json: &str| {
        at_explicit_defaults(json).replacen(
            r#""centered":false"#,
            r#""centered":false,"boxed":false"#,
            1,
        )
    };
    let (path, _, _) = written_at_defaults("explicit-defaults-and-unknown", unknown);
    let db = Db::open_read_only(&path).unwrap();
    assert!(db.leaves_out_newer(), "the head");
    assert!(matches!(
        db.restore_snapshot(1),
        Err(kame_core::storage::DbError::Newer)
    ));
    assert!(db.snapshot_documents().unwrap().is_empty(), "the snapshot");
    drop(db);
    let session = Session::open(&path).unwrap();
    assert!(session.newer);
    assert!(session.save_failed, "edits are refused");

    // Only in a step of history.
    let (path, _, _) = written_at_defaults("explicit-defaults-unknown-op", |json| {
        if json.contains("InsertBlock") {
            unknown(json)
        } else {
            at_explicit_defaults(json)
        }
    });
    let db = Db::open_read_only(&path).unwrap();
    assert!(!db.leaves_out_newer());
    assert!(db.holds_newer(), "the step");
}

/// A field written at another value than its default is read as it always was.
#[test]
fn a_field_written_at_another_value_is_read_as_ever() {
    let (path, _, cut) = written_at_defaults("explicit-non-defaults", |json| {
        at_explicit_defaults(json)
            .replace(r#""centered":false"#, r#""centered":true"#)
            .replace(r#""align":"Left""#, r#""align":"Right""#)
            .replace(r#""language":null"#, r#""language":"rust""#)
    });
    let session = Session::open(&path).unwrap();
    assert!(!session.newer);
    let kinds: Vec<&BlockKind> = session.document().blocks.iter().map(|b| &b.kind).collect();
    assert_eq!(
        kinds[1],
        &BlockKind::Heading {
            level: 1,
            centered: true
        }
    );
    assert!(matches!(
        kinds[3],
        BlockKind::Image {
            align: ImageAlign::Right,
            ..
        }
    ));
    assert_eq!(
        kinds[4],
        &BlockKind::Code {
            language: Some("rust".to_string())
        }
    );
    assert_eq!(session.document().blocks[0].text, cut.blocks[0].text);
}
