//! A logged document holding a kind of block only a newer kame knows. This build shows what it can
//! of it, and neither a sync nor the notebook's passes over it write anything, to the file or to
//! the logs, since what this build would write leaves out what it doesn't know.

use std::path::{Path, PathBuf};

use kame_core::lock::Locks;
use kame_core::session::Session;
use kame_core::storage::{Db, NEWER_PART};
use kame_notebook::stranded::reconcile;
use kame_notebook::Notebook;
use kame_sync::logs::{log_folder, Logged, MARKER};
use kame_sync::revlog::{Replica, Store};

const DEVICE: &str = "titan-bbbb";

fn logged_notebook(name: &str) -> (PathBuf, Notebook) {
    let root = kame_test_dir::folder(name).canonicalize().unwrap();
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    std::fs::write(root.join(MARKER), "").unwrap();
    (root.clone(), Notebook::open(&root).unwrap())
}

/// A document that has synced, so that its database remembers a revision in its logs.
fn synced(path: &Path) {
    let mut session = Session::open(path).unwrap();
    session.insert("Words.");
    Logged::open(&session, DEVICE)
        .unwrap()
        .sync(&mut session, 100)
        .unwrap();
}

/// Its paragraph, as a build knowing tables would have written one in its place.
fn as_a_newer_build_wrote_it(path: &Path) {
    let connection = rusqlite::Connection::open(path).unwrap();
    let head: String = connection
        .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let newer = head.replacen(r#""kind":"Paragraph""#, r#""kind":"Table""#, 1);
    assert_ne!(newer, head);
    connection
        .execute("UPDATE documents SET json = ?1 WHERE id = 1", [&newer])
        .unwrap();
}

/// Everything in the file, as it is on the disk.
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

/// Each log beside the document, by name, with what it holds.
fn logs_of(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut logs: Vec<(String, Vec<u8>)> = std::fs::read_dir(log_folder(path))
        .unwrap()
        .flatten()
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect();
    logs.sort();
    logs
}

/// Another machine's words, arriving in its own log, built on what this one last wrote.
fn words_arrive(path: &Path) {
    let folder = log_folder(path);
    let store = Store::load(&folder).unwrap();
    let heads = store.heads();
    let mut doc = store.document(&heads[0]);
    doc.blocks[0].text.push_str(" More from talos.");
    Replica {
        folder,
        device: "talos-aaaa".to_string(),
    }
    .sync(&doc, Some(&heads[0]), 300)
    .unwrap();
}

#[test]
fn a_sync_writes_nothing_from_a_document_this_build_can_only_partly_show() {
    let (root, _nb) = logged_notebook("newer-sync");
    let a = root.join("a.kame");
    synced(&a);
    as_a_newer_build_wrote_it(&a);
    words_arrive(&a);
    let (file, logs) = (everything_in(&a), logs_of(&a));

    let mut session = Session::open(&a).unwrap();
    assert!(session.lossy);
    assert_eq!(session.document().blocks[0].text, NEWER_PART);
    assert!(!session.insert("Typed here."));
    let mut logged = Logged::open(&session, DEVICE).unwrap();
    for now in [200, 400] {
        let outcome = logged.sync(&mut session, now).unwrap();
        assert_eq!(outcome.written, 0);
        assert!(!outcome.merged, "what arrived isn't merged into a view");
    }
    assert!(session.newer, "still read-only");
    assert_eq!(session.document().blocks[0].text, NEWER_PART);
    drop(logged);
    drop(session);
    assert_eq!(everything_in(&a), file);
    assert_eq!(logs_of(&a), logs);
}

/// A pass records the place of a database that synced before places were recorded. One from a newer
/// version is left as it is, which leaves it merely not known to be stranded.
#[test]
fn a_pass_does_not_stamp_a_document_from_a_newer_version() {
    let (root, nb) = logged_notebook("newer-stamp");
    let a = root.join("a.kame");
    synced(&a);
    {
        let state = Db::open_read_only(&a).unwrap().load_sync_state().unwrap();
        let mut db = Db::open(&a).unwrap();
        db.clear_sync_state().unwrap();
        db.save_sync_state(&state).unwrap();
    }
    as_a_newer_build_wrote_it(&a);
    let file = everything_in(&a);

    reconcile(&nb, &[], None, Locks::NONE);
    assert_eq!(Db::open_read_only(&a).unwrap().load_sync_place(), None);
    assert_eq!(everything_in(&a), file);
}
