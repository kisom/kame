//! Checking a document's new path, and deleting one.

use std::path::{Path, PathBuf};

use kame_core::session::Session;
use kame_notebook::manage::{check_new_path, delete_document};
use kame_notebook::Notebook;

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
        "X.kame-wal",
        "X.kame-shm",
        "X.sync-conflict-1.kame.merged",
        ".X.retired.kame",
        ".X.retired-2.kame",
    ] {
        std::fs::write(root.join(name), "").unwrap();
    }
    std::fs::copy(&x, root.join("X.sync-conflict-1.kame")).unwrap();
    std::fs::create_dir_all(root.join(".X.kame.sync")).unwrap();
    std::fs::write(root.join(".X.kame.sync/titan.log"), "").unwrap();
    document(&root.join("XY.kame"), "XY.");
    std::fs::write(root.join(".XY.retired.kame"), "").unwrap();
    delete_document(&x).unwrap();
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
    delete_document(&root.join("n.md")).unwrap();
    assert!(!root.join("n.md").exists());
    assert!(root.join("n.sync-conflict-1.kame").exists());
}
