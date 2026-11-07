//! Checking a document's new path, deleting one, and moving one.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use kame_core::links::image_file;
use kame_core::lock::{DocumentLock, LockError, Locks};
use kame_core::model::{BlockKind, Document, ImageAlign, Metadata, Span, SpanStyle};
use kame_core::session::Session;
use kame_core::storage::Db;
use kame_notebook::links::Follow;
use kame_notebook::manage::{
    carry_out, check_new_path, delete_document, plan_move, DeleteError, MoveReport,
};
use kame_notebook::Notebook;
use kame_sync::logs::{log_folder, place_of, Logged, MARKER};
use kame_sync::revlog::Store;

fn folder(name: &str) -> PathBuf {
    kame_test_dir::folder(name).canonicalize().unwrap()
}

fn document(path: &Path, words: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut session = Session::open(path).unwrap();
    session.insert(words);
}

fn notebook(name: &str) -> (PathBuf, Notebook) {
    let root = folder(name);
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    document(&root.join("a.kame"), "A.");
    std::fs::write(root.join("note.md"), "# Note\n").unwrap();
    (root.clone(), Notebook::open(&root).unwrap())
}

#[test]
fn a_new_path_is_tidied_and_given_the_documents_extension() {
    let (root, nb) = notebook("manage-tidy");
    let a = root.join("a.kame");
    assert_eq!(check_new_path(&nb, &a, "b").unwrap(), root.join("b.kame"));
    assert_eq!(
        check_new_path(&nb, &a, "  ./x/y.kame ").unwrap(),
        root.join("x/y.kame")
    );
    assert_eq!(
        check_new_path(&nb, &a, "x/matière première").unwrap(),
        root.join("x/matière première.kame")
    );
    assert_eq!(
        check_new_path(&nb, &root.join("note.md"), "n").unwrap(),
        root.join("n.md")
    );
}

#[test]
fn a_new_path_is_refused_with_nothing_changed() {
    let (root, nb) = notebook("manage-refuse");
    let a = root.join("a.kame");
    document(&root.join("taken.kame"), "T.");
    std::fs::create_dir_all(root.join(".logs-only.kame.sync")).unwrap();
    let outside = folder("manage-refuse-outside");
    std::os::unix::fs::symlink(&outside, root.join("out-link")).unwrap();
    for typed in [
        "",
        "   ",
        "../out",
        "/etc/x.kame",
        "~/x.kame",
        "x/",
        ".hidden",
        "x/.y.kame",
        "static/a.kame",
        "images/a.kame",
        "theme/a.kame",
        "output/a.kame",
        "a.sync-conflict-1.kame",
        "a.txt",
        "taken",
        "logs-only",
        "out-link/a.kame",
    ] {
        assert!(check_new_path(&nb, &a, typed).is_err(), "{typed}");
    }
    assert!(!root.join("x").exists());
}

#[test]
fn the_refusal_for_a_taken_path_says_to_delete_it_first() {
    let (root, nb) = notebook("manage-taken");
    document(&root.join("taken.kame"), "T.");
    let err = check_new_path(&nb, &root.join("a.kame"), "taken").unwrap_err();
    assert_eq!(err, "something is already at taken.kame; delete it first");
}

#[test]
fn a_new_name_differing_only_in_case_is_allowed_where_the_disk_sees_one_file() {
    let (root, nb) = notebook("manage-case");
    if !root.join("A.kame").exists() {
        return;
    }
    assert_eq!(
        check_new_path(&nb, &root.join("a.kame"), "A").unwrap(),
        root.join("A.kame")
    );
}

#[test]
fn deleting_leaves_nothing_named_for_the_document() {
    let root = folder("manage-delete");
    let x = root.join("X.kame");
    document(&x, "X.");
    for name in [
        "X.sync-conflict-1.kame",
        ".X.retired.kame",
        ".X.retired-2.kame",
    ] {
        std::fs::copy(&x, root.join(name)).unwrap();
    }
    for name in ["X.kame-wal", "X.kame-shm", "X.sync-conflict-1.kame.merged"] {
        std::fs::write(root.join(name), "").unwrap();
    }
    std::fs::create_dir_all(root.join(".X.kame.sync")).unwrap();
    std::fs::write(root.join(".X.kame.sync/titan.log"), "").unwrap();
    document(&root.join("XY.kame"), "XY.");
    std::fs::copy(root.join("XY.kame"), root.join(".XY.retired.kame")).unwrap();
    delete_document(&x, Locks::NONE).unwrap();
    let mut left: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, [".XY.retired.kame", "XY.kame"]);
}

#[test]
fn deleting_a_markdown_document_removes_only_its_file() {
    let root = folder("manage-delete-md");
    std::fs::write(root.join("n.md"), "# N\n").unwrap();
    std::fs::write(root.join("n.sync-conflict-1.kame"), "").unwrap();
    assert!(!delete_document(&root.join("n.md"), Locks::NONE).unwrap());
    assert!(!root.join("n.md").exists());
    assert!(root.join("n.sync-conflict-1.kame").exists());
}

// Moving a document.

const DEVICE: &str = "here-0000";
const NOW: i64 = 100;

fn document_link(target: &str) -> SpanStyle {
    SpanStyle::DocumentLink {
        target: target.to_string(),
    }
}

/// Adds `words` to the end of the document's first block, styled as a link.
fn link(path: &Path, words: &str, style: SpanStyle) {
    let mut session = Session::open(path).unwrap();
    let mut doc = session.document().clone();
    let block = &mut doc.blocks[0];
    let start = block.text.len();
    block.text.push_str(words);
    block.spans.push(Span {
        start,
        end: start + words.len(),
        style,
    });
    assert!(session.replace_document(doc));
}

fn links_of(doc: &Document) -> Vec<String> {
    let mut found = Vec::new();
    for block in &doc.blocks {
        for span in &block.spans {
            match &span.style {
                SpanStyle::DocumentLink { target } => found.push(target.clone()),
                SpanStyle::Link { url } => found.push(url.clone()),
                _ => {}
            }
        }
        if let BlockKind::Image {
            link: Some(link), ..
        } = &block.kind
        {
            found.push(link.clone());
        }
    }
    found
}

fn read(path: &Path) -> Document {
    Session::open(path).unwrap().document().clone()
}

fn make_move(nb: &Notebook, from: &Path, to: &Path) -> MoveReport {
    let plan = plan_move(nb, from, to, None, Locks::NONE, DEVICE, NOW);
    carry_out(nb, plan, None, Locks::NONE, DEVICE, NOW).unwrap()
}

fn plain_notebook(name: &str) -> (PathBuf, Notebook) {
    let root = folder(name);
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    (root.clone(), Notebook::open(&root).unwrap())
}

fn logged_notebook(name: &str) -> (PathBuf, Notebook) {
    let (root, nb) = plain_notebook(name);
    std::fs::write(root.join(MARKER), "").unwrap();
    (root, nb)
}

fn sync(path: &Path) {
    let mut session = Session::open(path).unwrap();
    let mut logged = Logged::open(&session, DEVICE).unwrap();
    logged.sync(&mut session, NOW).unwrap();
}

/// The one latest revision in a document's logs.
fn head(path: &Path) -> (Document, Metadata) {
    let store = Store::load(&log_folder(path)).unwrap();
    let heads = store.heads();
    assert_eq!(heads.len(), 1);
    (store.document(&heads[0]), store.metadata(&heads[0]))
}

fn assert_follows(nb: &Notebook, target: &str, base: &Path, to: &Path) {
    assert_eq!(
        nb.follow(target, base),
        Follow::Open(to.to_path_buf()),
        "{target}"
    );
}

/// Makes a document's head say something only a newer kame knows.
fn make_newer(path: &Path) {
    let connection = rusqlite::Connection::open(path).unwrap();
    let head: String = connection
        .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let newer = head.replacen("\"spans\":[", "\"future\":1,\"spans\":[", 1);
    assert_ne!(newer, head);
    connection
        .execute("UPDATE documents SET json = ?1 WHERE id = 1", [&newer])
        .unwrap();
}

#[test]
fn every_form_of_link_follows_a_rename() {
    let (root, nb) = plain_notebook("move-forms");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    let absolute = a.display().to_string();
    link(&linker, "one", document_link("a.kame"));
    link(&linker, "two", document_link("a"));
    link(&linker, "three", document_link("a.kame#Heading"));
    link(
        &linker,
        "four",
        SpanStyle::Link {
            url: "a".to_string(),
        },
    );
    link(&linker, "five", document_link(&absolute));
    {
        let mut session = Session::open(&linker).unwrap();
        assert!(session.insert_image("p.png".to_string()));
        let mut doc = session.document().clone();
        let image = doc
            .blocks
            .iter_mut()
            .find(|block| matches!(block.kind, BlockKind::Image { .. }))
            .unwrap();
        image.kind = BlockKind::Image {
            path: "p.png".to_string(),
            link: Some("a.kame".to_string()),
            align: ImageAlign::Left,
        };
        assert!(session.replace_document(doc));
    }
    let below = root.join("sub/below.kame");
    document(&below, "");
    link(&below, "up", document_link("../a.kame"));

    let b = root.join("b.kame");
    let report = make_move(&nb, &a, &b);
    let mut changed = report.changed.clone();
    changed.sort();
    assert_eq!(changed, ["linker.kame", "sub/below.kame"]);
    assert!(report.left.is_empty());
    let links = links_of(&read(&linker));
    assert_eq!(
        links,
        [
            "b.kame",
            "b.kame",
            "b.kame#Heading",
            "b.kame",
            b.to_str().unwrap(),
            "b.kame",
        ]
    );
    for target in &links {
        assert_follows(&nb, target, &linker, &b);
    }
    assert_eq!(links_of(&read(&below)), ["../b.kame"]);
    assert_follows(&nb, "../b.kame", &below, &b);

    // And on to another folder, where the same links have to go down into it.
    let c = root.join("x/y/c.kame");
    make_move(&nb, &b, &c);
    let links = links_of(&read(&linker));
    assert_eq!(links[0], "x/y/c.kame");
    assert_eq!(links[2], "x/y/c.kame#Heading");
    assert_eq!(links[4], c.to_str().unwrap());
    for target in &links {
        assert_follows(&nb, target, &linker, &c);
    }
    assert_eq!(links_of(&read(&below)), ["../x/y/c.kame"]);
}

#[test]
fn a_link_elsewhere_is_left_alone() {
    let (root, nb) = plain_notebook("move-elsewhere");
    let a = root.join("a.kame");
    document(&a, "A.");
    document(&root.join("other/a.kame"), "Another A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("other/a.kame"));
    link(
        &linker,
        "two",
        SpanStyle::Link {
            url: "https://example.com/a.kame".to_string(),
        },
    );
    link(&linker, "three", document_link("#Heading"));
    let before = read(&linker);
    let history = Session::open(&linker).unwrap().history();

    let report = make_move(&nb, &a, &root.join("b.kame"));
    assert!(report.changed.is_empty());
    assert_eq!(read(&linker), before);
    assert_eq!(Session::open(&linker).unwrap().history(), history);
}

#[test]
fn a_move_across_folders_fixes_its_own_links_and_images() {
    let (root, nb) = plain_notebook("move-own");
    let own = root.join("own.kame");
    document(&own, "");
    document(&root.join("sib.kame"), "Sib.");
    std::fs::create_dir_all(root.join("images")).unwrap();
    std::fs::write(root.join("images/p.png"), "p").unwrap();
    link(&own, "one", document_link("sib.kame"));
    link(&own, "two", document_link("#Heading"));
    {
        let mut session = Session::open(&own).unwrap();
        assert!(session.insert_image("images/p.png".to_string()));
    }
    let to = root.join("deep/er/own.kame");
    let report = make_move(&nb, &own, &to);
    assert_eq!(report.changed, ["deep/er/own.kame"]);
    let doc = read(&to);
    assert_eq!(links_of(&doc), ["../../sib.kame", "#Heading"]);
    let stored = doc
        .blocks
        .iter()
        .find_map(|block| match &block.kind {
            BlockKind::Image { path, .. } => Some(path.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(stored, "../../images/p.png");
    assert_eq!(std::fs::read(image_file(&stored, Some(&to))).unwrap(), b"p");
}

#[test]
fn a_rename_in_the_same_folder_leaves_how_an_image_is_written() {
    let (root, nb) = plain_notebook("move-image-same-folder");
    let a = root.join("a.kame");
    document(&a, "A.");
    std::fs::write(root.join("p.png"), "p").unwrap();
    let absolute = root.join("p.png").display().to_string();
    {
        let mut session = Session::open(&a).unwrap();
        assert!(session.insert_image(absolute.clone()));
    }
    let b = root.join("b.kame");
    let report = make_move(&nb, &a, &b);
    assert!(report.changed.is_empty());
    assert!(read(&b)
        .blocks
        .iter()
        .any(|block| matches!(&block.kind, BlockKind::Image { path, .. } if *path == absolute)));
}

#[test]
fn a_document_linking_to_itself_by_name_follows() {
    let (root, nb) = plain_notebook("move-itself");
    let a = root.join("a.kame");
    document(&a, "");
    link(&a, "me", document_link("a.kame"));
    let b = root.join("b.kame");
    let report = make_move(&nb, &a, &b);
    assert_eq!(report.changed, ["b.kame"]);
    assert_eq!(links_of(&read(&b)), ["b.kame"]);
}

#[test]
fn a_rename_carries_files_and_history() {
    let (root, nb) = plain_notebook("move-carries");
    let x = root.join("X.kame");
    let y = root.join("Y.kame");
    let (history, snapshots) = {
        let mut session = Session::open(&x).unwrap();
        session.insert("Some words.");
        session.snapshot(Some("kept"));
        session.insert(" More.");
        // A title of its own, so that the move has no reason to add a step.
        let mut doc = session.document().clone();
        doc.title = "Mine".to_string();
        assert!(session.replace_document(doc));
        let mut metadata = session.metadata.clone();
        metadata.title = "Mine".to_string();
        session.set_metadata(metadata);
        (session.history(), session.snapshots().unwrap())
    };
    for name in [
        "X.sync-conflict-1.kame",
        ".X.retired.kame",
        ".X.retired-2.kame",
    ] {
        std::fs::copy(&x, root.join(name)).unwrap();
    }
    for name in ["X.kame-wal", "X.kame-shm", "X.sync-conflict-1.kame.merged"] {
        std::fs::write(root.join(name), "").unwrap();
    }
    std::fs::create_dir_all(root.join(".X.kame.sync")).unwrap();
    std::fs::write(root.join(".X.kame.sync/titan.log"), "").unwrap();
    document(&root.join("XY.kame"), "XY.");
    std::fs::copy(root.join("XY.kame"), root.join(".XY.retired.kame")).unwrap();

    let report = make_move(&nb, &x, &y);
    assert!(report.left.is_empty());
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.contains("XY"))
        .collect();
    names.sort();
    // SQLite folds in what's beside the file when the move's last connection closes, so these
    // leave no `-wal` or `-shm` here. `words_only_in_the_wal_move_with_the_file` shows it arrives.
    assert_eq!(
        names,
        [
            ".Y.kame.sync",
            ".Y.retired-2.kame",
            ".Y.retired.kame",
            "Y.kame",
            "Y.sync-conflict-1.kame",
            "Y.sync-conflict-1.kame.merged",
            "notebook.yaml",
        ]
    );
    assert!(root.join(".Y.kame.sync/titan.log").is_file());
    assert!(root.join("XY.kame").is_file());
    assert!(root.join(".XY.retired.kame").is_file());
    let mut session = Session::open(&y).unwrap();
    assert_eq!(session.history(), history);
    let carried = session.snapshots().unwrap();
    assert_eq!(carried.len(), snapshots.len());
    assert_eq!(carried[0].id, snapshots[0].id);
    assert_eq!(carried[0].label, snapshots[0].label);
}

#[test]
fn a_rename_changes_a_matching_title_and_a_move_within_the_same_name_does_not() {
    let (root, nb) = plain_notebook("move-title");
    let a = root.join("a.kame");
    document(&a, "A.");
    let b = root.join("b.kame");
    make_move(&nb, &a, &b);
    {
        let session = Session::open(&b).unwrap();
        assert_eq!(session.document().title, "b");
        assert_eq!(session.metadata.title, "b");
    }

    let history = Session::open(&b).unwrap().history();
    let moved = root.join("x/b.kame");
    make_move(&nb, &b, &moved);
    {
        let session = Session::open(&moved).unwrap();
        assert_eq!(session.document().title, "b");
        assert_eq!(session.metadata.title, "b");
        assert_eq!(session.history(), history);
    }

    let mine = root.join("mine.kame");
    {
        let mut session = Session::open(&mine).unwrap();
        let mut metadata = session.metadata.clone();
        metadata.title = "A title of my own".to_string();
        session.set_metadata(metadata);
    }
    let renamed = root.join("renamed.kame");
    make_move(&nb, &mine, &renamed);
    let session = Session::open(&renamed).unwrap();
    assert_eq!(session.metadata.title, "A title of my own");
    assert_eq!(session.document().title, "renamed");
}

#[test]
fn a_title_change_reaches_the_logs() {
    let (root, nb) = logged_notebook("move-title-logs");
    let a = root.join("a.kame");
    document(&a, "A.");
    sync(&a);
    let b = root.join("b.kame");
    make_move(&nb, &a, &b);
    assert!(!log_folder(&a).exists());
    let (doc, metadata) = head(&b);
    assert_eq!(metadata.title, "b");
    assert_eq!(doc.title, "b");
}

#[test]
fn changes_to_a_closed_logged_document_reach_its_logs() {
    let (root, nb) = logged_notebook("move-closed-logged");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    sync(&linker);
    let report = make_move(&nb, &a, &root.join("b.kame"));
    assert_eq!(report.changed, ["linker.kame"]);
    assert_eq!(links_of(&head(&linker).0), ["b.kame"]);
}

#[test]
fn a_document_with_only_logs_is_changed_too() {
    let (root, nb) = logged_notebook("move-logs-only");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    sync(&linker);
    std::fs::remove_file(&linker).unwrap();

    let report = make_move(&nb, &a, &root.join("b.kame"));
    assert_eq!(report.changed, ["linker.kame"]);
    assert_eq!(links_of(&head(&linker).0), ["b.kame"]);
}

#[test]
fn a_document_with_only_logs_can_itself_be_moved() {
    let (root, nb) = logged_notebook("move-logs-only-moved");
    let a = root.join("a.kame");
    document(&a, "The words.");
    sync(&a);
    std::fs::remove_file(&a).unwrap();
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));

    let b = root.join("x/b.kame");
    let report = make_move(&nb, &a, &b);
    assert!(report.left.is_empty(), "{:?}", report.left);
    assert!(!a.exists());
    assert!(!log_folder(&a).exists());
    assert_eq!(links_of(&read(&linker)), ["x/b.kame"]);
    assert!(read(&b).blocks[0].text.contains("The words."));
    assert_eq!(head(&b).1.title, "b");
}

#[test]
fn deleting_a_document_with_only_logs_removes_them() {
    let (root, _nb) = logged_notebook("delete-logs-only");
    let a = root.join("a.kame");
    document(&a, "The words.");
    sync(&a);
    std::fs::remove_file(&a).unwrap();
    assert!(
        !delete_document(&a, Locks::NONE).unwrap(),
        "no logs were left"
    );
    assert!(!log_folder(&a).exists());
}

/// Every file in a folder, with what's in it.
fn files_in(folder: &Path) -> Vec<(std::ffi::OsString, Vec<u8>)> {
    let mut files: Vec<_> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), std::fs::read(entry.path()).unwrap())
        })
        .collect();
    files.sort();
    files
}

#[test]
fn deleting_a_stale_copy_leaves_the_logs_of_the_document_now_at_its_path() {
    let (root, _nb) = logged_notebook("delete-stale-copy");
    let a = root.join("a.kame");
    document(&a, "Old words, deleted on another machine.");
    sync(&a);
    // What this machine still holds, from before a new document was made at that path elsewhere.
    let stale = root.join("stale");
    std::fs::rename(&a, &stale).unwrap();
    std::fs::remove_dir_all(log_folder(&a)).unwrap();
    document(&a, "A new document.");
    sync(&a);
    std::fs::remove_file(&a).unwrap();
    std::fs::rename(&stale, &a).unwrap();
    std::fs::copy(&a, root.join(".a.retired.kame")).unwrap();
    let logs = files_in(&log_folder(&a));
    assert!(!logs.is_empty());

    assert!(
        delete_document(&a, Locks::NONE).unwrap(),
        "it says it left the logs"
    );

    assert!(!a.exists());
    assert!(!root.join(".a.retired.kame").exists());
    assert_eq!(files_in(&log_folder(&a)), logs);
}

#[test]
fn deleting_a_stale_copy_with_no_logs_at_its_path_left_none() {
    let (root, _nb) = logged_notebook("delete-stale-no-logs");
    let a = root.join("a.kame");
    document(&a, "Old words.");
    sync(&a);
    std::fs::remove_dir_all(log_folder(&a)).unwrap();
    assert!(!delete_document(&a, Locks::NONE).unwrap());
    assert!(!a.exists());
}

/// A file `.X.retired.kame` holding a different document, as a path reuse leaves one.
fn another_documents_retired_copy(root: &Path, name: &str) -> Vec<u8> {
    let made = root.join("made-elsewhere");
    document(&made, "Words that never synced.");
    let retired = root.join(name);
    std::fs::rename(&made, &retired).unwrap();
    std::fs::read(&retired).unwrap()
}

#[test]
fn deleting_leaves_a_retired_copy_of_another_document() {
    let (root, _nb) = logged_notebook("delete-retired-other");
    let x = root.join("X.kame");
    let words = another_documents_retired_copy(&root, ".X.retired.kame");
    document(&x, "A new, unrelated document.");
    sync(&x);
    std::fs::copy(&x, root.join(".X.retired-2.kame")).unwrap();
    std::fs::write(root.join(".X.retired-3.kame"), "can't be read").unwrap();

    delete_document(&x, Locks::NONE).unwrap();

    assert_eq!(std::fs::read(root.join(".X.retired.kame")).unwrap(), words);
    assert!(!root.join(".X.retired-2.kame").exists(), "its own goes");
    assert_eq!(
        std::fs::read(root.join(".X.retired-3.kame")).unwrap(),
        b"can't be read"
    );
}

#[test]
fn a_move_leaves_a_retired_copy_of_another_document_under_its_name() {
    let (root, nb) = logged_notebook("move-retired-other");
    let (x, y) = (root.join("X.kame"), root.join("Y.kame"));
    let words = another_documents_retired_copy(&root, ".X.retired.kame");
    document(&x, "A new, unrelated document.");
    sync(&x);
    std::fs::copy(&x, root.join(".X.retired-2.kame")).unwrap();
    std::fs::write(root.join(".X.retired-3.kame"), "can't be read").unwrap();

    make_move(&nb, &x, &y);

    assert_eq!(std::fs::read(root.join(".X.retired.kame")).unwrap(), words);
    assert!(!root.join(".Y.retired.kame").exists());
    assert!(root.join(".Y.retired-2.kame").is_file(), "its own follows");
    assert!(!root.join(".X.retired-2.kame").exists());
    assert_eq!(
        std::fs::read(root.join(".X.retired-3.kame")).unwrap(),
        b"can't be read"
    );
}

#[test]
fn a_document_with_only_logs_is_known_by_the_id_in_them() {
    let (root, _nb) = logged_notebook("delete-retired-logs-only");
    let x = root.join("X.kame");
    let words = another_documents_retired_copy(&root, ".X.retired.kame");
    document(&x, "The words.");
    sync(&x);
    std::fs::rename(&x, root.join(".X.retired-2.kame")).unwrap();

    delete_document(&x, Locks::NONE).unwrap();

    assert_eq!(std::fs::read(root.join(".X.retired.kame")).unwrap(), words);
    assert!(!root.join(".X.retired-2.kame").exists(), "its own goes");
    assert!(!log_folder(&x).exists());
}

#[test]
fn logs_with_no_head_to_read_leave_every_retired_copy() {
    let (root, _nb) = logged_notebook("delete-retired-no-head");
    let x = root.join("X.kame");
    document(&x, "The words.");
    sync(&x);
    std::fs::rename(&x, root.join(".X.retired.kame")).unwrap();
    let folder = log_folder(&x);
    for entry in std::fs::read_dir(&folder).unwrap().flatten() {
        std::fs::write(entry.path(), "").unwrap();
    }

    delete_document(&x, Locks::NONE).unwrap();

    assert!(root.join(".X.retired.kame").is_file());
    assert!(!folder.exists());
}

#[test]
fn a_database_that_cannot_be_read_leaves_every_retired_copy() {
    let (root, _nb) = logged_notebook("delete-retired-unreadable");
    let x = root.join("X.kame");
    document(&x, "The words.");
    std::fs::copy(&x, root.join(".X.retired.kame")).unwrap();
    std::fs::write(&x, "not a database").unwrap();

    delete_document(&x, Locks::NONE).unwrap();

    assert!(!x.exists());
    assert!(root.join(".X.retired.kame").is_file());
}

#[test]
fn deleting_a_document_in_step_with_its_logs_removes_them() {
    let (root, _nb) = logged_notebook("delete-in-step");
    let a = root.join("a.kame");
    document(&a, "The words.");
    sync(&a);
    assert!(
        !delete_document(&a, Locks::NONE).unwrap(),
        "no logs were left"
    );
    assert!(!a.exists());
    assert!(!log_folder(&a).exists());
}

#[test]
fn deleting_what_isnt_there_says_so() {
    let (root, _nb) = logged_notebook("delete-nothing");
    let err = delete_document(&root.join("a.kame"), Locks::NONE).unwrap_err();
    assert!(
        matches!(&err, DeleteError::Io(err) if err.kind() == std::io::ErrorKind::NotFound),
        "{err:?}"
    );
}

/// A second machine: its own folder, holding a copy of the document with the same past, whose
/// logs are played across by copying them.
fn second_machine(root: &Path, linker: &Path, name: &str) -> PathBuf {
    let elsewhere = folder(name);
    std::fs::write(elsewhere.join(MARKER), "").unwrap();
    let copy = elsewhere.join(linker.strip_prefix(root).unwrap());
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(linker, &copy).unwrap();
    let (from, to) = (log_folder(linker), log_folder(&copy));
    std::fs::create_dir_all(&to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
    copy
}

fn write_and_send(copy: &Path, linker: &Path, change: impl FnOnce(&mut Session)) {
    let mut session = Session::open(copy).unwrap();
    change(&mut session);
    let mut logged = Logged::open(&session, "there-1111").unwrap();
    logged.sync(&mut session, NOW + 1).unwrap();
    let (from, to) = (log_folder(copy), log_folder(linker));
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

#[test]
fn a_link_only_in_the_logs_is_found() {
    let (root, nb) = logged_notebook("move-only-in-logs");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "Words.");
    sync(&linker);
    let copy = second_machine(&root, &linker, "move-only-in-logs-there");
    write_and_send(&copy, &linker, |session| {
        let mut doc = session.document().clone();
        let block = &mut doc.blocks[0];
        let start = block.text.len();
        block.text.push_str(" Link.");
        block.spans.push(Span {
            start,
            end: block.text.len(),
            style: document_link("a.kame"),
        });
        assert!(session.replace_document(doc));
    });
    assert!(links_of(&read(&linker)).is_empty());

    let report = make_move(&nb, &a, &root.join("b.kame"));
    assert_eq!(report.changed, ["linker.kame"]);
    assert_eq!(links_of(&head(&linker).0), ["b.kame"]);
    assert_eq!(links_of(&read(&linker)), ["b.kame"]);
}

/// Its database has the same link already, so that looking for one it lacks wouldn't find this.
#[test]
fn another_use_of_a_link_arriving_in_the_logs_is_changed_too() {
    let (root, nb) = logged_notebook("move-again-in-logs");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    sync(&linker);
    let copy = second_machine(&root, &linker, "move-again-in-logs-there");
    write_and_send(&copy, &linker, |session| {
        let mut doc = session.document().clone();
        let block = &mut doc.blocks[0];
        let start = block.text.len();
        block.text.push_str(" two");
        block.spans.push(Span {
            start: start + 1,
            end: block.text.len(),
            style: document_link("a.kame"),
        });
        assert!(session.replace_document(doc));
    });

    make_move(&nb, &a, &root.join("b.kame"));
    assert_eq!(links_of(&head(&linker).0), ["b.kame", "b.kame"]);
}

#[test]
fn markdown_and_newer_documents_are_listed_and_untouched() {
    let (root, nb) = plain_notebook("move-left");
    let a = root.join("a.kame");
    document(&a, "A.");
    let note = root.join("note.md");
    std::fs::write(&note, "See [a](a.kame).\n").unwrap();
    let longer = root.join("note.markdown");
    std::fs::write(&longer, "See [a](a.kame).\n").unwrap();
    let newer = root.join("newer.kame");
    document(&newer, "");
    link(&newer, "one", document_link("a.kame"));
    make_newer(&newer);
    let newer_bytes = std::fs::read(&newer).unwrap();

    let report = make_move(&nb, &a, &root.join("b.kame"));
    assert!(report.changed.is_empty());
    assert_eq!(std::fs::read(&note).unwrap(), b"See [a](a.kame).\n");
    assert_eq!(std::fs::read(&longer).unwrap(), b"See [a](a.kame).\n");
    assert_eq!(std::fs::read(&newer).unwrap(), newer_bytes);
    let mut left: Vec<&str> = report
        .left
        .iter()
        .map(|left| left.relative.as_str())
        .collect();
    left.sort();
    assert_eq!(left, ["newer.kame", "note.markdown", "note.md"]);
    for entry in &report.left {
        let wants = if entry.relative == "newer.kame" {
            "newer version"
        } else {
            "Markdown"
        };
        assert!(entry.why.contains(wants), "{}", entry.why);
        assert_eq!(entry.path, root.join(&entry.relative));
    }
}

#[test]
fn a_moved_document_from_a_newer_kame_moves_and_is_listed_and_untouched() {
    let (root, nb) = plain_notebook("move-newer-itself");
    let a = root.join("a.kame");
    document(&a, "");
    link(&a, "me", document_link("a.kame"));
    make_newer(&a);
    let bytes = std::fs::read(&a).unwrap();
    let b = root.join("x/b.kame");
    let report = make_move(&nb, &a, &b);
    assert!(!a.exists());
    assert_eq!(std::fs::read(&b).unwrap(), bytes);
    assert_eq!(report.left.len(), 1);
    assert_eq!(report.left[0].relative, "x/b.kame");
    assert!(report.left[0].why.contains("newer version"));
}

#[test]
fn a_rename_in_case_only_works_where_the_disk_sees_one_file() {
    let (root, nb) = plain_notebook("move-case");
    let a = root.join("a.kame");
    document(&a, "A.");
    if !root.join("A.kame").exists() {
        return;
    }
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    std::fs::copy(&a, root.join(".a.retired.kame")).unwrap();
    let upper = root.join("A.kame");
    let report = make_move(&nb, &a, &upper);
    assert!(report.left.is_empty());
    assert_eq!(links_of(&read(&linker)), ["A.kame"]);
    let names: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"A.kame".to_string()), "{names:?}");
    assert!(!names.contains(&"a.kame".to_string()), "{names:?}");
    assert!(names.contains(&".A.retired.kame".to_string()), "{names:?}");
    assert_eq!(read(&upper).title, "A");
}

#[test]
fn a_move_to_where_it_already_is_changes_nothing() {
    let (root, nb) = plain_notebook("move-nowhere");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a"));
    let report = make_move(&nb, &a, &a);
    assert!(report.changed.is_empty());
    assert!(a.is_file());
    assert_eq!(links_of(&read(&linker)), ["a"]);
}

#[test]
fn a_moved_markdown_document_with_relative_links_is_listed_and_untouched() {
    let (root, nb) = plain_notebook("move-markdown");
    let words = "See [s](sib.kame) and ![p](p.png).\n";
    std::fs::write(root.join("m.md"), words).unwrap();
    std::fs::write(root.join("p.md"), "![p](p.png)\n").unwrap();
    std::fs::write(root.join("web.md"), "[s](https://example.com) [h](#h)\n").unwrap();
    document(&root.join("sib.kame"), "Sib.");

    let to = root.join("sub/m.md");
    let report = make_move(&nb, &root.join("m.md"), &to);
    assert!(!root.join("m.md").exists());
    assert_eq!(std::fs::read(&to).unwrap(), words.as_bytes());
    assert_eq!(report.left.len(), 1);
    assert_eq!(report.left[0].relative, "sub/m.md");
    assert_eq!(report.left[0].path, to);
    assert!(report.left[0].why.contains("weren't changed"));

    // An image alone is relative too.
    let report = make_move(&nb, &root.join("p.md"), &root.join("sub/p.md"));
    assert_eq!(report.left[0].relative, "sub/p.md");

    // Nothing here depends on the folder.
    let report = make_move(&nb, &root.join("web.md"), &root.join("sub/web.md"));
    assert!(report.left.is_empty());
    // And a rename doesn't change a folder.
    let report = make_move(&nb, &to, &root.join("sub/n.md"));
    assert!(report.left.is_empty());
}

#[test]
fn a_document_that_cannot_be_written_is_listed_and_the_others_still_change() {
    let (root, nb) = plain_notebook("move-read-only");
    let a = root.join("a.kame");
    document(&a, "A.");
    let locked = root.join("locked.kame");
    document(&locked, "");
    link(&locked, "one", document_link("a.kame"));
    let other = root.join("other.kame");
    document(&other, "");
    link(&other, "one", document_link("a.kame"));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o444)).unwrap();
    let bytes = std::fs::read(&locked).unwrap();

    let b = root.join("b.kame");
    let report = make_move(&nb, &a, &b);
    assert_eq!(report.changed, ["other.kame"]);
    assert_eq!(report.left.len(), 1);
    assert_eq!(report.left[0].relative, "locked.kame");
    assert!(!report.left[0].why.is_empty());
    assert_eq!(std::fs::read(&locked).unwrap(), bytes);
    assert_eq!(links_of(&read(&other)), ["b.kame"]);
    assert!(b.is_file());
}

#[test]
fn the_open_document_changes_through_its_session() {
    let (root, nb) = plain_notebook("move-open");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    let mut session = Session::open(&linker).unwrap();
    let before = session.history();
    let b = root.join("b.kame");
    let plan = plan_move(
        &nb,
        &a,
        &b,
        Some((linker.as_path(), session.document())),
        Locks::NONE,
        DEVICE,
        NOW,
    );
    let report = carry_out(&nb, plan, Some(&mut session), Locks::NONE, DEVICE, NOW).unwrap();
    assert_eq!(report.changed, ["linker.kame"]);
    assert_eq!(links_of(session.document()), ["b.kame"]);
    assert_eq!(session.history().0, before.0 + 1);
}

#[test]
fn a_session_on_the_moving_document_is_refused() {
    let (root, nb) = plain_notebook("move-open-itself");
    let a = root.join("a.kame");
    document(&a, "A.");
    let mut session = Session::open(&a).unwrap();
    let b = root.join("b.kame");
    let plan = plan_move(
        &nb,
        &a,
        &b,
        Some((a.as_path(), session.document())),
        Locks::NONE,
        DEVICE,
        NOW,
    );
    assert!(carry_out(&nb, plan, Some(&mut session), Locks::NONE, DEVICE, NOW).is_err());
    assert!(a.is_file());
    assert!(!b.exists());
}

#[test]
fn renaming_the_root_index_keeps_links_to_it() {
    let (root, nb) = plain_notebook("move-index");
    let index = root.join("Notebook.kame");
    document(&index, "Contents.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("./"));
    link(&linker, "two", document_link("Notebook.kame"));
    let below = root.join("sub/below.kame");
    document(&below, "");
    link(&below, "up", document_link("../"));
    assert_eq!(nb.index(), Some(index.clone()));

    let home = root.join("Home.kame");
    make_move(&nb, &index, &home);
    assert_eq!(links_of(&read(&linker)), ["Home.kame", "Home.kame"]);
    assert_eq!(links_of(&read(&below)), ["../Home.kame"]);
    assert_follows(&nb, "Home.kame", &linker, &home);
    assert_follows(&nb, "../Home.kame", &below, &home);
    assert_eq!(nb.index(), None);
}

#[test]
fn a_folder_link_stays_one_while_the_index_stays_its_folders() {
    let (root, nb) = plain_notebook("move-index-link");
    let index = root.join("old/Notebook.kame");
    document(&index, "");
    link(&index, "me", document_link("./"));
    link(&index, "here", document_link("#Part"));
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("old/"));

    let to = root.join("new/Notebook.kame");
    make_move(&nb, &index, &to);
    assert_eq!(links_of(&read(&to)), ["./", "#Part"]);
    assert_eq!(links_of(&read(&linker)), ["new/Notebook.kame"]);
}

#[test]
fn a_move_with_unsaved_typing_keeps_all_of_it() {
    let (root, nb) = plain_notebook("move-typing");
    let a = root.join("a.kame");
    {
        let mut session = Session::open(&a).unwrap();
        for word in ["Every ", "word ", "typed ", "is ", "kept."] {
            session.insert(word);
        }
    }
    let b = root.join("x/b.kame");
    make_move(&nb, &a, &b);
    assert_eq!(read(&b).blocks[0].text, "Every word typed is kept.");
}

/// As a crash would leave it: the words are in the `-wal`, and no connection folded them in.
#[test]
fn words_only_in_the_wal_move_with_the_file() {
    let (root, nb) = plain_notebook("move-wal");
    let live = folder("move-wal-live").join("live.kame");
    let a = root.join("a.kame");
    {
        let mut session = Session::open(&live).unwrap();
        session.insert("Only in the log.");
        std::fs::copy(&live, &a).unwrap();
        let wal = PathBuf::from(format!("{}-wal", live.display()));
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);
        std::fs::copy(&wal, root.join("a.kame-wal")).unwrap();
    }
    let b = root.join("x/b.kame");
    make_move(&nb, &a, &b);
    assert_eq!(read(&b).blocks[0].text, "Only in the log.");
}

#[test]
fn a_name_with_spaces_and_accents_survives() {
    let (root, nb) = plain_notebook("move-accents");
    let from = root.join("maritain/matière première.kame");
    document(&from, "");
    std::fs::write(root.join("maritain/é.png"), "p").unwrap();
    {
        let mut session = Session::open(&from).unwrap();
        assert!(session.insert_image("é.png".to_string()));
    }
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(
        &linker,
        "one",
        document_link("maritain/matière première.kame"),
    );
    link(
        &linker,
        "two",
        document_link("maritain/matière première#Été"),
    );
    let to = root.join("été/première matière.kame");
    let report = make_move(&nb, &from, &to);
    assert!(report.left.is_empty());
    assert_eq!(
        links_of(&read(&linker)),
        ["été/première matière.kame", "été/première matière.kame#Été"]
    );
    assert_follows(&nb, "été/première matière.kame#Été", &linker, &to);
    let session = Session::open(&to).unwrap();
    assert_eq!(session.document().title, "première matière");
    assert!(session.document().blocks.iter().any(|block| matches!(
        &block.kind,
        BlockKind::Image { path, .. } if path == "../maritain/é.png"
    )));
}

#[test]
fn a_failed_move_leaves_everything_as_it_was() {
    let (root, nb) = plain_notebook("move-failed");
    let a = root.join("a.kame");
    document(&a, "A.");
    std::fs::write(root.join("a.kame-wal"), "").unwrap();
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    let locked = root.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    let before = read(&linker);
    let history = Session::open(&linker).unwrap().history();
    let to = locked.join("b.kame");

    let plan = plan_move(&nb, &a, &to, None, Locks::NONE, DEVICE, NOW);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = carry_out(&nb, plan, None, Locks::NONE, DEVICE, NOW);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(result.is_err());
    assert!(a.is_file());
    assert!(root.join("a.kame-wal").is_file());
    assert!(!to.exists());
    assert_eq!(read(&linker), before);
    assert_eq!(Session::open(&linker).unwrap().history(), history);
    assert_eq!(read(&a).title, "a");
}

#[test]
fn a_file_in_the_way_stops_the_move_and_is_not_overwritten() {
    let (root, nb) = plain_notebook("move-in-the-way");
    let a = root.join("a.kame");
    document(&a, "A.");
    // Left over from a `b` that's gone. Beside a new `b.kame`, SQLite could read it as its log.
    std::fs::write(root.join("b.kame-wal"), "stale").unwrap();
    let b = root.join("b.kame");
    let plan = plan_move(&nb, &a, &b, None, Locks::NONE, DEVICE, NOW);
    let err = carry_out(&nb, plan, None, Locks::NONE, DEVICE, NOW).unwrap_err();
    assert_eq!(err, "something is already at b.kame-wal; delete it first");
    assert!(a.is_file());
    assert!(!b.exists());
    assert_eq!(std::fs::read(root.join("b.kame-wal")).unwrap(), b"stale");
}

#[test]
fn a_real_log_left_at_the_new_path_does_not_replace_the_moved_words() {
    let (root, nb) = plain_notebook("move-stale-log");
    let a = root.join("a.kame");
    document(&a, "The words of a.");
    let b = root.join("b.kame");
    let gone = folder("move-stale-log-gone").join("gone.kame");
    {
        let mut session = Session::open(&gone).unwrap();
        session.insert("Stale words from a deleted b.");
        let wal = PathBuf::from(format!("{}-wal", gone.display()));
        std::fs::copy(&wal, root.join("b.kame-wal")).unwrap();
    }
    let plan = plan_move(&nb, &a, &b, None, Locks::NONE, DEVICE, NOW);
    assert!(carry_out(&nb, plan, None, Locks::NONE, DEVICE, NOW).is_err());
    assert!(!b.exists());
    assert_eq!(read(&a).blocks[0].text, "The words of a.");
}

#[test]
fn logs_in_the_way_stop_a_move_with_none_of_its_own_to_bring() {
    let (root, nb) = plain_notebook("move-logs-in-the-way");
    let a = root.join("a.kame");
    document(&a, "A.");
    let b = root.join("b.kame");
    std::fs::create_dir_all(log_folder(&b)).unwrap();
    let plan = plan_move(&nb, &a, &b, None, Locks::NONE, DEVICE, NOW);
    let err = carry_out(&nb, plan, None, Locks::NONE, DEVICE, NOW).unwrap_err();
    assert_eq!(err, "something is already at .b.kame.sync; delete it first");
    assert!(a.is_file());
    assert!(!b.exists());
}

#[test]
fn a_document_that_is_not_there_is_an_error() {
    let (root, nb) = plain_notebook("move-nothing");
    let gone = root.join("gone.kame");
    let plan = plan_move(
        &nb,
        &gone,
        &root.join("b.kame"),
        None,
        Locks::NONE,
        DEVICE,
        NOW,
    );
    let err = carry_out(&nb, plan, None, Locks::NONE, DEVICE, NOW).unwrap_err();
    assert_eq!(err, "there's nothing at gone.kame");
    assert!(!root.join("b.kame").exists());
}

/// The revisions that hadn't arrived were written for the old folder, so their links are too.
#[test]
fn a_moved_document_behind_its_logs_has_all_its_links_and_images_fixed() {
    let (root, nb) = logged_notebook("move-own-behind");
    let own = root.join("own.kame");
    document(&own, "Words.");
    sync(&own);
    let sib = root.join("sib.kame");
    document(&sib, "Sib.");
    std::fs::write(root.join("p.png"), "p").unwrap();
    let copy = second_machine(&root, &own, "move-own-behind-there");
    write_and_send(&copy, &own, |session| {
        let mut doc = session.document().clone();
        let block = &mut doc.blocks[0];
        let start = block.text.len();
        block.text.push_str(" Link.");
        block.spans.push(Span {
            start,
            end: block.text.len(),
            style: document_link("sib.kame"),
        });
        assert!(session.replace_document(doc));
        assert!(session.insert_image("p.png".to_string()));
    });

    let to = root.join("deep/er/own.kame");
    let report = make_move(&nb, &own, &to);
    assert!(report.left.is_empty(), "{:?}", report.left);
    assert_eq!(report.changed, ["deep/er/own.kame"]);
    for doc in [read(&to), head(&to).0] {
        assert_eq!(links_of(&doc), ["../../sib.kame"]);
        assert!(doc.blocks.iter().any(|block| matches!(
            &block.kind,
            BlockKind::Image { path, .. } if path == "../../p.png"
        )));
    }
    assert_follows(&nb, "../../sib.kame", &to, &sib);
}

#[test]
fn a_folder_link_into_the_folder_it_moves_to_still_leads_there() {
    let (root, nb) = plain_notebook("move-into-linked-folder");
    let index = root.join("Notebook.kame");
    document(&index, "Contents.");
    let inner = root.join("sub/inner/Notebook.kame");
    document(&inner, "Inner.");
    let own = root.join("sub/own.kame");
    document(&own, "");
    link(&own, "up", document_link("../"));
    link(&own, "part", document_link("../#Part"));
    link(&own, "down", document_link("inner/"));

    let to = root.join("own.kame");
    let report = make_move(&nb, &own, &to);
    assert_eq!(report.changed, ["own.kame"]);
    assert_eq!(links_of(&read(&to)), ["./", "./#Part", "sub/inner/"]);
    assert_follows(&nb, "./", &to, &index);
    assert_follows(&nb, "./#Part", &to, &index);
    assert_follows(&nb, "sub/inner/", &to, &inner);
}

#[test]
fn a_stranded_document_is_refused_and_another_documents_logs_stay() {
    let (root, nb) = logged_notebook("move-stranded-itself");
    let a = root.join("a.kame");
    document(&a, "A.");
    sync(&a);
    // Another machine deleted `a` and made a new one, whose logs arrive at the same path.
    let other = root.join("other.kame");
    document(&other, "Another document.");
    sync(&other);
    std::fs::remove_dir_all(log_folder(&a)).unwrap();
    std::fs::rename(log_folder(&other), log_folder(&a)).unwrap();
    let revisions = Store::load(&log_folder(&a)).unwrap().revision_count();
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    let bytes = std::fs::read(&a).unwrap();

    let b = root.join("b.kame");
    let plan = plan_move(&nb, &a, &b, None, Locks::NONE, DEVICE, NOW);
    let err = carry_out(&nb, plan, None, Locks::NONE, DEVICE, NOW).unwrap_err();
    assert_eq!(
        err,
        "this document was moved or deleted on another machine, so it can't be moved until that's settled"
    );
    assert_eq!(std::fs::read(&a).unwrap(), bytes);
    assert!(!b.exists());
    assert!(!log_folder(&b).exists());
    assert_eq!(
        Store::load(&log_folder(&a)).unwrap().revision_count(),
        revisions
    );
    assert_eq!(links_of(&read(&linker)), ["a.kame"]);
}

/// Nothing above it turns logs on, so nothing will ever settle it, and the sync state means nothing.
#[test]
fn a_database_that_once_synced_can_move_where_logs_are_off() {
    let (root, nb) = logged_notebook("move-logs-off");
    let a = root.join("a.kame");
    document(&a, "A.");
    sync(&a);
    std::fs::remove_dir_all(log_folder(&a)).unwrap();
    std::fs::remove_file(root.join(MARKER)).unwrap();
    let b = root.join("b.kame");
    let report = make_move(&nb, &a, &b);
    assert!(report.left.is_empty(), "{:?}", report.left);
    assert!(b.is_file());
    assert!(!a.exists());
}

/// Its properties are saved on their own, where a failure shows only as a message on the session.
#[test]
fn a_title_that_could_not_be_saved_is_listed() {
    let (root, nb) = plain_notebook("move-title-unsaved");
    let a = root.join("a.kame");
    {
        let mut session = Session::open(&a).unwrap();
        let mut doc = session.document().clone();
        doc.title = "Mine".to_string();
        assert!(session.replace_document(doc));
        assert_eq!(session.metadata.title, "a");
    }
    std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o444)).unwrap();
    let b = root.join("b.kame");
    let report = make_move(&nb, &a, &b);
    assert!(b.is_file());
    assert_eq!(report.left.len(), 1, "{:?}", report.left);
    assert_eq!(report.left[0].relative, "b.kame");
    std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(Session::open(&b).unwrap().metadata.title, "a");
}

#[test]
fn a_retired_copy_in_the_way_is_kept_and_the_new_one_gets_a_number() {
    let (root, nb) = plain_notebook("move-retired-taken");
    let a = root.join("a.kame");
    document(&a, "A.");
    std::fs::copy(&a, root.join(".a.retired.kame")).unwrap();
    let from_a = std::fs::read(root.join(".a.retired.kame")).unwrap();
    std::fs::write(root.join(".b.retired.kame"), "from b").unwrap();
    make_move(&nb, &a, &root.join("b.kame"));
    assert_eq!(
        std::fs::read(root.join(".b.retired.kame")).unwrap(),
        b"from b"
    );
    assert_eq!(
        std::fs::read(root.join(".b.retired-2.kame")).unwrap(),
        from_a
    );
    assert!(!root.join(".a.retired.kame").exists());
}

#[test]
fn logs_from_a_newer_kame_leave_a_document_listed_and_unchanged() {
    let (root, nb) = logged_notebook("move-newer-logs");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    sync(&linker);
    std::fs::write(
        log_folder(&linker).join("new-0000.log"),
        "{\"id\":\"x\",\"revision\":{\"parents\":[],\"device\":\"new-0000\",\"when\":1,\"document\":\"00000000-0000-0000-0000-000000000001\",\"title\":\"t\",\"order\":[],\"annotations\":[],\"metadata\":{},\"future\":1},\"blocks\":[]}\n",
    )
    .unwrap();

    let report = make_move(&nb, &a, &root.join("b.kame"));
    assert!(report.changed.is_empty());
    assert_eq!(report.left.len(), 1);
    assert!(report.left[0].why.contains("newer version"));
    assert_eq!(links_of(&read(&linker)), ["a.kame"]);
}

#[test]
fn a_stranded_document_is_listed_and_no_logs_come_back_for_it() {
    let (root, nb) = logged_notebook("move-stranded");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    sync(&linker);
    std::fs::remove_dir_all(log_folder(&linker)).unwrap();

    let report = make_move(&nb, &a, &root.join("b.kame"));
    assert!(report.changed.is_empty());
    assert_eq!(report.left[0].relative, "linker.kame");
    assert!(report.left[0].why.contains("moved or deleted"));
    assert!(!log_folder(&linker).exists());
    assert_eq!(links_of(&read(&linker)), ["a.kame"]);
}

fn place_in(path: &Path) -> Option<String> {
    Db::open_read_only(path).unwrap().load_sync_place()
}

/// What a kame from before places were recorded leaves, with its logs since gone: a sync state, no
/// place and nothing beside it.
fn as_an_older_kame_left_it_with_no_logs(path: &Path) {
    sync(path);
    let state = Db::open_read_only(path).unwrap().load_sync_state().unwrap();
    {
        let mut db = Db::open(path).unwrap();
        db.clear_sync_state().unwrap();
        db.save_sync_state(&state).unwrap();
    }
    std::fs::remove_dir_all(log_folder(path)).unwrap();
    assert_eq!(place_in(path), None);
}

/// Last synced before kame recorded where, and its logs gone: it may have been moved or deleted on
/// another machine, so its history mustn't be written anywhere. Its link to a moved document is
/// changed all the same, here only.
#[test]
fn a_document_with_no_place_and_no_logs_is_changed_and_no_logs_come_back_for_it() {
    let (root, nb) = logged_notebook("move-no-place");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    as_an_older_kame_left_it_with_no_logs(&linker);

    let report = make_move(&nb, &a, &root.join("b.kame"));
    assert!(report.left.is_empty(), "{:?}", report.left);
    assert_eq!(report.changed, ["linker.kame"]);
    assert_eq!(links_of(&read(&linker)), ["b.kame"]);
    assert!(!log_folder(&linker).exists());
    assert_eq!(place_in(&linker), None);
}

/// Moved itself, its own links, images and title follow, and still nothing reaches a log.
#[test]
fn a_moved_document_with_no_place_and_no_logs_has_its_links_images_and_title_fixed() {
    let (root, nb) = logged_notebook("move-no-place-itself");
    let own = root.join("essays/a.kame");
    document(&own, "");
    document(&root.join("notes/b.kame"), "B.");
    std::fs::create_dir_all(root.join("essays/img")).unwrap();
    std::fs::write(root.join("essays/img/p.png"), "p").unwrap();
    link(&own, "one", document_link("../notes/b.kame"));
    {
        let mut session = Session::open(&own).unwrap();
        assert!(session.insert_image("img/p.png".to_string()));
    }
    as_an_older_kame_left_it_with_no_logs(&own);
    assert_eq!(Session::open(&own).unwrap().metadata.title, "a");

    let to = root.join("drafts/deep/c.kame");
    let report = make_move(&nb, &own, &to);
    assert!(report.left.is_empty(), "{:?}", report.left);
    assert_eq!(report.changed, ["drafts/deep/c.kame"]);
    let session = Session::open(&to).unwrap();
    let doc = session.document();
    assert_eq!(links_of(doc), ["../../notes/b.kame"]);
    let stored = doc
        .blocks
        .iter()
        .find_map(|block| match &block.kind {
            BlockKind::Image { path, .. } => Some(path.clone()),
            _ => None,
        })
        .unwrap();
    assert_ne!(stored, "img/p.png");
    assert_eq!(std::fs::read(image_file(&stored, Some(&to))).unwrap(), b"p");
    assert_eq!(session.metadata.title, "c");
    assert_eq!(doc.title, "c");
    drop(session);
    for path in [&own, &to] {
        assert!(!log_folder(path).exists(), "{}", path.display());
    }
    assert_eq!(place_in(&to), None);
}

#[test]
fn a_rename_with_nothing_else_to_change_records_the_new_place() {
    let (root, nb) = logged_notebook("move-records-place");
    let a = root.join("a.kame");
    document(&a, "Words.");
    sync(&a);
    assert_eq!(place_in(&a), place_of(&a));
    let revisions = Store::load(&log_folder(&a)).unwrap().revision_count();

    let b = root.join("deep/b.kame");
    let report = make_move(&nb, &a, &b);
    assert!(report.left.is_empty(), "{:?}", report.left);
    assert_eq!(place_in(&b), place_of(&b));
    assert!(Store::load(&log_folder(&b)).unwrap().revision_count() >= revisions);

    // It's recognised if the logs now leave it.
    std::fs::remove_dir_all(log_folder(&b)).unwrap();
    let plan = plan_move(
        &nb,
        &b,
        &root.join("c.kame"),
        None,
        Locks::NONE,
        DEVICE,
        NOW,
    );
    assert!(carry_out(&nb, plan, None, Locks::NONE, DEVICE, NOW).is_err());
    assert!(b.is_file());
}

/// A database with a sync state from somewhere else is a document like any other here.
#[test]
fn a_hand_copy_is_changed_moved_and_deleted_like_any_document() {
    let (root, nb) = logged_notebook("move-hand-copy");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    sync(&linker);
    let copy = root.join("deep/copy.kame");
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(&linker, &copy).unwrap();
    let original = std::fs::read(&linker).unwrap();

    // Its link is relative, so from its own folder it leads here.
    let target = root.join("deep/a.kame");
    document(&target, "A second a.");
    let report = make_move(&nb, &target, &root.join("deep/b.kame"));
    assert_eq!(report.changed, ["deep/copy.kame"]);
    assert!(report.left.is_empty(), "{:?}", report.left);
    assert_eq!(links_of(&head(&copy).0), ["b.kame"]);
    assert_eq!(std::fs::read(&linker).unwrap(), original);

    let moved = root.join("moved.kame");
    let report = make_move(&nb, &copy, &moved);
    assert!(report.left.is_empty(), "{:?}", report.left);
    assert!(moved.is_file());
    assert!(log_folder(&moved).is_dir());
    assert_eq!(place_in(&moved), place_of(&moved));

    assert!(!delete_document(&moved, Locks::NONE).unwrap());
    assert!(!moved.exists());
    assert!(!log_folder(&moved).exists());
}

/// A database from elsewhere, on logs that were never its, with those logs another document's.
fn on_another_documents_logs(root: &Path) -> (PathBuf, Vec<(std::ffi::OsString, Vec<u8>)>) {
    let (one, other) = (root.join("one.kame"), root.join("other.kame"));
    document(&one, "One.");
    sync(&one);
    document(&other, "The other.");
    sync(&other);
    std::fs::remove_file(&other).unwrap();
    std::fs::rename(&one, &other).unwrap();
    std::fs::remove_dir_all(log_folder(&one)).unwrap();
    let logs = files_in(&log_folder(&other));
    (other, logs)
}

#[test]
fn deleting_a_database_on_another_documents_logs_leaves_them() {
    let (root, _nb) = logged_notebook("delete-on-others-logs");
    let (path, logs) = on_another_documents_logs(&root);
    assert!(
        delete_document(&path, Locks::NONE).unwrap(),
        "it says it left the logs"
    );
    assert!(!path.exists());
    assert_eq!(files_in(&log_folder(&path)), logs);
}

#[test]
fn moving_a_database_on_another_documents_logs_is_refused_and_they_stay() {
    let (root, nb) = logged_notebook("move-on-others-logs");
    let (path, logs) = on_another_documents_logs(&root);
    let to = root.join("to.kame");
    let plan = plan_move(&nb, &path, &to, None, Locks::NONE, DEVICE, NOW);
    let err = carry_out(&nb, plan, None, Locks::NONE, DEVICE, NOW).unwrap_err();
    assert!(err.contains("another document's"), "{err}");
    assert!(path.is_file());
    assert!(!to.exists());
    assert_eq!(files_in(&log_folder(&path)), logs);
}

#[test]
fn a_database_on_another_documents_logs_is_listed_and_they_stay() {
    let (root, nb) = logged_notebook("rewrite-on-others-logs");
    let (path, logs) = on_another_documents_logs(&root);
    link(&path, "one", document_link("a.kame"));
    let a = root.join("a.kame");
    document(&a, "A.");

    let report = make_move(&nb, &a, &root.join("b.kame"));
    assert!(report.changed.is_empty());
    assert_eq!(report.left[0].relative, "other.kame");
    assert!(report.left[0].why.contains("another document's"));
    assert_eq!(files_in(&log_folder(&path)), logs);
}

/// Puts a copy of the document at `of`, with `words` put at its start, at `at`, with the words
/// only in its `-wal`, as a crash would leave a database. `of` may be a database of another
/// document's, kept elsewhere.
fn copy_with_words_in_its_wal(of: &Path, at: &Path, words: &str) {
    let live = kame_test_dir::folder("wal-copy").join("live.kame");
    std::fs::copy(of, &live).unwrap();
    let mut session = Session::open(&live).unwrap();
    session.insert(words);
    let wal = PathBuf::from(format!("{}-wal", live.display()));
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);
    std::fs::copy(&live, at).unwrap();
    std::fs::copy(&wal, format!("{}-wal", at.display())).unwrap();
}

/// The words a database holds, as read with what's in its `-wal`.
fn words_in(path: &Path) -> String {
    Db::open_read_only(path)
        .unwrap()
        .load_document()
        .unwrap()
        .blocks[0]
        .text
        .clone()
}

#[test]
fn deleting_takes_what_sqlite_keeps_beside_a_retired_copy_and_leaves_a_neighbours() {
    let root = folder("delete-retired-wal");
    let x = root.join("X.kame");
    document(&x, "X.");
    copy_with_words_in_its_wal(&x, &root.join(".X.retired.kame"), "Never synced. ");
    std::fs::write(root.join(".X.retired.kame-shm"), "").unwrap();
    let elsewhere = folder("delete-retired-wal-other").join("other.kame");
    document(&elsewhere, "Another document.");
    copy_with_words_in_its_wal(&elsewhere, &root.join(".X.retired-2.kame"), "Its own. ");
    let xy = root.join("XY.kame");
    document(&xy, "XY.");
    copy_with_words_in_its_wal(&xy, &root.join(".XY.retired.kame"), "Neighbour's. ");

    delete_document(&x, Locks::NONE).unwrap();

    let mut left: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            ".X.retired-2.kame",
            ".X.retired-2.kame-wal",
            ".XY.retired.kame",
            ".XY.retired.kame-wal",
            "XY.kame"
        ]
    );
    assert_eq!(
        words_in(&root.join(".X.retired-2.kame")),
        "Its own. Another document."
    );
    assert_eq!(words_in(&root.join(".XY.retired.kame")), "Neighbour's. XY.");
}

#[test]
fn a_retired_copy_moves_with_what_sqlite_keeps_beside_it() {
    let (root, nb) = plain_notebook("move-retired-wal");
    let a = root.join("a.kame");
    document(&a, "A.");
    copy_with_words_in_its_wal(&a, &root.join(".a.retired.kame"), "Never synced. ");
    copy_with_words_in_its_wal(&a, &root.join(".a.retired-2.kame"), "Nor these. ");
    // The first free name for the second is taken, so it gets the next, and its -wal has to
    // follow it there.
    let elsewhere = folder("move-retired-wal-other").join("other.kame");
    document(&elsewhere, "Another document.");
    copy_with_words_in_its_wal(&elsewhere, &root.join(".b.retired.kame"), "Its own. ");
    let xy = root.join("ab.kame");
    document(&xy, "AB.");
    copy_with_words_in_its_wal(&xy, &root.join(".ab.retired.kame"), "Neighbour's. ");

    make_move(&nb, &a, &root.join("b.kame"));

    assert_eq!(
        words_in(&root.join(".b.retired.kame")),
        "Its own. Another document."
    );
    let moved = [".b.retired-2.kame", ".b.retired-3.kame"].map(|name| words_in(&root.join(name)));
    assert!(moved.contains(&"Never synced. A.".to_string()), "{moved:?}");
    assert!(moved.contains(&"Nor these. A.".to_string()), "{moved:?}");
    for name in [".b.retired-2.kame-wal", ".b.retired-3.kame-wal"] {
        assert!(root.join(name).is_file(), "{name}");
    }
    for gone in [
        ".a.retired.kame",
        ".a.retired.kame-wal",
        ".a.retired-2.kame-wal",
    ] {
        assert!(!root.join(gone).exists(), "{gone}");
    }
    assert_eq!(words_in(&root.join(".ab.retired.kame")), "Neighbour's. AB.");
    assert!(root.join(".ab.retired.kame-wal").is_file());
}

#[test]
fn a_side_file_left_at_a_retired_name_is_not_taken_for_a_moving_copys() {
    let (root, nb) = plain_notebook("move-retired-wal-in-way");
    let a = root.join("a.kame");
    document(&a, "A.");
    copy_with_words_in_its_wal(&a, &root.join(".a.retired.kame"), "Never synced. ");
    std::fs::write(root.join(".b.retired.kame-wal"), "left by something else").unwrap();

    make_move(&nb, &a, &root.join("b.kame"));

    assert_eq!(
        std::fs::read(root.join(".b.retired.kame-wal")).unwrap(),
        b"left by something else"
    );
    assert!(!root.join(".b.retired.kame").exists());
    assert_eq!(
        words_in(&root.join(".b.retired-2.kame")),
        "Never synced. A."
    );
}

/// A database a crash left in the name a clean-up moves one through, as it's then treated as
/// retired, so it's the document's as a retired copy is.
#[test]
fn deleting_takes_a_copy_left_in_a_moving_name_and_leaves_a_neighbours() {
    let root = folder("delete-moving");
    let x = root.join("X.kame");
    document(&x, "X.");
    copy_with_words_in_its_wal(&x, &root.join(".X.moving-1.kame"), "Never synced. ");
    let elsewhere = folder("delete-moving-other").join("other.kame");
    document(&elsewhere, "Another document.");
    copy_with_words_in_its_wal(&elsewhere, &root.join(".X.moving-2.kame"), "Its own. ");
    let xy = root.join("XY.kame");
    document(&xy, "XY.");
    copy_with_words_in_its_wal(&xy, &root.join(".XY.moving-1.kame"), "Neighbour's. ");
    for name in [".X.moving-.kame", ".X.moving-a.kame", ".X.movingly.kame"] {
        std::fs::copy(&x, root.join(name)).unwrap();
    }

    delete_document(&x, Locks::NONE).unwrap();

    let mut left: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            ".X.moving-.kame",
            ".X.moving-2.kame",
            ".X.moving-2.kame-wal",
            ".X.moving-a.kame",
            ".X.movingly.kame",
            ".XY.moving-1.kame",
            ".XY.moving-1.kame-wal",
            "XY.kame"
        ]
    );
    assert_eq!(
        words_in(&root.join(".X.moving-2.kame")),
        "Its own. Another document."
    );
    assert_eq!(
        words_in(&root.join(".XY.moving-1.kame")),
        "Neighbour's. XY."
    );
}

#[test]
fn a_copy_left_in_a_moving_name_moves_with_its_document() {
    let (root, nb) = plain_notebook("move-moving");
    let a = root.join("a.kame");
    document(&a, "A.");
    copy_with_words_in_its_wal(&a, &root.join(".a.moving-1.kame"), "Never synced. ");
    let elsewhere = folder("move-moving-other").join("other.kame");
    document(&elsewhere, "Another document.");
    copy_with_words_in_its_wal(&elsewhere, &root.join(".b.moving-1.kame"), "Its own. ");
    let ab = root.join("ab.kame");
    document(&ab, "AB.");
    copy_with_words_in_its_wal(&ab, &root.join(".ab.moving-1.kame"), "Neighbour's. ");

    make_move(&nb, &a, &root.join("b.kame"));

    assert_eq!(
        words_in(&root.join(".b.moving-1.kame")),
        "Its own. Another document."
    );
    assert_eq!(words_in(&root.join(".b.moving-2.kame")), "Never synced. A.");
    assert!(root.join(".b.moving-2.kame-wal").is_file());
    assert!(!root.join(".a.moving-1.kame").exists());
    assert!(!root.join(".a.moving-1.kame-wal").exists());
    assert_eq!(
        words_in(&root.join(".ab.moving-1.kame")),
        "Neighbour's. AB."
    );
}

#[test]
fn another_notebook_inside_is_outside_for_a_move() {
    let (root, nb) = plain_notebook("move-inner-notebook");
    let a = root.join("a.kame");
    document(&a, "A.");
    let mine = root.join("mine.kame");
    document(&mine, "");
    link(&mine, "mine", document_link("a"));
    let theirs = root.join("inner/theirs.kame");
    document(&theirs, "");
    link(&theirs, "theirs", document_link("../a"));
    std::fs::write(root.join("inner/notebook.yaml"), "").unwrap();

    let refused = check_new_path(&nb, &a, "inner/a").unwrap_err();
    assert!(
        refused.contains("inner/") && refused.contains("another notebook"),
        "{refused}"
    );
    let to = root.join("z.kame");
    let report = make_move(&nb, &a, &to);
    assert_eq!(report.changed, ["mine.kame"]);
    assert_eq!(links_of(&read(&mine)), ["z.kame"]);
    assert_eq!(
        links_of(&read(&theirs)),
        ["../a"],
        "the other notebook's links are its own"
    );
}

#[test]
fn a_document_beside_another_notebook_of_its_name_has_its_links_moved() {
    let (root, nb) = plain_notebook("move-beside-inner-notebook");
    let projects = root.join("projects.kame");
    document(&projects, "P.");
    document(&root.join("projects/theirs.kame"), "");
    std::fs::write(root.join("projects/notebook.yaml"), "").unwrap();
    let mine = root.join("mine.kame");
    document(&mine, "");
    link(&mine, "mine", document_link("projects"));

    let report = make_move(&nb, &projects, &root.join("work.kame"));
    assert_eq!(report.changed, ["mine.kame"]);
    assert_eq!(links_of(&read(&mine)), ["work.kame"]);
}

// Another kame holding a document. What it has open is never written into, renamed or removed.

/// A folder for one test's locks, as the app keeps them in its own.
fn locks_folder(name: &str) -> PathBuf {
    kame_test_dir::folder(&format!("{name}-locks"))
}

fn no_locks_left(folder: &Path) -> bool {
    std::fs::read_dir(folder).map_or(true, |mut entries| entries.next().is_none())
}

/// The name of everything in a folder and below it, and what's in each file.
fn everything_in(folder: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(folder).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.push((path.clone(), Vec::new()));
            found.extend(everything_in(&path));
        } else {
            found.push((path.clone(), std::fs::read(&path).unwrap()));
        }
    }
    found.sort();
    found
}

#[test]
fn a_move_of_a_document_another_kame_has_open_is_refused_with_nothing_changed() {
    let (root, nb) = plain_notebook("move-held");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    let folder = locks_folder("move-held");
    let locks = Locks::new(Some(&folder), None);
    let before = everything_in(&root);

    let _theirs = DocumentLock::take(&folder, &a).unwrap();
    let plan = plan_move(&nb, &a, &root.join("b.kame"), None, locks, DEVICE, NOW);
    let err = carry_out(&nb, plan, None, locks, DEVICE, NOW).unwrap_err();
    assert_eq!(err, "a.kame is open in another kame window");
    assert_eq!(everything_in(&root), before);
}

/// It's behind its logs, which a move would otherwise bring in before rewriting it.
#[test]
fn a_move_is_refused_when_another_kame_has_a_document_linking_to_it_open() {
    let (root, nb) = logged_notebook("move-held-linker");
    let a = root.join("a.kame");
    document(&a, "A.");
    sync(&a);
    let linker = root.join("deep/linker.kame");
    document(&linker, "Words.");
    sync(&linker);
    let copy = second_machine(&root, &linker, "move-held-linker-there");
    write_and_send(&copy, &linker, |session| {
        let mut doc = session.document().clone();
        let block = &mut doc.blocks[0];
        let start = block.text.len();
        block.text.push_str(" Link.");
        block.spans.push(Span {
            start,
            end: block.text.len(),
            style: document_link("../a.kame"),
        });
        assert!(session.replace_document(doc));
    });
    let folder = locks_folder("move-held-linker");
    let locks = Locks::new(Some(&folder), None);
    let history = Session::open(&linker).unwrap().history();
    let before = everything_in(&root);

    let theirs = DocumentLock::take(&folder, &linker).unwrap();
    let plan = plan_move(&nb, &a, &root.join("b.kame"), None, locks, DEVICE, NOW);
    let err = carry_out(&nb, plan, None, locks, DEVICE, NOW).unwrap_err();
    assert_eq!(err, "deep/linker.kame is open in another kame window");
    assert_eq!(everything_in(&root), before);
    drop(theirs);
    assert_eq!(Session::open(&linker).unwrap().history(), history);
    assert!(links_of(&read(&linker)).is_empty());
    assert!(no_locks_left(&folder), "the move let go of what it took");
}

/// As the app holds the document it has open, and keeps holding the one it's moving.
#[test]
fn a_move_goes_ahead_with_the_callers_own_document_and_lets_go_of_the_rest() {
    let (root, nb) = plain_notebook("move-own");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    let folder = locks_folder("move-own");
    let own = DocumentLock::take(&folder, &a).unwrap();
    let locks = Locks::new(Some(&folder), Some(&own));
    let b = root.join("b.kame");

    let plan = plan_move(&nb, &a, &b, None, locks, DEVICE, NOW);
    let report = carry_out(&nb, plan, None, locks, DEVICE, NOW).unwrap();
    assert_eq!(report.changed, ["linker.kame"]);
    assert_eq!(links_of(&read(&linker)), ["b.kame"]);
    assert_eq!(read(&b).title, "b");
    assert!(own.is_for(&b));
    assert!(matches!(
        DocumentLock::take(&folder, &b),
        Err(LockError::Held)
    ));
    DocumentLock::take(&folder, &linker).unwrap();
    drop(own);
    assert!(no_locks_left(&folder));
}

#[test]
fn deleting_a_document_another_kame_has_open_is_refused_with_nothing_removed() {
    let (root, _nb) = logged_notebook("delete-held");
    let x = root.join("X.kame");
    document(&x, "X.");
    sync(&x);
    std::fs::copy(&x, root.join(".X.retired.kame")).unwrap();
    let folder = locks_folder("delete-held");
    let locks = Locks::new(Some(&folder), None);
    let before = everything_in(&root);

    let theirs = DocumentLock::take(&folder, &x).unwrap();
    let err = delete_document(&x, locks).unwrap_err();
    assert!(
        matches!(&err, DeleteError::HeldElsewhere(held) if *held == x),
        "{err:?}"
    );
    assert_eq!(err.to_string(), "X.kame is open in another kame window");
    assert_eq!(everything_in(&root), before);
    drop(theirs);

    delete_document(&x, locks).unwrap();
    assert!(!x.exists());
    assert!(no_locks_left(&folder));
}

const CONFLICT: &str = "X.sync-conflict-20261103-011500-ABCDEFG.kame";

/// A document with a retired copy and a conflicting copy beside it, either of which File > Open Path
/// opens.
fn with_copies(root: &Path) -> (PathBuf, [PathBuf; 2]) {
    let x = root.join("X.kame");
    document(&x, "X.");
    let copies = [root.join(".X.retired.kame"), root.join(CONFLICT)];
    for copy in &copies {
        std::fs::copy(&x, copy).unwrap();
    }
    (x, copies)
}

#[test]
fn deleting_is_refused_while_another_kame_has_a_copy_of_the_document_open() {
    let (root, _nb) = plain_notebook("delete-held-copy");
    let (x, copies) = with_copies(&root);
    let folder = locks_folder("delete-held-copy");
    let locks = Locks::new(Some(&folder), None);
    let before = everything_in(&root);

    for copy in &copies {
        let theirs = DocumentLock::take(&folder, copy).unwrap();
        let err = delete_document(&x, locks).unwrap_err();
        assert!(
            matches!(&err, DeleteError::HeldElsewhere(held) if held == copy),
            "{err:?}"
        );
        assert_eq!(everything_in(&root), before);
        drop(theirs);
    }
    assert!(no_locks_left(&folder));
}

/// It would be removed from under the window's own session.
#[test]
fn deleting_is_refused_while_the_caller_has_a_copy_of_the_document_open() {
    let (root, _nb) = plain_notebook("delete-own-copy");
    let (x, [retired, _]) = with_copies(&root);
    let folder = locks_folder("delete-own-copy");
    let own = DocumentLock::take(&folder, &retired).unwrap();
    let before = everything_in(&root);

    let err = delete_document(&x, Locks::new(Some(&folder), Some(&own))).unwrap_err();
    assert!(
        matches!(&err, DeleteError::OpenHere(held) if *held == retired),
        "{err:?}"
    );
    assert_eq!(err.to_string(), ".X.retired.kame is open in this window");
    assert_eq!(everything_in(&root), before);
}

#[test]
fn a_move_is_refused_while_another_kame_has_a_copy_of_the_document_open() {
    let (root, nb) = plain_notebook("move-held-copy");
    let (x, copies) = with_copies(&root);
    let folder = locks_folder("move-held-copy");
    let locks = Locks::new(Some(&folder), None);
    let before = everything_in(&root);

    for (copy, name) in copies.iter().zip([".X.retired.kame", CONFLICT]) {
        let theirs = DocumentLock::take(&folder, copy).unwrap();
        let plan = plan_move(&nb, &x, &root.join("Y.kame"), None, locks, DEVICE, NOW);
        let err = carry_out(&nb, plan, None, locks, DEVICE, NOW).unwrap_err();
        assert_eq!(err, format!("{name} is open in another kame window"));
        assert_eq!(everything_in(&root), before);
        drop(theirs);
    }
    assert!(no_locks_left(&folder));
}

#[test]
fn a_move_is_refused_while_the_caller_has_a_copy_of_the_document_open() {
    let (root, nb) = plain_notebook("move-own-copy");
    let (x, [retired, _]) = with_copies(&root);
    let folder = locks_folder("move-own-copy");
    let own = DocumentLock::take(&folder, &retired).unwrap();
    let locks = Locks::new(Some(&folder), Some(&own));
    let before = everything_in(&root);

    let plan = plan_move(&nb, &x, &root.join("Y.kame"), None, locks, DEVICE, NOW);
    let err = carry_out(&nb, plan, None, locks, DEVICE, NOW).unwrap_err();
    assert_eq!(err, ".X.retired.kame is open in this window");
    assert_eq!(everything_in(&root), before);
}

/// Adds a block after the rest, as a build knowing tables would have written one, and returns its
/// words.
fn add_a_newer_block(path: &Path) -> &'static str {
    let words = "A table of otters.";
    {
        let mut session = Session::open(path).unwrap();
        let mut doc = session.document().clone();
        doc.blocks.push(kame_core::model::Block {
            id: uuid::Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: words.to_string(),
            spans: Vec::new(),
        });
        assert!(session.replace_document(doc));
    }
    make_a_table_of(path, words);
    words
}

/// The block holding `words`, as a build knowing tables would have written it.
fn make_a_table_of(path: &Path, words: &str) {
    let connection = rusqlite::Connection::open(path).unwrap();
    let head: String = connection
        .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let paragraph = format!(r#""kind":"Paragraph","text":"{words}"#);
    let table = format!(r#""kind":"Table","text":"{words}"#);
    let newer = head.replacen(&paragraph, &table, 1);
    assert_ne!(newer, head);
    connection
        .execute("UPDATE documents SET json = ?1 WHERE id = 1", [&newer])
        .unwrap();
}

/// What a move says of a document from a newer kame that it can't change.
const NEWER_LEFT: &str = "it was written by a newer version of kame; update kame to change it";

/// A document from a newer kame whose links to the moving document would change is left as it was,
/// and named with why, whether it holds a kind or a field only a newer kame knows.
#[test]
fn a_document_from_a_newer_kame_whose_links_would_change_is_named_and_untouched() {
    let (root, nb) = plain_notebook("move-newer-kind");
    let a = root.join("a.kame");
    document(&a, "A.");
    let kinds = root.join("kinds.kame");
    document(&kinds, "");
    link(&kinds, "one", document_link("a.kame"));
    add_a_newer_block(&kinds);
    let fields = root.join("fields.kame");
    document(&fields, "");
    link(&fields, "one", document_link("a.kame"));
    make_newer(&fields);
    let bytes = [&kinds, &fields].map(|path| std::fs::read(path).unwrap());

    let report = make_move(&nb, &a, &root.join("b.kame"));
    assert!(report.changed.is_empty());
    let mut left: Vec<(&str, &str)> = report
        .left
        .iter()
        .map(|left| (left.relative.as_str(), left.why.as_str()))
        .collect();
    left.sort();
    assert_eq!(
        left,
        [("fields.kame", NEWER_LEFT), ("kinds.kame", NEWER_LEFT)]
    );
    assert_eq!(
        [&kinds, &fields].map(|path| std::fs::read(path).unwrap()),
        bytes
    );
}

/// What a build can't read of a document from a newer kame may link to the moving document, so the
/// document is named even when nothing it can read does.
#[test]
fn a_document_from_a_newer_kame_that_may_link_here_is_named() {
    let (root, nb) = plain_notebook("move-newer-hidden");
    let a = root.join("a.kame");
    document(&a, "A.");
    let hidden = root.join("hidden.kame");
    document(&hidden, "");
    link(&hidden, "one", document_link("a.kame"));
    make_a_table_of(&hidden, "one");
    let elsewhere = root.join("elsewhere.kame");
    document(&elsewhere, "Nothing newer and no links.");
    let bytes = std::fs::read(&hidden).unwrap();

    let report = make_move(&nb, &a, &root.join("b.kame"));
    assert_eq!(report.left.len(), 1, "{:?}", report.left);
    assert_eq!(report.left[0].relative, "hidden.kame");
    assert!(
        report.left[0].why.contains("newer version of kame")
            && report.left[0].why.contains("update kame"),
        "{}",
        report.left[0].why
    );
    assert_eq!(std::fs::read(&hidden).unwrap(), bytes);
}

/// A logged document from a newer kame is its logs' own as much as any other, so it moves with
/// them, and is named for its own links.
#[test]
fn a_logged_document_with_a_kind_from_a_newer_kame_moves_with_its_logs() {
    let (root, nb) = logged_notebook("move-newer-logged");
    let a = root.join("a.kame");
    document(&a, "A.");
    sync(&a);
    add_a_newer_block(&a);
    let bytes = std::fs::read(&a).unwrap();
    let b = root.join("b.kame");
    let report = make_move(&nb, &a, &b);
    assert!(!a.exists());
    assert!(!log_folder(&a).exists());
    assert!(log_folder(&b).is_dir());
    assert_eq!(std::fs::read(&b).unwrap(), bytes);
    assert_eq!(report.left.len(), 1, "{:?}", report.left);
    assert_eq!(report.left[0].relative, "b.kame");
    assert_eq!(report.left[0].why, NEWER_LEFT);
}

/// A retired copy is told to be a document's by its id, and the id of one from a newer kame is read
/// as far as this build can, so the copy goes with it.
#[test]
fn a_retired_copy_moves_with_a_document_from_a_newer_kame() {
    let (root, nb) = plain_notebook("move-newer-retired");
    let a = root.join("a.kame");
    document(&a, "A.");
    std::fs::copy(&a, root.join(".a.retired.kame")).unwrap();
    let copy = std::fs::read(root.join(".a.retired.kame")).unwrap();
    add_a_newer_block(&a);
    make_move(&nb, &a, &root.join("b.kame"));
    assert!(!root.join(".a.retired.kame").exists());
    assert_eq!(std::fs::read(root.join(".b.retired.kame")).unwrap(), copy);
}

/// The open document, from a newer kame, whose links would change is left with the same reason as
/// a closed one.
#[test]
fn an_open_document_from_a_newer_kame_is_named_as_a_closed_one_is() {
    let (root, nb) = plain_notebook("move-open-newer");
    let a = root.join("a.kame");
    document(&a, "A.");
    let linker = root.join("linker.kame");
    document(&linker, "");
    link(&linker, "one", document_link("a.kame"));
    make_newer(&linker);
    let bytes = std::fs::read(&linker).unwrap();
    let mut session = Session::open(&linker).unwrap();
    assert!(session.newer);
    let plan = plan_move(
        &nb,
        &a,
        &root.join("b.kame"),
        Some((linker.as_path(), session.document())),
        Locks::NONE,
        DEVICE,
        NOW,
    );
    let report = carry_out(&nb, plan, Some(&mut session), Locks::NONE, DEVICE, NOW).unwrap();
    let left: Vec<(&str, &str)> = report
        .left
        .iter()
        .map(|left| (left.relative.as_str(), left.why.as_str()))
        .collect();
    assert_eq!(left, [("linker.kame", NEWER_LEFT)]);
    drop(session);
    assert_eq!(std::fs::read(&linker).unwrap(), bytes);
}
