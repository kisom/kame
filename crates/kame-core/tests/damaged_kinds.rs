//! A file whose heading levels and list depths are past what the model holds. One a byte can't
//! hold, below zero or past 255, is damage, and opens with it fitted, as offsets are. One a byte
//! holds, a level outside 1 to 6 or a depth past 5, is what a newer kame that went further would
//! write, so the document opens read-only, as for a field this build doesn't know, shown fitted.
//! kame never writes either, so each is put into the file's JSON through SQL.

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

/// A level or depth a byte holds, past what this build's model has, is what a newer kame that
/// went further would write, so the document opens read-only, as for a field this build doesn't
/// know, showing it fitted, and nothing is written over it.
#[test]
fn a_level_or_depth_a_newer_kame_could_write_opens_read_only() {
    for (name, damage) in [
        ("depth", vec!["2", "7"]),
        ("level", vec!["7"]),
        ("level-zero", vec!["0"]),
        ("both", vec!["255", "1", "255"]),
    ] {
        let path = file(name);
        Db::open(&path).unwrap().save_document(&document()).unwrap();
        let head = damaged(&stored_head(&path), &damage);
        sql(&path, "UPDATE documents SET json = ?1 WHERE id = 1", &head);
        assert!(Db::open(&path).unwrap().holds_newer(), "{name}");

        let mut session = Session::open(&path).unwrap();

        assert!(session.newer, "{name}: not taken for a newer version's");
        let shown = kinds(session.document());
        assert!(
            shown.iter().all(|kind| match kind {
                BlockKind::Heading { level, .. } => (1..=6).contains(level),
                BlockKind::ListItem { depth, .. } => *depth <= MAX_LIST_DEPTH,
                _ => true,
            }),
            "{name}: shown unfitted: {shown:?}"
        );
        let block_id = session.document().blocks[0].id;
        session.set_caret(kame_core::edit::Cursor {
            block_id,
            offset: 0,
        });
        session.insert("Typed. ");
        let copy = path.with_file_name("copy.kame");
        assert!(
            session.save_as(&copy, None, |_, _| false).is_err(),
            "{name}"
        );
        session.save_state();
        drop(session);
        assert_eq!(stored_head(&path), head, "{name}: written over");
        assert!(!copy.exists(), "{name}");
    }
}

/// A depth of 0 written out, which this build leaves out, and depths and levels in what the model
/// has, are this build's own.
#[test]
fn a_level_and_depth_this_build_has_open_to_edit() {
    let path = file("own");
    Db::open(&path).unwrap().save_document(&document()).unwrap();
    let head = damaged(&stored_head(&path), &["6", "0", "5"]);
    sql(&path, "UPDATE documents SET json = ?1 WHERE id = 1", &head);
    let session = Session::open(&path).unwrap();
    assert!(!session.newer);
}

#[test]
fn a_step_holding_a_level_a_newer_kame_could_write_opens_read_only() {
    let path = file("newer-step");
    let mut doc = document();
    let step = Op::SetBlockKind {
        block_id: doc.blocks[0].id,
        old_kind: HEADING,
        kind: BlockKind::Paragraph,
    };
    doc.blocks[0].kind = BlockKind::Paragraph;
    Db::open(&path).unwrap().record_edit(&doc, &step).unwrap();
    let op: String = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT json FROM ops", [], |row| row.get(0))
        .unwrap();
    sql(&path, "UPDATE ops SET json = ?1", &damaged(&op, &["9"]));

    assert!(Db::open(&path).unwrap().holds_newer());
    assert!(Session::open(&path).unwrap().newer);
}

#[test]
fn a_snapshot_holding_a_depth_a_newer_kame_could_write_is_not_restored() {
    let path = file("newer-snapshot");
    let mut db = Db::open(&path).unwrap();
    db.save_document(&document()).unwrap();
    db.create_snapshot(&document(), Some("newer")).unwrap();
    drop(db);
    let snapshot: String = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT json FROM snapshots", [], |row| row.get(0))
        .unwrap();
    sql(
        &path,
        "UPDATE snapshots SET json = ?1",
        &damaged(&snapshot, &["2", "1", "8"]),
    );

    let mut session = Session::open(&path).unwrap();
    assert!(
        !session.newer,
        "a snapshot doesn't make the document read-only"
    );
    let id = session.snapshots().unwrap()[0].id;
    assert!(!session.restore(id));
    assert_eq!(kinds(session.document()), kinds(&document()));
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
