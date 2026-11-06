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

/// A notebook with no logs, holding `kinds.kame`, with a block of a kind only a newer kame knows,
/// and `fields.kame`, with a field only a newer kame knows. Each says something about otters, links
/// to `target.kame` and cites `knuth1981` in a footnote, in what this build can read of it.
fn notebook_with_newer_documents(name: &str) -> (PathBuf, Notebook) {
    use kame_core::model::{
        Anchor, Annotation, AnnotationKind, Block, BlockKind, Document, Metadata, Span, SpanStyle,
    };
    let root = kame_test_dir::folder(name).canonicalize().unwrap();
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    std::fs::create_dir_all(root.join("references")).unwrap();
    std::fs::write(
        root.join("references/knuth1981.yaml"),
        "kind: article\ntitle: Breaking paragraphs into lines\nauthors: [D. E. Knuth]\nyear: 1981\n",
    )
    .unwrap();
    drop(Session::open(&root.join("target.kame")).unwrap());
    for (stem, title) in [("kinds", "Newer kinds"), ("fields", "Newer fields")] {
        let words = "Otters hold hands. See target.";
        let first = Block {
            id: uuid::Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: words.to_string(),
            spans: vec![Span {
                start: words.len() - "target.".len(),
                end: words.len() - 1,
                style: SpanStyle::DocumentLink {
                    target: "target.kame".to_string(),
                },
            }],
        };
        let mut doc = Document::new(title);
        doc.annotations = vec![Annotation {
            id: uuid::Uuid::new_v4(),
            kind: AnnotationKind::Footnote,
            anchor: Anchor::block(first.id),
            body: "As in [knuth1981].".to_string(),
        }];
        doc.blocks = vec![
            first,
            Block {
                id: uuid::Uuid::new_v4(),
                kind: BlockKind::Paragraph,
                text: "A table of otters.".to_string(),
                spans: Vec::new(),
            },
        ];
        let path = root.join(format!("{stem}.kame"));
        let mut db = Db::open(&path).unwrap();
        db.save_document(&doc).unwrap();
        db.save_metadata(&Metadata {
            title: title.to_string(),
            ..Metadata::default()
        })
        .unwrap();
    }
    let newer = |path: &Path, from: &str, to: &str| {
        let connection = rusqlite::Connection::open(path).unwrap();
        let head: String = connection
            .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let newer = head.replacen(from, to, 1);
        assert_ne!(newer, head);
        connection
            .execute("UPDATE documents SET json = ?1 WHERE id = 1", [&newer])
            .unwrap();
    };
    newer(
        &root.join("kinds.kame"),
        r#""kind":"Paragraph","text":"A table of otters.""#,
        r#""kind":"Table","text":"A table of otters.""#,
    );
    newer(
        &root.join("fields.kame"),
        r#""text":"A table of otters.""#,
        r#""text":"A table of otters.","colour":"red""#,
    );
    (root.clone(), Notebook::open(&root).unwrap())
}

/// Lists, search, what links here and the references' citing read what they can of a document from
/// a newer kame, and say nothing of the rest, which the document says itself when it's opened.
#[test]
fn a_notebook_finds_what_it_can_read_of_documents_from_a_newer_version() {
    use std::sync::atomic::AtomicBool;
    let (root, nb) = notebook_with_newer_documents("newer-views");
    let (kinds, fields) = (root.join("kinds.kame"), root.join("fields.kame"));
    let before = (everything_in(&kinds), everything_in(&fields));
    let cancel = AtomicBool::new(false);

    let titles: Vec<String> = kame_notebook::list::listed(&nb)
        .into_iter()
        .map(|listed| listed.title)
        .collect();
    assert!(titles.contains(&"Newer kinds".to_string()), "{titles:?}");
    assert!(titles.contains(&"Newer fields".to_string()), "{titles:?}");

    let mut found = Vec::new();
    kame_notebook::search::search(&nb, "otters", None, &cancel, |hit| found.push(hit));
    let mut where_found: Vec<(String, String)> = found
        .iter()
        .map(|hit| (hit.relative.clone(), hit.title.clone()))
        .collect();
    where_found.sort();
    assert_eq!(
        where_found,
        [
            ("fields.kame".to_string(), "Newer fields".to_string()),
            ("fields.kame".to_string(), "Newer fields".to_string()),
            ("kinds.kame".to_string(), "Newer kinds".to_string()),
        ],
        "the table's words in kinds.kame aren't read, and the rest is"
    );
    let mut placeholder = Vec::new();
    kame_notebook::search::search(&nb, "newer version", None, &cancel, |hit| {
        placeholder.push(hit)
    });
    assert!(placeholder.is_empty(), "nothing stands in for the table");

    let mut linking: Vec<String> =
        kame_notebook::links::linking_to(&nb, &root.join("target.kame"), None, &cancel)
            .into_iter()
            .map(|from| from.relative)
            .collect();
    linking.sort();
    assert_eq!(linking, ["fields.kame", "kinds.kame"]);

    let citing = kame_notebook::references::citing(&nb);
    let mut cite = citing.get("knuth1981").cloned().unwrap_or_default();
    cite.sort();
    assert_eq!(cite, [fields.clone(), kinds.clone()]);

    assert_eq!((everything_in(&kinds), everything_in(&fields)), before);
}

/// A conflict copy from a newer kame is found as any other is, and isn't merged, since what this
/// build would take from it leaves out what it doesn't know. It stays as it is, to merge once kame
/// is updated, and so does the document.
#[test]
fn a_conflict_copy_from_a_newer_version_is_found_and_not_merged() {
    use kame_sync::files::{conflict_copies, merge_copy_into};
    for (name, from, to) in [
        (
            "newer-copy-field",
            r#""spans":["#,
            r#""colour":"red","spans":["#,
        ),
        (
            "newer-copy-kind",
            r#""kind":"Paragraph""#,
            r#""kind":"Table""#,
        ),
    ] {
        let dir = kame_test_dir::folder(name).canonicalize().unwrap();
        let doc = dir.join("a.kame");
        Session::open(&doc).unwrap().insert("Words.");
        let copy = dir.join("a.sync-conflict-20261103-011500-ABCDEFG.kame");
        std::fs::copy(&doc, &copy).unwrap();
        Session::open(&copy).unwrap().insert("More. ");
        let connection = rusqlite::Connection::open(&copy).unwrap();
        let head: String = connection
            .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let newer = head.replacen(from, to, 1);
        assert_ne!(newer, head, "{name}");
        connection
            .execute("UPDATE documents SET json = ?1 WHERE id = 1", [&newer])
            .unwrap();
        drop(connection);
        assert!(Db::open_read_only(&copy).unwrap().holds_newer(), "{name}");
        let (in_doc, in_copy) = (everything_in(&doc), everything_in(&copy));

        let mut session = Session::open(&doc).unwrap();
        assert_eq!(conflict_copies(&doc), std::slice::from_ref(&copy), "{name}");
        let said = merge_copy_into(&mut session, &copy)
            .err()
            .unwrap_or_else(|| panic!("{name} merged"))
            .to_string();
        assert_eq!(
            said, "the copy was written by a newer version of kame; update kame to merge it",
            "{name}"
        );
        assert_eq!(session.document().blocks[0].text, "Words.", "{name}");
        drop(session);
        assert_eq!(everything_in(&doc), in_doc, "{name}");
        assert_eq!(everything_in(&copy), in_copy, "{name}");
    }
}

/// A file from a newer kame that replaced an open document isn't merged with what's on the screen,
/// and nothing is written into it, not even the snapshot a merge takes first.
#[test]
fn a_file_from_a_newer_version_that_replaced_this_one_is_not_merged() {
    use kame_sync::files::merge_replaced;
    let dir = kame_test_dir::folder("newer-replaced")
        .canonicalize()
        .unwrap();
    let path = dir.join("a.kame");
    Session::open(&path).unwrap().insert("Words.");
    let other = dir.join("other.kame");
    std::fs::copy(&path, &other).unwrap();
    Session::open(&other).unwrap().insert("Theirs. ");
    let connection = rusqlite::Connection::open(&other).unwrap();
    let head: String = connection
        .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let newer = head.replacen(r#""spans":["#, r#""colour":"red","spans":["#, 1);
    assert_ne!(newer, head);
    connection
        .execute("UPDATE documents SET json = ?1 WHERE id = 1", [&newer])
        .unwrap();
    drop(connection);

    let mut old = Session::open(&path).unwrap();
    old.insert(" Mine.");
    std::fs::rename(&other, &path).unwrap();
    old.insert(" Unsaved.");
    assert!(old.replaced);
    let arrived = everything_in(&path);
    let said = merge_replaced(&old)
        .err()
        .expect("it isn't merged")
        .to_string();
    assert_eq!(
        said,
        "the file there now was written by a newer version of kame; update kame to merge it"
    );
    assert!(old.document().blocks[0].text.contains("Unsaved"));
    drop(old);
    assert_eq!(everything_in(&path), arrived);
}
