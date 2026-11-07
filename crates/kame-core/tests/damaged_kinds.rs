//! A file whose heading levels and list depths are past what the model holds, as a damaged or
//! hand-made one can be: a level that isn't 1 to 6, a depth deeper than lists go, or either beyond
//! a byte or below zero. kame never writes one, so each is put into the file's JSON through SQL.
//! It opens with them fitted, as offsets are, and isn't taken for a newer version's.

use std::path::{Path, PathBuf};

use kame_core::edit::Op;
use kame_core::model::{Block, BlockKind, Document, MAX_LIST_DEPTH};
use kame_core::session::Session;
use kame_core::storage::Db;
use uuid::Uuid;

fn file(name: &str) -> PathBuf {
    kame_test_dir::folder(name).join("damaged.kame")
}

fn block(kind: BlockKind, text: &str) -> Block {
    Block {
        id: Uuid::new_v4(),
        kind,
        text: text.to_string(),
        spans: Vec::new(),
    }
}

const HEADING: BlockKind = BlockKind::Heading {
    level: 2,
    centered: false,
};
const ITEM: BlockKind = BlockKind::ListItem {
    ordered: false,
    depth: 1,
};

/// A heading and two list items, as kame writes them, to be damaged.
fn document() -> Document {
    let mut doc = Document::new("Damaged");
    doc.blocks = vec![
        block(HEADING, "A heading"),
        block(ITEM, "An item"),
        block(ITEM, "Another item"),
    ];
    doc
}

/// Each sound level and depth in `json` changed, in order, to one of `damage`.
fn damaged(json: &str, damage: &[&str]) -> String {
    let mut json = json.to_string();
    for value in damage {
        let (sound, field) = if json.contains(r#""level":2"#) {
            (r#""level":2"#, "level")
        } else {
            (r#""depth":1"#, "depth")
        };
        json = json.replacen(sound, &format!(r#""{field}":{value}"#), 1);
    }
    json
}

fn sql(path: &Path, statement: &str, value: &str) {
    rusqlite::Connection::open(path)
        .unwrap()
        .execute(statement, [value])
        .unwrap();
}

fn stored_head(path: &Path) -> String {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn kinds(doc: &Document) -> Vec<BlockKind> {
    doc.blocks.iter().map(|block| block.kind.clone()).collect()
}

fn fitted() -> Vec<BlockKind> {
    vec![
        BlockKind::Heading {
            level: 6,
            centered: false,
        },
        BlockKind::ListItem {
            ordered: false,
            depth: 0,
        },
        BlockKind::ListItem {
            ordered: false,
            depth: MAX_LIST_DEPTH,
        },
    ]
}

#[test]
fn a_head_with_a_level_and_depths_past_a_byte_opens_fitted_and_is_left_as_it_was() {
    let path = file("head");
    Db::open(&path).unwrap().save_document(&document()).unwrap();
    let head = damaged(&stored_head(&path), &["300", "-2", "4294967296"]);
    sql(&path, "UPDATE documents SET json = ?1 WHERE id = 1", &head);

    let session = Session::open(&path).unwrap();

    assert_eq!(kinds(session.document()), fitted());
    assert!(!session.newer, "taken for a newer version's");
    assert_eq!(session.last_error, None);
    drop(session);
    assert_eq!(stored_head(&path), head, "nothing is written on opening");
    assert!(!Db::open(&path).unwrap().holds_newer());
}

#[test]
fn a_level_and_depths_inside_a_byte_but_past_the_model_open_fitted() {
    let path = file("byte");
    Db::open(&path).unwrap().save_document(&document()).unwrap();
    let head = damaged(&stored_head(&path), &["9", "0", "40"]);
    sql(&path, "UPDATE documents SET json = ?1 WHERE id = 1", &head);

    let session = Session::open(&path).unwrap();

    assert_eq!(kinds(session.document()), fitted());
    assert!(!session.newer);
}

/// The step that made the heading recorded it at the level the head holds. Both are fitted the
/// same way, so it undoes, and redoes to the fitted heading.
#[test]
fn a_step_making_a_heading_past_the_levels_undoes_and_redoes() {
    let path = file("history");
    let mut doc = document();
    let was = BlockKind::Paragraph;
    doc.blocks[0].kind = was.clone();
    let step = Op::SetBlockKind {
        block_id: doc.blocks[0].id,
        old_kind: was.clone(),
        kind: HEADING,
    };
    doc.blocks[0].kind = HEADING;
    let mut db = Db::open(&path).unwrap();
    db.record_edit(&doc, &step).unwrap();
    drop(db);
    let head = damaged(&stored_head(&path), &["300"]);
    sql(&path, "UPDATE documents SET json = ?1 WHERE id = 1", &head);
    let op: String = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT json FROM ops", [], |row| row.get(0))
        .unwrap();
    sql(&path, "UPDATE ops SET json = ?1", &damaged(&op, &["300"]));

    let mut session = Session::open(&path).unwrap();
    assert_eq!(kinds(session.document())[0], fitted()[0]);
    assert!(session.undo(), "{:?}", session.last_error);
    assert_eq!(kinds(session.document())[0], was);
    assert!(session.redo(), "{:?}", session.last_error);
    assert_eq!(kinds(session.document())[0], fitted()[0]);
}

#[test]
fn a_snapshot_with_a_level_past_a_byte_restores_fitted() {
    let path = file("snapshot");
    let mut db = Db::open(&path).unwrap();
    db.save_document(&document()).unwrap();
    db.create_snapshot(&document(), Some("damaged")).unwrap();
    drop(db);
    let snapshot: String = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT json FROM snapshots", [], |row| row.get(0))
        .unwrap();
    let damaged_snapshot = damaged(&snapshot, &["300", "-2", "4294967296"]);
    sql(&path, "UPDATE snapshots SET json = ?1", &damaged_snapshot);

    let mut session = Session::open(&path).unwrap();
    let id = session.snapshots().unwrap()[0].id;
    assert!(session.restore(id), "{:?}", session.last_error);
    assert_eq!(kinds(session.document()), fitted());
}
