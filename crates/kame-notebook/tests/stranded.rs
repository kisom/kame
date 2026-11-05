//! Databases whose logs have left them. The other machine is played by what it does to the log
//! folders here, as the sync tool would carry it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use kame_core::session::Session;
use kame_notebook::stranded::{apply, decide, find_stranded, reconcile, Done};
use kame_notebook::Notebook;
use kame_sync::logs::{build_from_logs, log_folder, Logged, MARKER};
use kame_sync::revlog::Store;

const DEVICE: &str = "titan-bbbb";

fn logged_notebook(name: &str) -> (PathBuf, Notebook) {
    let root = kame_test_dir::folder(name).canonicalize().unwrap();
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    std::fs::write(root.join(MARKER), "").unwrap();
    (root.clone(), Notebook::open(&root).unwrap())
}

fn sync(session: &mut Session) -> kame_sync::logs::Outcome {
    Logged::open(session, DEVICE)
        .expect("the folder is opted in")
        .sync(session, 100)
        .unwrap()
}

/// A document that has synced, so that its database remembers a revision in its logs.
fn synced(path: &Path, words: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut session = Session::open(path).unwrap();
    session.insert(words);
    sync(&mut session);
}

fn text_of(path: &Path) -> String {
    let session = Session::open(path).unwrap();
    session
        .document()
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the logs in a folder have ever held, as words.
fn words_in_logs(folder: &Path) -> String {
    let mut all = String::new();
    for entry in std::fs::read_dir(folder).unwrap().flatten() {
        all.push_str(&String::from_utf8_lossy(
            &std::fs::read(entry.path()).unwrap(),
        ));
    }
    all
}

fn move_logs(from: &Path, to: &Path) {
    std::fs::rename(log_folder(from), log_folder(to)).unwrap();
}

fn remove_logs(path: &Path) {
    std::fs::remove_dir_all(log_folder(path)).unwrap();
}

fn copy_logs(from: &Path, to: &Path) {
    let (from, to) = (log_folder(from), log_folder(to));
    std::fs::create_dir_all(&to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

/// A log holding a line from a kame newer than this one.
fn add_newer_line(path: &Path) {
    let folder = log_folder(path);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("new-0000.log"),
        "{\"id\":\"x\",\"revision\":{\"parents\":[],\"device\":\"new-0000\",\"when\":1,\"document\":\"00000000-0000-0000-0000-000000000001\",\"title\":\"t\",\"order\":[],\"annotations\":[],\"metadata\":{},\"future\":1},\"blocks\":[]}\n",
    )
    .unwrap();
}

/// Everything in a folder, hidden or not, in order.
fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn listed(notebook: &Notebook) -> Vec<String> {
    notebook
        .documents()
        .into_iter()
        .map(|entry| entry.relative)
        .collect()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// What a crash leaves: words in the `-wal` that the file itself doesn't have yet. The files are
/// copied while the session has them open, and put back after closing it has folded them together.
fn crash_with_words_in_the_wal(path: &Path, words: &str) {
    let held = with_suffix(path, ".held");
    std::fs::create_dir_all(&held).unwrap();
    {
        let mut session = Session::open(path).unwrap();
        session.insert(words);
        for suffix in ["", "-wal", "-shm"] {
            std::fs::copy(
                with_suffix(path, suffix),
                held.join(format!("copy{suffix}")),
            )
            .unwrap();
        }
    }
    for suffix in ["", "-wal", "-shm"] {
        std::fs::copy(
            held.join(format!("copy{suffix}")),
            with_suffix(path, suffix),
        )
        .unwrap();
    }
    std::fs::remove_dir_all(&held).unwrap();
}

fn just(path: &PathBuf) -> &[PathBuf] {
    std::slice::from_ref(path)
}

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn a_database_that_never_synced_is_not_stranded() {
    let (root, nb) = logged_notebook("stranded-never-synced");
    let a = root.join("a.kame");
    Session::open(&a).unwrap().insert("Never synced.");
    assert!(find_stranded(&nb).is_empty());
    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(a.is_file());
}

#[test]
fn one_outside_a_marked_folder_is_not_stranded() {
    let (root, nb) = logged_notebook("stranded-outside-marker");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    assert_eq!(find_stranded(&nb), just(&a));
    std::fs::remove_file(root.join(MARKER)).unwrap();
    assert!(find_stranded(&nb).is_empty());
    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(a.is_file());
}

#[test]
fn one_with_its_logs_is_not_stranded() {
    let (root, nb) = logged_notebook("stranded-with-logs");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    assert!(find_stranded(&nb).is_empty());
    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(a.is_file());
}

#[test]
fn a_moved_document_joins_its_logs_with_history_intact() {
    let (root, nb) = logged_notebook("stranded-moved");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    let (history, snapshots) = {
        let mut session = Session::open(&a).unwrap();
        session.snapshot(Some("kept"));
        (session.history(), session.snapshots().unwrap())
    };
    let revisions = Store::load(&log_folder(&a)).unwrap().revision_count();
    move_logs(&a, &b);

    assert_eq!(find_stranded(&nb), just(&a));
    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Moved {
            from: a.clone(),
            to: b.clone()
        }]
    );
    assert!(!a.exists());
    assert!(find_stranded(&nb).is_empty());

    let mut session = Session::open(&b).unwrap();
    assert_eq!(session.history(), history);
    assert_eq!(session.snapshots().unwrap(), snapshots);
    let outcome = sync(&mut session);
    assert!(!outcome.stranded);
    assert!(!outcome.merged);
    assert_eq!(outcome.written, 0);
    assert_eq!(
        Store::load(&log_folder(&b)).unwrap().revision_count(),
        revisions
    );
    assert!(!log_folder(&a).exists());
}

#[test]
fn unsynced_words_merge_after_joining() {
    let (root, nb) = logged_notebook("stranded-unsynced");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Synced.");
    Session::open(&a)
        .unwrap()
        .insert(" Typed before the logs moved.");
    move_logs(&a, &b);

    reconcile(&nb, just(&a), None);
    let mut session = Session::open(&b).unwrap();
    assert!(sync(&mut session).written > 0);
    let store = Store::load(&log_folder(&b)).unwrap();
    let heads = store.heads();
    assert_eq!(heads.len(), 1);
    assert!(store.document(&heads[0]).blocks[0]
        .text
        .contains("Typed before the logs moved."));
}

#[test]
fn a_deleted_document_is_retired_and_stays_gone() {
    let (root, nb) = logged_notebook("stranded-deleted");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);

    let retired = root.join(".a.retired.kame");
    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Retired {
            from: a.clone(),
            to: retired.clone(),
            opened_there: false,
            unsynced: true,
        }]
    );
    assert!(!a.exists());
    assert!(text_of(&retired).contains("Words."));
    assert!(listed(&nb).is_empty());
    assert!(find_stranded(&nb).is_empty());
    assert!(!log_folder(&a).exists());
    assert!(reconcile(&nb, &[], None).is_empty());
    assert!(!a.exists());
}

#[test]
fn a_second_retirement_gets_a_number() {
    let (root, nb) = logged_notebook("stranded-second");
    let a = root.join("a.kame");
    synced(&a, "First.");
    remove_logs(&a);
    reconcile(&nb, just(&a), None);
    synced(&a, "Second.");
    remove_logs(&a);

    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Retired {
            from: a.clone(),
            to: root.join(".a.retired-2.kame"),
            opened_there: false,
            unsynced: true,
        }]
    );
    assert!(text_of(&root.join(".a.retired.kame")).contains("First."));
    assert!(text_of(&root.join(".a.retired-2.kame")).contains("Second."));
}

#[test]
fn a_reused_path_retires_the_old_database_and_keeps_the_new_logs_clean() {
    let (root, nb) = logged_notebook("stranded-reused");
    let (a, other) = (root.join("a.kame"), root.join("sub/other.kame"));
    synced(&a, "Old words that were deleted.");
    synced(&other, "A different document.");
    remove_logs(&a);
    move_logs(&other, &a);
    std::fs::remove_file(&other).unwrap();

    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Retired {
            from: a.clone(),
            to: root.join(".a.retired.kame"),
            opened_there: false,
            unsynced: true,
        }]
    );
    assert!(!words_in_logs(&log_folder(&a)).contains("Old words"));
    assert!(build_from_logs(&a).unwrap());
    assert!(text_of(&a).contains("A different document."));
    let mut session = Session::open(&a).unwrap();
    sync(&mut session);
    assert!(!words_in_logs(&log_folder(&a)).contains("Old words"));
}

#[test]
fn a_swap_moves_both_through_temporary_names() {
    let (root, nb) = logged_notebook("stranded-swap");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words of a.");
    synced(&b, "Words of b.");
    let held = root.join(".held");
    std::fs::rename(log_folder(&a), &held).unwrap();
    move_logs(&b, &a);
    std::fs::rename(&held, log_folder(&b)).unwrap();

    let done = reconcile(&nb, &[a.clone(), b.clone()], None);
    assert_eq!(
        done,
        [
            Done::Moved {
                from: a.clone(),
                to: b.clone()
            },
            Done::Moved {
                from: b.clone(),
                to: a.clone()
            },
        ]
    );
    assert!(text_of(&b).contains("Words of a."));
    assert!(text_of(&a).contains("Words of b."));
    assert!(find_stranded(&nb).is_empty());
    let left = names_in(&root);
    assert!(
        !left.iter().any(|name| name.contains(".moving-")),
        "{left:?}"
    );
}

#[test]
fn half_a_swap_waits_for_the_other_half() {
    let (root, nb) = logged_notebook("stranded-half-swap");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words of a.");
    synced(&b, "Words of b.");
    let held = root.join(".held");
    std::fs::rename(log_folder(&a), &held).unwrap();
    move_logs(&b, &a);
    std::fs::rename(&held, log_folder(&b)).unwrap();

    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(text_of(&a).contains("Words of a."));
    assert!(text_of(&b).contains("Words of b."));
    assert!(reconcile(&nb, &[a.clone(), b.clone()], Some(&b)).is_empty());
    assert!(text_of(&a).contains("Words of a."));
    assert!(!names_in(&root).iter().any(|name| name.contains(".moving-")));
}

#[test]
fn a_chain_of_moves_frees_each_name_for_the_next() {
    let (root, nb) = logged_notebook("stranded-chain");
    let (a, b, c) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("c.kame"),
    );
    synced(&a, "Words of a.");
    synced(&b, "Words of b.");
    move_logs(&b, &c);
    move_logs(&a, &b);

    let done = reconcile(&nb, &[a.clone(), b.clone()], None);
    assert_eq!(done.len(), 2, "{done:?}");
    assert!(!a.exists());
    assert!(text_of(&b).contains("Words of a."));
    assert!(text_of(&c).contains("Words of b."));
}

/// Were `m` to set out for `z` and be turned back, `a` would have taken its name by then.
#[test]
fn a_chain_stopped_at_its_far_end_does_not_set_out() {
    let (root, nb) = logged_notebook("stranded-stopped-chain");
    let (a, m, z, gone) = (
        root.join("a.kame"),
        root.join("m.kame"),
        root.join("z.kame"),
        root.join("gone.kame"),
    );
    synced(&a, "Words of a.");
    synced(&m, "Words of m.");
    synced(&z, "Words of z.");
    move_logs(&z, &gone);
    move_logs(&m, &z);
    move_logs(&a, &m);

    assert!(reconcile(&nb, &[a.clone(), m.clone()], None).is_empty());
    assert!(text_of(&a).contains("Words of a."));
    assert!(text_of(&m).contains("Words of m."));
    assert!(text_of(&z).contains("Words of z."));
    assert!(!names_in(&root).iter().any(|name| name.contains(".moving-")));
}

/// `b` meant to leave and couldn't, as it found no temporary name, so `a` can't have its place.
#[test]
fn a_name_that_was_to_be_freed_and_was_not_is_not_written_over() {
    let (root, nb) = logged_notebook("stranded-not-freed");
    let (a, b, c) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("c.kame"),
    );
    synced(&a, "Words of a.");
    synced(&b, "Words of b.");
    move_logs(&b, &c);
    move_logs(&a, &b);
    for number in 1..1000 {
        std::fs::write(root.join(format!(".b.moving-{number}.kame")), "").unwrap();
    }

    assert!(reconcile(&nb, &[a.clone(), b.clone()], None).is_empty());
    assert!(text_of(&a).contains("Words of a."));
    assert!(text_of(&b).contains("Words of b."));
}

#[test]
fn a_retirement_frees_its_name_for_a_move_in_the_same_pass() {
    let (root, nb) = logged_notebook("stranded-freed");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words of a.");
    synced(&b, "Words of b.");
    remove_logs(&b);
    move_logs(&a, &b);

    let done = reconcile(&nb, &[a.clone(), b.clone()], None);
    assert_eq!(
        done,
        [
            Done::Retired {
                from: b.clone(),
                to: root.join(".b.retired.kame"),
                opened_there: false,
                unsynced: true,
            },
            Done::Moved {
                from: a.clone(),
                to: b.clone()
            },
        ]
    );
    assert!(text_of(&b).contains("Words of a."));
    assert!(text_of(&root.join(".b.retired.kame")).contains("Words of b."));
}

#[test]
fn when_the_moved_document_is_already_open_there_the_old_one_retires() {
    let (root, nb) = logged_notebook("stranded-opened-there");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    move_logs(&a, &b);
    assert!(build_from_logs(&b).unwrap());

    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Retired {
            from: a.clone(),
            to: root.join(".a.retired.kame"),
            opened_there: true,
            unsynced: false,
        }]
    );
    assert!(b.is_file());
}

#[test]
fn two_homes_leave_it_alone() {
    let (root, nb) = logged_notebook("stranded-two-homes");
    let (a, b, c) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("c.kame"),
    );
    synced(&a, "Words.");
    copy_logs(&a, &b);
    move_logs(&a, &c);
    let before = std::fs::read(&a).unwrap();

    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert_eq!(std::fs::read(&a).unwrap(), before);
    assert!(!b.exists());
    assert!(!c.exists());
    assert_eq!(find_stranded(&nb), just(&a));
}

#[test]
fn two_databases_with_the_same_home_are_left_alone() {
    let (root, nb) = logged_notebook("stranded-same-home");
    let (a, copy, b) = (
        root.join("a.kame"),
        root.join("copy.kame"),
        root.join("b.kame"),
    );
    synced(&a, "Words.");
    std::fs::copy(&a, &copy).unwrap();
    move_logs(&a, &b);

    assert!(reconcile(&nb, &[a.clone(), copy.clone()], None).is_empty());
    assert!(a.is_file());
    assert!(copy.is_file());
    assert!(!b.exists());
}

#[test]
fn late_logs_bring_a_retired_database_back() {
    let (root, nb) = logged_notebook("stranded-late");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    let history = Session::open(&a).unwrap().history();
    let elsewhere = root.join(".carried");
    std::fs::rename(log_folder(&a), &elsewhere).unwrap();
    assert_eq!(reconcile(&nb, just(&a), None).len(), 1);
    let retired = root.join(".a.retired.kame");
    assert!(retired.is_file());

    std::fs::rename(&elsewhere, log_folder(&b)).unwrap();
    assert_eq!(
        reconcile(&nb, &[], None),
        [Done::Returned {
            from: retired.clone(),
            to: b.clone()
        }]
    );
    assert!(!retired.exists());
    assert_eq!(Session::open(&b).unwrap().history(), history);
    assert!(find_stranded(&nb).is_empty());
}

#[test]
fn a_retired_database_stays_retired_with_a_database_at_its_logs() {
    let (root, nb) = logged_notebook("stranded-stays-retired");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    move_logs(&a, &b);
    assert!(build_from_logs(&b).unwrap());
    reconcile(&nb, just(&a), None);
    let before = std::fs::read(&b).unwrap();

    assert!(reconcile(&nb, &[], None).is_empty());
    assert!(root.join(".a.retired.kame").is_file());
    assert_eq!(std::fs::read(&b).unwrap(), before);
}

/// The document moved on from `b` to `c`, and `b` has words the retired copy doesn't. Were the
/// retired copy to take `c` while `b` waits, `b` would then be the one retired.
#[test]
fn a_retired_database_leaves_a_home_to_the_stranded_one_still_waiting() {
    let (root, nb) = logged_notebook("stranded-retired-and-waiting");
    let (a, b, c) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("c.kame"),
    );
    synced(&a, "Words.");
    move_logs(&a, &b);
    assert!(build_from_logs(&b).unwrap());
    reconcile(&nb, just(&a), None);
    {
        let mut session = Session::open(&b).unwrap();
        session.insert(" Newer.");
        sync(&mut session);
    }
    move_logs(&b, &c);

    assert!(reconcile(&nb, &[], None).is_empty());
    assert!(root.join(".a.retired.kame").is_file());
    assert!(reconcile(&nb, &[], Some(&b)).is_empty());
    assert!(root.join(".a.retired.kame").is_file());

    assert_eq!(
        reconcile(&nb, just(&b), None),
        [Done::Moved {
            from: b.clone(),
            to: c.clone()
        }]
    );
    assert!(text_of(&c).contains("Newer."));
    assert!(root.join(".a.retired.kame").is_file());
}

#[test]
fn two_retired_databases_with_the_same_home_stay_retired() {
    let (root, nb) = logged_notebook("stranded-two-retired");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    let (one, other) = (root.join(".a.retired.kame"), root.join(".a.retired-2.kame"));
    std::fs::copy(&a, &one).unwrap();
    std::fs::rename(&a, &other).unwrap();
    move_logs(&a, &b);

    assert!(reconcile(&nb, &[], None).is_empty());
    assert!(one.is_file());
    assert!(other.is_file());
    assert!(!b.exists());
}

#[test]
fn a_move_stopped_by_a_crash_comes_back_from_its_temporary_name() {
    let (root, nb) = logged_notebook("stranded-crash");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    let parked = root.join(".a.moving-1.kame");
    std::fs::rename(&a, &parked).unwrap();
    move_logs(&a, &b);
    assert!(listed(&nb) == ["b.kame"]);

    assert_eq!(
        reconcile(&nb, &[], None),
        [Done::Returned {
            from: parked.clone(),
            to: b.clone()
        }]
    );
    assert!(text_of(&b).contains("Words."));
}

#[test]
fn a_temporary_name_left_by_a_crash_is_not_used_again() {
    let (root, nb) = logged_notebook("stranded-crash-reused");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    let parked = root.join(".a.moving-1.kame");
    std::fs::write(&parked, "left by a crash").unwrap();
    move_logs(&a, &b);

    assert_eq!(reconcile(&nb, just(&a), None).len(), 1);
    assert!(text_of(&b).contains("Words."));
    assert_eq!(std::fs::read(&parked).unwrap(), b"left by a crash");
}

#[test]
fn a_save_as_copy_is_never_mistaken() {
    let (root, nb) = logged_notebook("stranded-save-as");
    let (a, c) = (root.join("a.kame"), root.join("c.kame"));
    synced(&a, "Words.");
    {
        let mut session = Session::open(&a).unwrap();
        session.save_as(&c).unwrap();
    }
    assert!(find_stranded(&nb).is_empty());

    let copy = std::fs::read(&c).unwrap();
    remove_logs(&a);
    assert_eq!(find_stranded(&nb), just(&a));
    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Retired {
            from: a.clone(),
            to: root.join(".a.retired.kame"),
            opened_there: false,
            unsynced: true,
        }]
    );
    assert_eq!(std::fs::read(&c).unwrap(), copy);
}

#[test]
fn only_what_may_be_acted_on_is_touched() {
    let (root, nb) = logged_notebook("stranded-may-act");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words of a.");
    synced(&b, "Words of b.");
    remove_logs(&a);
    remove_logs(&b);

    assert!(reconcile(&nb, &[], None).is_empty());
    assert_eq!(find_stranded(&nb), [a.clone(), b.clone()]);
    assert_eq!(reconcile(&nb, just(&b), None).len(), 1);
    assert!(a.is_file());
    assert!(!b.exists());
}

#[test]
fn what_has_stopped_being_stranded_is_left_alone() {
    let (root, nb) = logged_notebook("stranded-recheck");
    let (a, gone) = (root.join("a.kame"), root.join("gone.kame"));
    synced(&a, "Words.");
    assert!(reconcile(&nb, &[a.clone(), gone.clone()], None).is_empty());
    assert!(a.is_file());
}

#[test]
fn a_path_that_is_no_document_of_the_notebook_is_left_alone() {
    let (root, nb) = logged_notebook("stranded-not-a-document");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    reconcile(&nb, just(&a), None);
    let retired = root.join(".a.retired.kame");
    assert!(reconcile(&nb, just(&retired), None).is_empty());
    assert!(retired.is_file());

    let outside = kame_test_dir::folder("stranded-not-a-document-outside")
        .canonicalize()
        .unwrap();
    std::fs::write(outside.join(MARKER), "").unwrap();
    let there = outside.join("there.kame");
    synced(&there, "Words.");
    remove_logs(&there);
    assert!(reconcile(&nb, just(&there), None).is_empty());
    assert!(there.is_file());

    std::os::unix::fs::symlink(&outside, root.join("out-link")).unwrap();
    let through_link = root.join("out-link/there.kame");
    assert!(reconcile(&nb, &[through_link], None).is_empty());
    assert!(there.is_file());
}

#[test]
fn the_open_document_is_never_touched() {
    let (root, nb) = logged_notebook("stranded-open");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    assert!(reconcile(&nb, just(&a), Some(&a)).is_empty());
    assert!(a.is_file());
}

#[test]
fn a_retired_database_that_is_open_stays_where_it_is() {
    let (root, nb) = logged_notebook("stranded-open-retired");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    let elsewhere = root.join(".carried");
    std::fs::rename(log_folder(&a), &elsewhere).unwrap();
    reconcile(&nb, just(&a), None);
    let retired = root.join(".a.retired.kame");
    std::fs::rename(&elsewhere, log_folder(&b)).unwrap();

    assert!(reconcile(&nb, &[], Some(&retired)).is_empty());
    assert!(retired.is_file());
    assert!(!b.exists());
}

#[test]
fn nothing_goes_to_a_path_on_the_screen_with_no_file_yet() {
    let (root, nb) = logged_notebook("stranded-open-path");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    move_logs(&a, &b);
    assert!(reconcile(&nb, just(&a), Some(&b)).is_empty());
    assert!(a.is_file());
    assert!(!b.exists());

    let retired = root.join(".a.retired.kame");
    std::fs::rename(&a, &retired).unwrap();
    assert!(reconcile(&nb, &[], Some(&b)).is_empty());
    assert!(retired.is_file());
    assert!(!b.exists());
}

#[test]
fn retired_files_are_not_listed_and_their_side_files_go_with_them() {
    let (root, nb) = logged_notebook("stranded-side-files");
    let a = root.join("a.kame");
    synced(&a, "Synced.");
    crash_with_words_in_the_wal(&a, " Only in the wal.");
    remove_logs(&a);

    let retired = root.join(".a.retired.kame");
    assert_eq!(reconcile(&nb, just(&a), None).len(), 1);
    assert_eq!(
        names_in(&root),
        [
            ".a.retired.kame",
            ".a.retired.kame-shm",
            ".a.retired.kame-wal",
            ".kame-logs",
            "notebook.yaml"
        ]
    );
    assert!(listed(&nb).is_empty());
    assert!(nb.entries().is_empty());
    assert!(text_of(&retired).contains("Only in the wal."));
}

#[test]
fn a_moved_database_takes_its_side_files_along() {
    let (root, nb) = logged_notebook("stranded-move-side-files");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Synced.");
    crash_with_words_in_the_wal(&a, " Only in the wal.");
    move_logs(&a, &b);

    assert_eq!(reconcile(&nb, just(&a), None).len(), 1);
    assert!(!with_suffix(&a, "-wal").exists());
    assert!(!with_suffix(&a, "-shm").exists());
    assert!(text_of(&b).contains("Only in the wal."));
}

#[test]
fn a_stale_wal_at_the_new_home_stops_the_move() {
    let (root, nb) = logged_notebook("stranded-stale-wal");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    move_logs(&a, &b);
    for suffix in ["-wal", "-shm"] {
        let stale = with_suffix(&b, suffix);
        std::fs::write(&stale, "stale").unwrap();
        assert!(reconcile(&nb, just(&a), None).is_empty(), "{suffix}");
        assert!(a.is_file());
        assert!(!b.exists());
        assert_eq!(std::fs::read(&stale).unwrap(), b"stale");
        std::fs::remove_file(&stale).unwrap();
    }
    assert!(!names_in(&root).iter().any(|name| name.contains(".moving-")));
    assert_eq!(reconcile(&nb, just(&a), None).len(), 1);
}

#[test]
fn a_stale_wal_at_the_new_home_keeps_a_retired_database_retired() {
    let (root, nb) = logged_notebook("stranded-stale-wal-return");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    let elsewhere = root.join(".carried");
    std::fs::rename(log_folder(&a), &elsewhere).unwrap();
    reconcile(&nb, just(&a), None);
    let retired = root.join(".a.retired.kame");
    std::fs::rename(&elsewhere, log_folder(&b)).unwrap();
    let stale = with_suffix(&b, "-wal");
    std::fs::write(&stale, "stale").unwrap();

    assert!(reconcile(&nb, &[], None).is_empty());
    assert!(retired.is_file());
    assert_eq!(std::fs::read(&stale).unwrap(), b"stale");
}

#[test]
fn a_retired_name_with_a_stale_side_file_is_passed_over() {
    let (root, nb) = logged_notebook("stranded-retired-stale");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    let stale = root.join(".a.retired.kame-wal");
    std::fs::write(&stale, "stale").unwrap();

    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Retired {
            from: a.clone(),
            to: root.join(".a.retired-2.kame"),
            opened_there: false,
            unsynced: true,
        }]
    );
    assert_eq!(std::fs::read(&stale).unwrap(), b"stale");
}

#[test]
fn a_name_with_dots_is_retired_under_its_whole_name() {
    let (root, nb) = logged_notebook("stranded-dots");
    let a = root.join("sub/v1.2.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Retired {
            from: a.clone(),
            to: root.join("sub/.v1.2.retired.kame"),
            opened_there: false,
            unsynced: true,
        }]
    );
}

#[test]
fn a_document_moved_to_another_folder_is_found_there() {
    let (root, nb) = logged_notebook("stranded-other-folder");
    let (a, b) = (root.join("a.kame"), root.join("deep/down/b.kame"));
    synced(&a, "Words.");
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    move_logs(&a, &b);
    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Moved {
            from: a.clone(),
            to: b.clone()
        }]
    );
    assert!(text_of(&b).contains("Words."));
}

#[test]
fn logs_where_no_document_can_be_are_no_home() {
    let (root, nb) = logged_notebook("stranded-no-document-area");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let b = root.join("static/b.kame");
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    move_logs(&a, &b);
    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Retired {
            from: a.clone(),
            to: root.join(".a.retired.kame"),
            opened_there: false,
            unsynced: true,
        }]
    );
    assert!(!b.exists());
}

#[test]
fn old_logs_kept_by_the_sync_tool_are_no_home() {
    let (root, nb) = logged_notebook("stranded-versions");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let kept = root.join(".stversions/a.kame");
    std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
    move_logs(&a, &kept);
    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Retired {
            from: a.clone(),
            to: root.join(".a.retired.kame"),
            opened_there: false,
            unsynced: true,
        }]
    );
}

#[test]
fn logs_from_a_newer_kame_could_be_its_home_so_nothing_is_done() {
    let (root, nb) = logged_notebook("stranded-newer");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&a, "Words.");
    synced(&other, "Other words.");
    remove_logs(&a);
    add_newer_line(&other);

    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(a.is_file());
}

#[test]
fn a_retired_database_waits_too_while_some_logs_are_from_a_newer_kame() {
    let (root, nb) = logged_notebook("stranded-newer-return");
    let (a, b, other) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("other.kame"),
    );
    synced(&a, "Words.");
    synced(&other, "Other words.");
    let elsewhere = root.join(".carried");
    std::fs::rename(log_folder(&a), &elsewhere).unwrap();
    reconcile(&nb, just(&a), None);
    std::fs::rename(&elsewhere, log_folder(&b)).unwrap();
    add_newer_line(&other);

    assert!(reconcile(&nb, &[], None).is_empty());
    assert!(root.join(".a.retired.kame").is_file());
}

#[test]
fn a_log_that_cannot_be_read_could_be_its_home_so_nothing_is_done() {
    let (root, nb) = logged_notebook("stranded-unreadable-log");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&a, "Words.");
    synced(&other, "Other words.");
    remove_logs(&a);
    let log = std::fs::read_dir(log_folder(&other))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    set_mode(&log, 0o000);

    let done = reconcile(&nb, just(&a), None);
    set_mode(&log, 0o644);
    assert!(done.is_empty());
    assert!(a.is_file());
}

#[test]
fn a_log_folder_that_cannot_be_listed_could_be_its_home_so_nothing_is_done() {
    let (root, nb) = logged_notebook("stranded-unlistable-logs");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&a, "Words.");
    synced(&other, "Other words.");
    remove_logs(&a);
    let folder = log_folder(&other);
    set_mode(&folder, 0o000);

    let done = reconcile(&nb, just(&a), None);
    set_mode(&folder, 0o755);
    assert!(done.is_empty());
    assert!(a.is_file());
}

#[test]
fn a_folder_that_cannot_be_read_could_hold_its_home_so_nothing_is_done() {
    let (root, nb) = logged_notebook("stranded-unreadable-folder");
    let (a, b) = (root.join("a.kame"), root.join("shut/b.kame"));
    synced(&a, "Words.");
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    move_logs(&a, &b);
    let shut = root.join("shut");
    set_mode(&shut, 0o000);

    let done = reconcile(&nb, just(&a), None);
    set_mode(&shut, 0o755);
    assert!(done.is_empty());
    assert!(a.is_file());
}

/// `is_stranded` reads an unlistable folder as empty, and a file there as no logs.
#[test]
fn its_own_log_folder_has_to_be_readable_to_tell() {
    let (root, nb) = logged_notebook("stranded-own-logs");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let folder = log_folder(&a);
    set_mode(&folder, 0o000);
    let done = reconcile(&nb, just(&a), None);
    set_mode(&folder, 0o755);
    assert!(done.is_empty());
    assert!(a.is_file());

    remove_logs(&a);
    std::fs::write(&folder, "a file where the logs go").unwrap();
    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(a.is_file());
}

#[test]
fn a_database_that_cannot_be_read_at_the_new_home_says_nothing_so_it_is_left_alone() {
    let (root, nb) = logged_notebook("stranded-garbage-home");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    move_logs(&a, &b);
    std::fs::write(&b, "not a database").unwrap();

    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(a.is_file());
    assert_eq!(std::fs::read(&b).unwrap(), b"not a database");
}

#[test]
fn a_folder_at_the_new_home_is_left_alone() {
    let (root, nb) = logged_notebook("stranded-folder-home");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    move_logs(&a, &b);
    std::fs::create_dir_all(&b).unwrap();

    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(a.is_file());
    assert!(b.is_dir());
}

#[test]
fn a_move_that_cannot_finish_goes_back() {
    let (root, nb) = logged_notebook("stranded-cannot-finish");
    let (a, b) = (root.join("a.kame"), root.join("shut/b.kame"));
    synced(&a, "Words.");
    let shut = root.join("shut");
    std::fs::create_dir_all(&shut).unwrap();
    move_logs(&a, &b);
    // Readable and searchable, so that its logs can be read, but nothing can be put in it.
    set_mode(&shut, 0o555);

    let done = reconcile(&nb, just(&a), None);
    set_mode(&shut, 0o755);
    assert!(done.is_empty());
    assert!(a.is_file());
    assert!(!b.exists());
    assert!(!names_in(&root).iter().any(|name| name.contains(".moving-")));
    assert!(text_of(&a).contains("Words."));
}

#[test]
fn a_database_that_cannot_be_renamed_is_not_reported() {
    let (root, nb) = logged_notebook("stranded-cannot-retire");
    let a = root.join("shut/a.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    let shut = root.join("shut");
    set_mode(&shut, 0o555);

    let done = reconcile(&nb, just(&a), None);
    set_mode(&shut, 0o755);
    assert!(done.is_empty());
    assert!(a.is_file());
}

#[test]
fn a_retired_database_with_words_that_never_synced_says_so() {
    let (root, nb) = logged_notebook("stranded-unsynced-words");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Synced.");
    Session::open(&a)
        .unwrap()
        .insert(" Typed while it could not sync.");
    move_logs(&a, &b);
    assert!(build_from_logs(&b).unwrap());

    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Retired {
            from: a.clone(),
            to: root.join(".a.retired.kame"),
            opened_there: true,
            unsynced: true,
        }]
    );
    assert!(text_of(&root.join(".a.retired.kame")).contains("Typed while it could not sync."));
    assert!(!text_of(&b).contains("Typed while it could not sync."));
}

#[test]
fn a_retired_database_holding_nothing_beyond_its_logs_says_so() {
    let (root, nb) = logged_notebook("stranded-nothing-unsynced");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Synced.");
    move_logs(&a, &b);
    assert!(build_from_logs(&b).unwrap());
    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Retired {
            from: a.clone(),
            to: root.join(".a.retired.kame"),
            opened_there: true,
            unsynced: false,
        }]
    );
}

#[test]
fn a_retired_database_with_only_a_new_title_counts_as_unsynced() {
    let (root, nb) = logged_notebook("stranded-unsynced-title");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Synced.");
    {
        let mut session = Session::open(&a).unwrap();
        let metadata = kame_core::model::Metadata {
            title: "A title of its own".to_string(),
            ..session.metadata.clone()
        };
        session.set_metadata(metadata);
    }
    move_logs(&a, &b);
    assert!(build_from_logs(&b).unwrap());
    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Retired {
            from: a.clone(),
            to: root.join(".a.retired.kame"),
            opened_there: true,
            unsynced: true,
        }]
    );
}

fn assert_no_temporary_names(root: &Path) {
    let left = names_in(root);
    assert!(
        !left.iter().any(|name| name.contains(".moving-")),
        "{left:?}"
    );
}

#[test]
fn a_step_is_skipped_if_what_it_found_has_changed() {
    // The stranded database has been opened since.
    let (root, nb) = logged_notebook("stranded-changed-opened");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    move_logs(&a, &b);
    let steps = decide(&nb, just(&a));
    let mut session = Session::open(&a).unwrap();
    assert!(apply(&nb, steps, Some(&a)).is_empty());
    assert!(a.is_file());
    assert!(!b.exists());
    assert_no_temporary_names(&root);
    assert!(session.insert(" Still editable."));
    assert!(!session.save_failed);
    drop(session);
    assert!(text_of(&a).contains("Still editable."));

    // A database has been built at the target since. This is what would otherwise destroy one.
    let (root, nb) = logged_notebook("stranded-changed-target");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    move_logs(&a, &b);
    let steps = decide(&nb, just(&a));
    assert!(build_from_logs(&b).unwrap());
    let built = std::fs::read(&b).unwrap();
    assert!(apply(&nb, steps, None).is_empty());
    assert_eq!(std::fs::read(&b).unwrap(), built);
    assert!(a.is_file());
    assert_no_temporary_names(&root);
    assert!(matches!(
        reconcile(&nb, just(&a), None)[..],
        [Done::Retired {
            opened_there: true,
            ..
        }]
    ));

    // The source has gone.
    let (root, nb) = logged_notebook("stranded-changed-source");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    move_logs(&a, &b);
    let steps = decide(&nb, just(&a));
    std::fs::remove_file(&a).unwrap();
    assert!(apply(&nb, steps, None).is_empty());
    assert!(!b.exists());
    assert_no_temporary_names(&root);

    // It remembers something else now, so it isn't what was looked at.
    let (root, nb) = logged_notebook("stranded-changed-state");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    move_logs(&a, &b);
    let steps = decide(&nb, just(&a));
    Session::open(&a).unwrap().set_sync_state("something else");
    assert!(apply(&nb, steps, None).is_empty());
    assert!(a.is_file());
    assert!(!b.exists());
    assert_no_temporary_names(&root);
}

#[test]
fn a_retirement_is_skipped_if_what_it_found_has_changed() {
    let (root, nb) = logged_notebook("stranded-changed-retire");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);

    let steps = decide(&nb, just(&a));
    let session = Session::open(&a).unwrap();
    assert!(apply(&nb, steps, Some(&a)).is_empty());
    drop(session);
    assert!(a.is_file());

    let steps = decide(&nb, just(&a));
    Session::open(&a).unwrap().set_sync_state("something else");
    assert!(apply(&nb, steps, None).is_empty());
    assert!(a.is_file());

    let steps = decide(&nb, just(&a));
    std::fs::remove_file(&a).unwrap();
    assert!(apply(&nb, steps, None).is_empty());
    assert!(!root.join(".a.retired.kame").exists());
}

#[test]
fn a_return_is_skipped_if_what_it_found_has_changed() {
    let (root, nb) = logged_notebook("stranded-changed-return");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    let retired = root.join(".a.retired.kame");
    std::fs::rename(&a, &retired).unwrap();
    move_logs(&a, &b);

    // The retired file is opened by hand, and then the path it would go to is.
    let steps = decide(&nb, &[]);
    assert!(apply(&nb, steps, Some(&retired)).is_empty());
    assert!(retired.is_file());
    let steps = decide(&nb, &[]);
    assert!(apply(&nb, steps, Some(&b)).is_empty());
    assert!(retired.is_file());

    // A database has been built there.
    let steps = decide(&nb, &[]);
    assert!(build_from_logs(&b).unwrap());
    let built = std::fs::read(&b).unwrap();
    assert!(apply(&nb, steps, None).is_empty());
    assert_eq!(std::fs::read(&b).unwrap(), built);
    assert!(retired.is_file());
}

/// The app reads on a thread, and renames on the one that opens documents.
#[test]
fn deciding_on_another_thread_changes_nothing_until_it_is_applied() {
    let (root, nb) = logged_notebook("stranded-decide-only");
    let (a, b, c, d, e) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("c.kame"),
        root.join("d.kame"),
        root.join("e.kame"),
    );
    synced(&a, "Moves.");
    synced(&b, "Retires.");
    synced(&c, "Returns.");
    let retired = root.join(".c.retired.kame");
    std::fs::rename(&c, &retired).unwrap();
    move_logs(&c, &d);
    move_logs(&a, &e);
    remove_logs(&b);
    let everything = |root: &Path| -> Vec<(String, Option<Vec<u8>>)> {
        names_in(root)
            .into_iter()
            .map(|name| {
                let bytes = std::fs::read(root.join(&name)).ok();
                (name, bytes)
            })
            .collect()
    };
    let before = everything(&root);

    let thread_notebook = nb.clone();
    let may_act_on = vec![a.clone(), b.clone()];
    let steps = std::thread::spawn(move || decide(&thread_notebook, &may_act_on))
        .join()
        .unwrap();
    assert_eq!(everything(&root), before);

    assert_eq!(
        apply(&nb, steps, None),
        [
            Done::Retired {
                from: b.clone(),
                to: root.join(".b.retired.kame"),
                opened_there: false,
                unsynced: true,
            },
            Done::Moved {
                from: a.clone(),
                to: e.clone(),
            },
            Done::Returned {
                from: retired,
                to: d.clone(),
            },
        ]
    );
}

#[test]
fn steps_are_applied_only_to_the_notebook_they_were_found_in() {
    let (root, nb) = logged_notebook("stranded-other-notebook");
    let (_, other) = logged_notebook("stranded-other-notebook-2");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    assert!(apply(&other, decide(&nb, just(&a)), None).is_empty());
    assert!(a.is_file());
}
