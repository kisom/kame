//! Databases whose logs have left them. The other machine is played by what it does to the log
//! folders here, as the sync tool would carry it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use kame_core::session::Session;
use kame_core::storage::Db;
use kame_notebook::manage::{carry_out, delete_document, plan_move};
use kame_notebook::stranded::{apply, decide, find_stranded, reconcile, Done, Healthy, Unsettled};
use kame_notebook::Notebook;
use kame_sync::logs::{build_from_logs, log_folder, place_of, Logged, MARKER};
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

/// What a kame from before places were recorded leaves: a sync state, and no place.
fn as_an_older_kame_left_it(path: &Path) {
    let state = Db::open_read_only(path).unwrap().load_sync_state().unwrap();
    let mut db = Db::open(path).unwrap();
    db.clear_sync_state().unwrap();
    db.save_sync_state(&state).unwrap();
}

fn place_in(path: &Path) -> Option<String> {
    Db::open_read_only(path).unwrap().load_sync_place()
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
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(a.is_file());
}

#[test]
fn one_outside_a_marked_folder_is_not_stranded() {
    let (root, nb) = logged_notebook("stranded-outside-marker");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&a));
    std::fs::remove_file(root.join(MARKER)).unwrap();
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(a.is_file());
}

#[test]
fn one_with_its_logs_is_not_stranded() {
    let (root, nb) = logged_notebook("stranded-with-logs");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
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

    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&a));
    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Moved {
            from: a.clone(),
            to: b.clone()
        }]
    );
    assert!(!a.exists());
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());

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
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
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
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
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
    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&a));
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
    // Each synced where it is, at the same revision, so that each is stale once the logs have left.
    std::fs::copy(&a, &copy).unwrap();
    copy_logs(&a, &copy);
    sync(&mut Session::open(&copy).unwrap());
    remove_logs(&copy);
    move_logs(&a, &b);
    assert_eq!(
        find_stranded(&nb, &mut Healthy::default()),
        [a.clone(), copy.clone()]
    );

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
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
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
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());

    let copy = std::fs::read(&c).unwrap();
    remove_logs(&a);
    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&a));
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
    assert_eq!(
        find_stranded(&nb, &mut Healthy::default()),
        [a.clone(), b.clone()]
    );
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

/// A home it has found needs nothing ruled out.
#[test]
fn a_retired_database_comes_back_while_some_logs_are_from_a_newer_kame() {
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

    assert_eq!(reconcile(&nb, &[], None).len(), 1);
    assert!(b.is_file());
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

/// A stranded document stays editable and saves no new revision, so its revision can't say whether
/// it changed after `decide` said what it holds.
#[test]
fn a_retirement_is_left_for_the_next_pass_if_words_were_typed_after_it_was_decided() {
    let (root, nb) = logged_notebook("stranded-typed-after-decide");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Synced.");
    move_logs(&a, &b);
    assert!(build_from_logs(&b).unwrap());
    let steps = decide(&nb, just(&a));
    {
        let mut session = Session::open(&a).unwrap();
        session.insert(" Typed after decide.");
        assert!(sync(&mut session).stranded);
    }

    assert!(apply(&nb, steps, Some(&b)).is_empty());
    assert!(text_of(&a).contains("Typed after decide."));
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

#[test]
fn a_retirement_is_left_for_the_next_pass_if_words_were_typed_in_a_session_open_through_deciding() {
    let (root, nb) = logged_notebook("stranded-open-through-decide");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Synced.");
    move_logs(&a, &b);
    assert!(build_from_logs(&b).unwrap());
    let mut session = Session::open(&a).unwrap();
    let steps = decide(&nb, just(&a));
    session.insert(" Typed while open.");
    drop(session);

    assert!(apply(&nb, steps, None).is_empty());
    assert!(a.is_file());
    assert!(matches!(
        reconcile(&nb, just(&a), None)[..],
        [Done::Retired { unsynced: true, .. }]
    ));
}

#[test]
fn a_retirement_is_left_for_the_next_pass_if_only_the_title_changed_after_it_was_decided() {
    let (root, nb) = logged_notebook("stranded-title-after-decide");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Synced.");
    move_logs(&a, &b);
    assert!(build_from_logs(&b).unwrap());
    let steps = decide(&nb, just(&a));
    {
        let mut session = Session::open(&a).unwrap();
        let metadata = kame_core::model::Metadata {
            title: "A title of its own".to_string(),
            ..session.metadata.clone()
        };
        session.set_metadata(metadata);
    }

    assert!(apply(&nb, steps, None).is_empty());
    assert!(a.is_file());
}

/// Opening a document and leaving it unchanged is no reason to wait.
#[test]
fn a_retirement_goes_ahead_if_the_database_was_only_opened_after_it_was_decided() {
    let (root, nb) = logged_notebook("stranded-opened-only");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Synced.");
    move_logs(&a, &b);
    assert!(build_from_logs(&b).unwrap());
    let steps = decide(&nb, just(&a));
    drop(Session::open(&a).unwrap());

    assert!(matches!(
        apply(&nb, steps, None)[..],
        [Done::Retired {
            opened_there: true,
            unsynced: false,
            ..
        }]
    ));
}

/// A document's one log.
fn only_log(path: &Path) -> PathBuf {
    let logs: Vec<PathBuf> = std::fs::read_dir(log_folder(path))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(logs.len(), 1, "{logs:?}");
    logs.into_iter().next().unwrap()
}

/// Blanks a log at the same size. Blank lines hold nothing, and can't be holding a revision.
fn blank_at_the_same_size(log: &Path) {
    let size = std::fs::metadata(log).unwrap().len();
    std::fs::write(log, "\n".repeat(size as usize)).unwrap();
}

/// Makes a document's log say something else at the same size, so that reading it finds no sign of
/// the revision its database remembers.
fn spoil_at_the_same_size(path: &Path) {
    blank_at_the_same_size(&only_log(path));
}

#[test]
fn a_second_look_at_healthy_logs_reads_none_of_them() {
    let (root, nb) = logged_notebook("stranded-healthy-unread");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let mut healthy = Healthy::default();
    assert!(find_stranded(&nb, &mut healthy).is_empty());

    spoil_at_the_same_size(&a);

    assert_eq!(
        find_stranded(&nb, &mut Healthy::default()),
        just(&a),
        "read, the log no longer holds the revision"
    );
    assert!(find_stranded(&nb, &mut healthy).is_empty(), "so it wasn't");
    assert!(find_stranded(&nb, &mut healthy).is_empty(), "nor this time");
}

#[test]
fn a_log_of_another_size_is_read_again() {
    let (root, nb) = logged_notebook("stranded-healthy-size");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let mut healthy = Healthy::default();
    assert!(find_stranded(&nb, &mut healthy).is_empty());

    spoil_at_the_same_size(&a);
    let log = only_log(&a);
    let mut grown = std::fs::read(&log).unwrap();
    grown.extend_from_slice(b"\n");
    std::fs::write(&log, grown).unwrap();

    assert_eq!(find_stranded(&nb, &mut healthy), just(&a));
}

#[test]
fn a_new_log_is_read_again() {
    let (root, nb) = logged_notebook("stranded-healthy-new-log");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let mut healthy = Healthy::default();
    assert!(find_stranded(&nb, &mut healthy).is_empty());

    spoil_at_the_same_size(&a);
    std::fs::write(log_folder(&a).join("other-0000.log"), "").unwrap();

    assert_eq!(find_stranded(&nb, &mut healthy), just(&a));
}

#[test]
fn another_recorded_revision_is_read_again() {
    let (root, nb) = logged_notebook("stranded-healthy-revision");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let mut healthy = Healthy::default();
    assert!(find_stranded(&nb, &mut healthy).is_empty());

    Session::open(&a).unwrap().set_sync_state("not in its logs");

    assert_eq!(find_stranded(&nb, &mut healthy), just(&a));
}

#[test]
fn logs_gone_are_seen_at_once_whatever_is_remembered() {
    let (root, nb) = logged_notebook("stranded-healthy-gone");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let mut healthy = Healthy::default();
    assert!(find_stranded(&nb, &mut healthy).is_empty());

    remove_logs(&a);

    assert_eq!(find_stranded(&nb, &mut healthy), just(&a));
}

#[test]
fn being_stranded_is_never_remembered() {
    let (root, nb) = logged_notebook("stranded-healthy-not-stranded");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let carried = root.join(".carried");
    std::fs::rename(log_folder(&a), &carried).unwrap();
    let mut healthy = Healthy::default();
    assert_eq!(find_stranded(&nb, &mut healthy), just(&a));
    assert_eq!(find_stranded(&nb, &mut healthy), just(&a));

    std::fs::rename(&carried, log_folder(&a)).unwrap();

    assert!(find_stranded(&nb, &mut healthy).is_empty());
}

#[test]
fn logs_that_cannot_be_read_are_not_remembered_as_healthy() {
    let (root, nb) = logged_notebook("stranded-healthy-unreadable");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let log = only_log(&a);
    set_mode(&log, 0o000);
    let mut healthy = Healthy::default();
    assert!(find_stranded(&nb, &mut healthy).is_empty(), "can't be told");

    set_mode(&log, 0o644);
    spoil_at_the_same_size(&a);

    assert_eq!(find_stranded(&nb, &mut healthy), just(&a));
}

#[test]
fn logs_from_a_newer_kame_are_not_remembered_as_healthy() {
    let (root, nb) = logged_notebook("stranded-healthy-newer");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let log = only_log(&a);
    add_newer_line(&a);
    let newer = log_folder(&a).join("new-0000.log");
    let mut healthy = Healthy::default();
    assert!(find_stranded(&nb, &mut healthy).is_empty(), "can't be told");

    blank_at_the_same_size(&newer);
    blank_at_the_same_size(&log);

    assert_eq!(find_stranded(&nb, &mut healthy), just(&a));
}

/// The case that found the rule: a byte-identical copy with the original's sync state and no logs.
#[test]
fn a_hand_copy_is_not_stranded_and_starts_its_own_logs() {
    let (root, nb) = logged_notebook("stranded-hand-copy");
    let (other_root, other_nb) = logged_notebook("stranded-hand-copy-other");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let (original, its_logs) = (std::fs::read(&a).unwrap(), words_in_logs(&log_folder(&a)));

    let copies = [
        root.join("deep/down/a.kame"),
        root.join("z.kame"),
        other_root.join("deep/a.kame"),
        other_root.join("z.kame"),
    ];
    for copy in &copies {
        std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
        std::fs::copy(&a, copy).unwrap();
    }
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
    assert!(find_stranded(&other_nb, &mut Healthy::default()).is_empty());
    assert!(reconcile(&nb, &copies, None).is_empty());
    assert!(listed(&nb).contains(&"z.kame".to_string()));
    assert!(root.join("z.kame").is_file());

    for copy in &copies {
        let mut session = Session::open(copy).unwrap();
        let outcome = sync(&mut session);
        assert!(!outcome.stranded, "{}", copy.display());
        assert!(outcome.written > 0, "its history, as new logs");
        assert!(words_in_logs(&log_folder(copy)).contains("Words."));
        assert_eq!(session.sync_place(), place_of(copy));
        session.insert(" More.");
        assert!(sync(&mut session).written > 0, "and it syncs from then on");
    }
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
    assert!(find_stranded(&other_nb, &mut Healthy::default()).is_empty());
    assert_eq!(std::fs::read(&a).unwrap(), original);
    assert_eq!(words_in_logs(&log_folder(&a)), its_logs);
}

/// The logging root's name is in the place, so that a copy into another notebook isn't taken for this
/// document having lost its logs there.
#[test]
fn a_hand_copy_to_the_same_path_in_another_notebook_is_not_stranded() {
    let (root, _) = logged_notebook("stranded-hand-copy-same-path");
    let (other_root, other_nb) = logged_notebook("stranded-hand-copy-same-path-other");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let copy = other_root.join("a.kame");
    std::fs::copy(&a, &copy).unwrap();

    assert!(find_stranded(&other_nb, &mut Healthy::default()).is_empty());
    assert!(reconcile(&other_nb, just(&copy), None).is_empty());
    assert!(copy.is_file());
    let mut session = Session::open(&copy).unwrap();
    assert!(sync(&mut session).written > 0);
}

/// A notebook named differently on another machine says nothing about the logs having left, so a
/// database carried there is merely not stranded until its next sync.
#[test]
fn a_database_in_a_renamed_notebook_is_not_stranded_until_it_next_syncs() {
    let (root, _) = logged_notebook("stranded-renamed-root");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let renamed = root.with_file_name("stranded-renamed-root-now");
    std::fs::rename(&root, &renamed).unwrap();
    let nb = Notebook::open(&renamed).unwrap();
    let a = renamed.join("a.kame");
    assert_ne!(place_in(&a), place_of(&a));
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());

    let mut session = Session::open(&a).unwrap();
    assert_eq!(sync(&mut session).written, 0, "its logs came along");
    drop(session);
    assert_eq!(place_in(&a), place_of(&a));
    remove_logs(&a);
    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&a));
}

/// The logs went with it, so it has what it remembers beside it, and just says where it is now.
#[test]
fn a_move_by_hand_with_its_logs_carries_on() {
    let (root, nb) = logged_notebook("stranded-hand-move");
    let (a, b) = (root.join("a.kame"), root.join("deep/b.kame"));
    synced(&a, "Words.");
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    std::fs::rename(&a, &b).unwrap();
    move_logs(&a, &b);
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());

    let mut session = Session::open(&b).unwrap();
    let outcome = sync(&mut session);
    assert!(!outcome.stranded);
    assert_eq!(outcome.written, 0);
    assert_eq!(session.sync_place(), place_of(&b));
}

/// The logs stayed, so it starts its own, as a copy does, and the old ones have no database.
#[test]
fn a_move_by_hand_without_its_logs_is_like_a_copy() {
    let (root, nb) = logged_notebook("stranded-hand-move-alone");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    std::fs::rename(&a, &b).unwrap();
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());

    let mut session = Session::open(&b).unwrap();
    let outcome = sync(&mut session);
    assert!(!outcome.stranded);
    assert!(outcome.written > 0);
    assert!(log_folder(&a).is_dir());
    assert!(log_folder(&b).is_dir());
}

#[test]
fn a_database_with_no_recorded_place_is_not_stranded() {
    let (root, nb) = logged_notebook("stranded-no-place");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    as_an_older_kame_left_it(&a);
    assert_eq!(place_in(&a), None);

    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(a.is_file());

    let mut session = Session::open(&a).unwrap();
    let outcome = sync(&mut session);
    assert!(!outcome.stranded);
    assert!(outcome.written > 0);
    drop(session);
    assert_eq!(place_in(&a), place_of(&a));

    remove_logs(&a);
    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&a));
}

#[test]
fn a_retired_file_renamed_back_by_hand_syncs_again() {
    let (root, nb) = logged_notebook("stranded-renamed-back");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    assert_eq!(reconcile(&nb, just(&a), None).len(), 1);
    let retired = root.join(".a.retired.kame");

    // Under the name it left, it's in the place it last synced, with the logs still gone.
    std::fs::rename(&retired, &a).unwrap();
    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&a));

    let back = root.join("back.kame");
    std::fs::rename(&a, &back).unwrap();
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
    let mut session = Session::open(&back).unwrap();
    let outcome = sync(&mut session);
    assert!(!outcome.stranded);
    assert!(outcome.written > 0);
    assert!(words_in_logs(&log_folder(&back)).contains("Words."));
    session.insert(" More.");
    assert!(sync(&mut session).written > 0);
    drop(session);
    assert!(!log_folder(&a).exists());
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
}

/// A log folder for a name like this would be carried to every machine, and be taken there for a
/// document's home. So a set-aside database that's opened stays out of the logs, whatever its place.
#[test]
fn a_retired_file_opened_where_it_is_writes_no_logs() {
    let (root, nb) = logged_notebook("stranded-retired-opened");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    assert_eq!(reconcile(&nb, just(&a), None).len(), 1);
    let retired = root.join(".a.retired.kame");

    let mut session = Session::open(&retired).unwrap();
    session.insert(" Typed in the retired file.");
    let outcome = sync(&mut session);
    assert!(outcome.stranded);
    assert_eq!(outcome.written, 0);
    assert!(!log_folder(&retired).exists());
    drop(session);
    assert!(text_of(&retired).contains("Typed in the retired file."));
}

#[test]
fn a_copy_on_a_reused_path() {
    let (root, nb) = logged_notebook("stranded-copy-on-reused-path");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&a, "Old words.");
    synced(&other, "A different document.");
    remove_logs(&a);
    move_logs(&other, &a);
    std::fs::remove_file(&other).unwrap();
    let before = words_in_logs(&log_folder(&a));
    assert_eq!(place_in(&a), place_of(&a));

    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&a));
    let mut session = Session::open(&a).unwrap();
    assert!(sync(&mut session).stranded);
    drop(session);
    assert_eq!(words_in_logs(&log_folder(&a)), before);
}

/// Not stranded, by the rule, since it was never synced here. But its history mustn't reach them.
#[test]
fn a_database_from_elsewhere_on_another_documents_logs_is_listed_and_writes_nothing() {
    let (root, nb) = logged_notebook("stranded-others-logs");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&a, "Old words.");
    synced(&other, "A different document.");
    remove_logs(&a);
    move_logs(&other, &a);
    std::fs::remove_file(&other).unwrap();
    as_an_older_kame_left_it(&a);
    let before = words_in_logs(&log_folder(&a));

    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(listed(&nb).contains(&"a.kame".to_string()));

    let mut session = Session::open(&a).unwrap();
    session.insert(" Typed here.");
    let outcome = sync(&mut session);
    assert!(!outcome.stranded);
    assert!(outcome.others_logs);
    assert_eq!(outcome.written, 0);
    drop(session);
    assert_eq!(words_in_logs(&log_folder(&a)), before);
    assert!(text_of(&a).contains("Typed here."));
    assert_eq!(place_in(&a), None, "it hasn't synced here");
}

#[test]
fn a_database_that_arrives_at_a_new_home_is_recognised_if_that_moves_too() {
    let (root, nb) = logged_notebook("stranded-second-move");
    let (a, b, c) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("c.kame"),
    );
    synced(&a, "Words.");
    move_logs(&a, &b);
    assert_eq!(reconcile(&nb, just(&a), None).len(), 1);
    assert_eq!(place_in(&b), place_of(&b));

    move_logs(&b, &c);
    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&b));
    assert_eq!(
        reconcile(&nb, just(&b), None),
        [Done::Moved {
            from: b.clone(),
            to: c.clone()
        }]
    );
    assert_eq!(place_in(&c), place_of(&c));
}

#[test]
fn a_retired_database_found_by_its_revision_records_its_new_place_on_arrival() {
    let (root, nb) = logged_notebook("stranded-return-place");
    let (a, b, c) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("c.kame"),
    );
    synced(&a, "Words.");
    let carried = root.join(".carried");
    std::fs::rename(log_folder(&a), &carried).unwrap();
    reconcile(&nb, just(&a), None);
    let retired = root.join(".a.retired.kame");
    assert_eq!(place_in(&retired), place_of(&a), "as it was");

    std::fs::rename(&carried, log_folder(&b)).unwrap();
    assert_eq!(
        reconcile(&nb, &[], None),
        [Done::Returned {
            from: retired,
            to: b.clone()
        }]
    );
    assert_eq!(place_in(&b), place_of(&b));

    move_logs(&b, &c);
    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&b));
}

/// Then it's merely not stranded, which is what the rule falls back on.
#[test]
fn a_place_that_cannot_be_written_on_arrival_does_not_stop_the_move() {
    let (root, nb) = logged_notebook("stranded-place-unwritable");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    move_logs(&a, &b);
    set_mode(&a, 0o444);
    assert_eq!(
        reconcile(&nb, just(&a), None),
        [Done::Moved {
            from: a.clone(),
            to: b.clone()
        }]
    );
    set_mode(&b, 0o644);
    assert_eq!(place_in(&b), place_of(&a));
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
}

#[test]
fn a_hand_copy_does_not_keep_a_retired_database_from_its_home() {
    let (root, nb) = logged_notebook("stranded-copy-and-retired");
    let (a, b, z) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("z.kame"),
    );
    synced(&a, "Words.");
    std::fs::copy(&a, &z).unwrap();
    let retired = root.join(".a.retired.kame");
    std::fs::rename(&a, &retired).unwrap();
    move_logs(&a, &b);

    assert_eq!(
        reconcile(&nb, &[], None),
        [Done::Returned {
            from: retired,
            to: b.clone()
        }]
    );
    assert!(z.is_file());
}

/// That copy has nothing to do with the document whose logs came there, so it hasn't been opened.
#[test]
fn a_hand_copy_of_another_document_at_a_new_home_leaves_the_stale_one_alone() {
    let (root, nb) = logged_notebook("stranded-copy-at-home");
    let (a, b, other) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("other.kame"),
    );
    synced(&a, "Words of a.");
    synced(&other, "Words of the other.");
    std::fs::copy(&other, &b).unwrap();
    move_logs(&a, &b);
    let before = std::fs::read(&b).unwrap();

    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&a));
    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(a.is_file());
    assert_eq!(std::fs::read(&b).unwrap(), before);
}

#[test]
fn a_step_is_skipped_if_the_place_it_found_has_changed() {
    let (root, nb) = logged_notebook("stranded-changed-place");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    let steps = decide(&nb, just(&a));
    Session::open(&a).unwrap().set_sync_place("elsewhere.kame");
    assert!(apply(&nb, steps, None).is_empty());
    assert!(a.is_file());
}

#[test]
fn another_recorded_place_is_read_again() {
    let (root, nb) = logged_notebook("stranded-healthy-place");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    Session::open(&a).unwrap().set_sync_place("elsewhere.kame");
    spoil_at_the_same_size(&a);
    let mut healthy = Healthy::default();
    assert!(find_stranded(&nb, &mut healthy).is_empty());

    Session::open(&a)
        .unwrap()
        .set_sync_place(&place_of(&a).unwrap());

    assert_eq!(find_stranded(&nb, &mut healthy), just(&a));
}

/// Rewrites a document's logs as a build from before `Metadata::publish` wrote them. This build still
/// parses each line, and works out another id for it, so that it no longer checks. Says what `state`
/// was called then.
fn age_logs(folder: &Path, state: &str) -> String {
    let mut renamed: Vec<(String, String)> = Vec::new();
    for entry in std::fs::read_dir(folder).unwrap().flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        let mut aged = String::new();
        for line in text.lines() {
            let mut line = line.replace(",\"publish\":false", "");
            for (old, new) in &renamed {
                line = line.replace(old.as_str(), new.as_str());
            }
            let start = line.find("\"revision\":").unwrap() + "\"revision\":".len();
            let end = line.rfind(",\"blocks\":[").unwrap();
            let id = kame_sync::digest(&line.as_bytes()[start..end]);
            let old = line["{\"id\":\"".len()..]
                .split('"')
                .next()
                .unwrap()
                .to_string();
            line = line.replacen(&old, &id, 1);
            renamed.push((old, id));
            aged.push_str(&line);
            aged.push('\n');
        }
        std::fs::write(entry.path(), aged).unwrap();
    }
    renamed
        .into_iter()
        .find(|(old, _)| old == state)
        .map(|(_, new)| new)
        .unwrap()
}

/// Adds a log holding one whole line that can't be checked.
fn add_line_that_cannot_be_checked(path: &Path) {
    let folder = log_folder(path);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("old-0000.log"), "nonsense\n").unwrap();
}

#[test]
fn a_document_last_synced_before_a_field_was_added_is_not_retired_and_heals() {
    let (root, nb) = logged_notebook("stranded-old-format");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let state = Session::open(&a).unwrap().sync_state().unwrap();
    let aged = age_logs(&log_folder(&a), &state);
    Session::open(&a).unwrap().set_sync_state(&aged);

    let mut healthy = Healthy::default();
    assert!(find_stranded(&nb, &mut healthy).is_empty());
    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(a.is_file());

    let mut session = Session::open(&a).unwrap();
    let outcome = sync(&mut session);
    assert!(!outcome.stranded);
    assert!(outcome.written > 0);
    drop(session);
    assert!(a.is_file());
    assert!(find_stranded(&nb, &mut healthy).is_empty());
    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(text_of(&a).contains("Words."));
}

/// It could be anyone's, this document's home included. So this one retirement waits, and says so.
#[test]
fn a_corrupt_line_that_could_be_this_documents_vetoes_its_retirement_and_says_so() {
    let (root, nb) = logged_notebook("stranded-corrupt-veto");
    let (a, z) = (root.join("a.kame"), root.join("z.kame"));
    synced(&a, "Words.");
    remove_logs(&a);
    add_line_that_cannot_be_checked(&z);

    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&a));
    let steps = decide(&nb, just(&a));
    assert_eq!(
        steps.unsettled(),
        Some(&Unsettled {
            waiting: 1,
            because: "z.kame".to_string()
        })
    );
    assert!(apply(&nb, steps, None).is_empty());
    assert!(a.is_file());
}

/// Its own lines are there, with a corrupt line beside them, so that may be where the revision was.
#[test]
fn a_corrupt_line_beside_this_documents_lines_vetoes_its_retirement() {
    let (root, nb) = logged_notebook("stranded-corrupt-same-document");
    let (a, copy) = (root.join("a.kame"), root.join("copy.kame"));
    synced(&a, "Words.");
    let early = Session::open(&a).unwrap().sync_state().unwrap();
    {
        let mut session = Session::open(&a).unwrap();
        session.insert(" More.");
        sync(&mut session);
    }
    // Logs of the same document elsewhere that don't say they hold what it remembers.
    copy_logs(&a, &copy);
    let remembered = Session::open(&a).unwrap().sync_state().unwrap();
    assert_ne!(remembered, early);
    let log = only_log(&copy);
    let lacking: String = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .filter(|line| !line.contains(&remembered))
        .map(|line| format!("{line}\n"))
        .collect();
    std::fs::write(&log, lacking + "nonsense\n").unwrap();
    remove_logs(&a);

    let steps = decide(&nb, just(&a));
    assert_eq!(steps.unsettled().map(|found| found.waiting), Some(1));
    assert!(apply(&nb, steps, None).is_empty());
    assert!(a.is_file());
}

/// What's beside the corrupt line is another document, so it can't be hiding this one's revision.
#[test]
fn a_corrupt_line_in_another_documents_logs_does_not_veto_a_retirement() {
    let (root, nb) = logged_notebook("stranded-corrupt-other");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&a, "Words.");
    synced(&other, "Other words.");
    remove_logs(&a);
    add_line_that_cannot_be_checked(&other);

    let steps = decide(&nb, just(&a));
    assert_eq!(steps.unsettled(), None);
    assert_eq!(apply(&nb, steps, None).len(), 1);
    assert!(root.join(".a.retired.kame").is_file());
}

#[test]
fn a_corrupt_line_elsewhere_does_not_veto_a_move() {
    let (root, nb) = logged_notebook("stranded-corrupt-move");
    let (a, b, z) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("z.kame"),
    );
    synced(&a, "Words.");
    move_logs(&a, &b);
    add_line_that_cannot_be_checked(&z);

    let steps = decide(&nb, just(&a));
    assert_eq!(steps.unsettled(), None);
    assert_eq!(
        apply(&nb, steps, None),
        [Done::Moved {
            from: a.clone(),
            to: b.clone()
        }]
    );
}

#[test]
fn a_corrupt_line_elsewhere_does_not_veto_a_retirement_with_the_document_opened_there() {
    let (root, nb) = logged_notebook("stranded-corrupt-opened");
    let (a, b, z) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("z.kame"),
    );
    synced(&a, "Words.");
    move_logs(&a, &b);
    assert!(build_from_logs(&b).unwrap());
    add_line_that_cannot_be_checked(&z);

    assert!(matches!(
        reconcile(&nb, just(&a), None)[..],
        [Done::Retired {
            opened_there: true,
            ..
        }]
    ));
}

#[test]
fn a_corrupt_line_elsewhere_lets_a_retired_database_come_back() {
    let (root, nb) = logged_notebook("stranded-corrupt-return");
    let (a, b, z) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("z.kame"),
    );
    synced(&a, "Words.");
    let elsewhere = root.join(".carried");
    std::fs::rename(log_folder(&a), &elsewhere).unwrap();
    reconcile(&nb, just(&a), None);
    std::fs::rename(&elsewhere, log_folder(&b)).unwrap();
    add_line_that_cannot_be_checked(&z);

    assert_eq!(reconcile(&nb, &[], None).len(), 1);
    assert!(b.is_file());
}

/// One document in the older format, healed or not, keeps those lines for good, and that says nothing
/// about any other document.
#[test]
fn a_line_in_the_older_format_elsewhere_stops_nothing() {
    let (root, nb) = logged_notebook("stranded-older-elsewhere");
    let (a, b, c, other) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("c.kame"),
        root.join("other.kame"),
    );
    synced(&a, "Goes.");
    synced(&b, "Moves.");
    synced(&other, "Older.");
    let state = Session::open(&other).unwrap().sync_state().unwrap();
    let aged = age_logs(&log_folder(&other), &state);
    Session::open(&other).unwrap().set_sync_state(&aged);
    remove_logs(&a);
    move_logs(&b, &c);

    let steps = decide(&nb, &[a.clone(), b.clone()]);
    assert_eq!(steps.unsettled(), None);
    assert_eq!(apply(&nb, steps, None).len(), 2);
    assert!(root.join(".a.retired.kame").is_file());
    assert!(c.is_file());

    // And still nothing once it has healed.
    sync(&mut Session::open(&other).unwrap());
    let (d, e) = (root.join("d.kame"), root.join("e.kame"));
    synced(&d, "Later.");
    move_logs(&d, &e);
    assert_eq!(reconcile(&nb, just(&d), None).len(), 1);
}

#[test]
fn a_database_parked_by_a_crash_comes_back_with_older_lines_about() {
    let (root, nb) = logged_notebook("stranded-parked-older");
    let (a, b, other) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("other.kame"),
    );
    synced(&a, "Words.");
    synced(&other, "Older.");
    let state = Session::open(&other).unwrap().sync_state().unwrap();
    age_logs(&log_folder(&other), &state);
    let parked = root.join(".a.moving-1.kame");
    std::fs::rename(&a, &parked).unwrap();
    move_logs(&a, &b);

    assert_eq!(
        reconcile(&nb, &[], None),
        [Done::Returned {
            from: parked,
            to: b.clone()
        }]
    );
    assert!(text_of(&b).contains("Words."));
}

#[test]
fn a_retired_database_comes_back_to_logs_in_the_older_format() {
    let (root, nb) = logged_notebook("stranded-return-older");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    let state = Session::open(&a).unwrap().sync_state().unwrap();
    let aged = age_logs(&log_folder(&a), &state);
    Session::open(&a).unwrap().set_sync_state(&aged);
    let elsewhere = root.join(".carried");
    std::fs::rename(log_folder(&a), &elsewhere).unwrap();
    reconcile(&nb, just(&a), None);
    let retired = root.join(".a.retired.kame");
    assert!(retired.is_file());
    std::fs::rename(&elsewhere, log_folder(&b)).unwrap();

    assert_eq!(
        reconcile(&nb, &[], None),
        [Done::Returned {
            from: retired,
            to: b.clone()
        }]
    );
}

/// Its revision is held only in a line that can't be read, so what it holds can't be compared.
#[test]
fn a_retirement_with_its_revision_only_in_an_older_line_counts_as_unsynced() {
    let (root, nb) = logged_notebook("stranded-unsynced-older");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    let state = Session::open(&a).unwrap().sync_state().unwrap();
    let aged = age_logs(&log_folder(&a), &state);
    Session::open(&a).unwrap().set_sync_state(&aged);
    move_logs(&a, &b);
    // A database at the new home that's in step with those logs, as far as it can be told.
    std::fs::copy(&a, &b).unwrap();
    {
        let mut session = Session::open(&b).unwrap();
        session.set_sync_place(&place_of(&b).unwrap());
    }

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

#[test]
fn logs_that_cannot_be_read_or_are_from_a_newer_kame_say_why_a_retirement_waits() {
    let (root, nb) = logged_notebook("stranded-says-why");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&a, "Words.");
    synced(&other, "Other words.");
    remove_logs(&a);
    add_newer_line(&other);
    let unsettled = Unsettled {
        waiting: 1,
        because: "other.kame".to_string(),
    };
    assert_eq!(decide(&nb, just(&a)).unsettled(), Some(&unsettled));

    std::fs::remove_file(log_folder(&other).join("new-0000.log")).unwrap();
    let log = only_log(&other);
    set_mode(&log, 0o000);
    let found = decide(&nb, just(&a)).unsettled().cloned();
    set_mode(&log, 0o644);
    assert_eq!(found, Some(unsettled));
    assert_eq!(decide(&nb, just(&a)).unsettled(), None);
}

#[test]
fn a_folder_that_cannot_be_read_says_why_a_retirement_waits() {
    let (root, nb) = logged_notebook("stranded-says-why-folder");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    let shut = root.join("shut");
    std::fs::create_dir_all(&shut).unwrap();
    set_mode(&shut, 0o000);
    let found = decide(&nb, just(&a)).unsettled().cloned();
    set_mode(&shut, 0o755);
    assert_eq!(
        found,
        Some(Unsettled {
            waiting: 1,
            because: "shut".to_string()
        })
    );
}

/// Nothing has waited, so there's nothing to say, whatever can't be read.
#[test]
fn nothing_is_said_of_logs_that_cannot_be_read_if_nothing_has_waited() {
    let (root, nb) = logged_notebook("stranded-says-nothing");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&a, "Words.");
    synced(&other, "Other words.");
    remove_logs(&a);
    add_newer_line(&other);
    assert_eq!(decide(&nb, &[]).unsettled(), None);
}

#[test]
fn a_move_goes_ahead_while_some_logs_are_from_a_newer_kame() {
    let (root, nb) = logged_notebook("stranded-newer-move");
    let (a, b, other) = (
        root.join("a.kame"),
        root.join("b.kame"),
        root.join("other.kame"),
    );
    synced(&a, "Words.");
    synced(&other, "Other words.");
    move_logs(&a, &b);
    add_newer_line(&other);

    assert_eq!(reconcile(&nb, just(&a), None).len(), 1);
    assert!(b.is_file());
}

#[test]
fn logs_with_a_line_that_cannot_be_checked_are_not_remembered_as_healthy_when_they_lack_it() {
    let (root, nb) = logged_notebook("stranded-dropped-unknown");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    spoil_at_the_same_size(&a);
    add_line_that_cannot_be_checked(&a);
    let mut healthy = Healthy::default();
    assert!(find_stranded(&nb, &mut healthy).is_empty(), "can't be told");

    blank_at_the_same_size(&log_folder(&a).join("old-0000.log"));

    assert_eq!(find_stranded(&nb, &mut healthy), just(&a));
}

/// It's there, whatever the other line was, and logs like these have to be remembered, as a document
/// keeps a line like that for good.
#[test]
fn logs_holding_it_beside_a_line_that_cannot_be_checked_are_remembered_as_healthy() {
    let (root, nb) = logged_notebook("stranded-dropped-held");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let log = only_log(&a);
    add_line_that_cannot_be_checked(&a);
    let mut healthy = Healthy::default();
    assert!(find_stranded(&nb, &mut healthy).is_empty());

    blank_at_the_same_size(&log);

    assert!(
        find_stranded(&nb, &mut healthy).is_empty(),
        "so it wasn't read"
    );
}

/// It can't be told, so it isn't stranded. But what can be read is all another document's, and that
/// a line can't be checked doesn't make them this one's to write to.
#[test]
fn a_line_that_cannot_be_checked_lets_no_history_into_another_documents_logs() {
    let (root, nb) = logged_notebook("stranded-dropped-others");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&a, "Old words.");
    synced(&other, "A different document.");
    remove_logs(&a);
    move_logs(&other, &a);
    std::fs::remove_file(&other).unwrap();
    add_line_that_cannot_be_checked(&a);
    let before = words_in_logs(&log_folder(&a));

    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
    assert!(reconcile(&nb, just(&a), None).is_empty());
    assert!(a.is_file());
    let mut session = Session::open(&a).unwrap();
    let outcome = sync(&mut session);
    assert!(!outcome.stranded);
    assert!(outcome.others_logs);
    assert_eq!(outcome.written, 0);
    drop(session);
    assert_eq!(words_in_logs(&log_folder(&a)), before);
}

/// Half a line isn't a line yet, and a stale document can't be held up by one being written.
#[test]
fn half_a_line_still_being_written_does_not_stop_a_pass() {
    let (root, nb) = logged_notebook("stranded-half-line");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&a, "Words.");
    synced(&other, "Other words.");
    remove_logs(&a);
    let log = only_log(&other);
    let mut bytes = std::fs::read(&log).unwrap();
    bytes.extend_from_slice(b"{\"id\":\"half");
    std::fs::write(&log, bytes).unwrap();

    assert_eq!(reconcile(&nb, just(&a), None).len(), 1);
}

/// Tries to move a database, and then to delete it, and shows that its logs stayed each time.
fn assert_logs_are_kept(nb: &Notebook, path: &Path, refused_for: &str) {
    let logs = log_folder(path);
    let before = words_in_logs(&logs);
    let plan = plan_move(
        nb,
        path,
        &with_suffix(path, "-moved.kame"),
        None,
        DEVICE,
        100,
    );
    let err = carry_out(nb, plan, None, DEVICE, 100).unwrap_err();
    assert!(err.contains(refused_for), "{err}");
    assert!(path.is_file());
    assert_eq!(words_in_logs(&logs), before);

    assert!(delete_document(path).unwrap(), "it says it left the logs");
    assert!(!path.exists());
    assert_eq!(words_in_logs(&logs), before);
}

/// A database at its recorded place, with what's there now another document's, in the older format.
/// Every line parses and fails its check, and each says what it held and whose it is, which isn't
/// what this one remembers, so it's as sure as it is with lines that check.
#[test]
fn another_documents_aged_logs_at_its_recorded_place_leave_it_stranded() {
    let (root, nb) = logged_notebook("stranded-aged-others");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&a, "Old words.");
    synced(&other, "A different document.");
    let state = Session::open(&other).unwrap().sync_state().unwrap();
    remove_logs(&a);
    move_logs(&other, &a);
    std::fs::remove_file(&other).unwrap();
    age_logs(&log_folder(&a), &state);
    assert_eq!(place_in(&a), place_of(&a));
    assert_eq!(Store::load(&log_folder(&a)).unwrap().revision_count(), 0);
    let before = words_in_logs(&log_folder(&a));

    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&a));
    let mut session = Session::open(&a).unwrap();
    session.insert(" Typed here.");
    let outcome = sync(&mut session);
    assert!(outcome.stranded);
    assert_eq!(outcome.written, 0);
    drop(session);
    assert_eq!(words_in_logs(&log_folder(&a)), before);

    assert_logs_are_kept(&nb, &a, "moved or deleted");
}

/// The same with no place, as an older kame leaves it, so it isn't stranded, and they still aren't its.
#[test]
fn another_documents_aged_logs_are_not_taken_by_a_database_with_no_place() {
    let (root, nb) = logged_notebook("stranded-aged-others-no-place");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&a, "Old words.");
    synced(&other, "A different document.");
    let state = Session::open(&other).unwrap().sync_state().unwrap();
    remove_logs(&a);
    move_logs(&other, &a);
    std::fs::remove_file(&other).unwrap();
    age_logs(&log_folder(&a), &state);
    as_an_older_kame_left_it(&a);
    let before = words_in_logs(&log_folder(&a));

    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
    assert!(reconcile(&nb, just(&a), None).is_empty());
    let mut session = Session::open(&a).unwrap();
    session.insert(" Typed here.");
    let outcome = sync(&mut session);
    assert!(outcome.others_logs);
    assert_eq!(outcome.written, 0);
    drop(session);
    assert_eq!(words_in_logs(&log_folder(&a)), before);

    assert_logs_are_kept(&nb, &a, "another document's");
}

/// Its own lines in the older format say they're its own, so its logs still go with it.
#[test]
fn a_documents_own_aged_logs_are_deleted_and_moved_with_it() {
    let (root, nb) = logged_notebook("stranded-aged-own");
    let (a, b) = (root.join("a.kame"), root.join("b.kame"));
    synced(&a, "Words.");
    let state = Session::open(&a).unwrap().sync_state().unwrap();
    let aged = age_logs(&log_folder(&a), &state);
    Session::open(&a).unwrap().set_sync_state(&aged);

    let plan = plan_move(&nb, &a, &b, None, DEVICE, 100);
    let report = carry_out(&nb, plan, None, DEVICE, 100).unwrap();
    assert!(report.left.is_empty(), "{:?}", report.left);
    assert!(b.is_file());
    assert!(log_folder(&b).is_dir());
    assert!(!log_folder(&a).exists());

    assert!(!delete_document(&b).unwrap());
    assert!(!log_folder(&b).exists());
}

/// Lines that don't even parse can't say whose they are, so nothing shows these logs are this one's.
#[test]
fn logs_holding_only_lines_that_name_no_document_are_kept_and_not_written_to() {
    let (root, nb) = logged_notebook("stranded-unnamed");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    remove_logs(&a);
    add_line_that_cannot_be_checked(&a);
    let before = words_in_logs(&log_folder(&a));

    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
    let mut session = Session::open(&a).unwrap();
    session.insert(" Typed here.");
    let outcome = sync(&mut session);
    assert!(outcome.others_logs);
    assert_eq!(outcome.written, 0);
    drop(session);
    assert_eq!(words_in_logs(&log_folder(&a)), before);

    assert_logs_are_kept(&nb, &a, "another document's");
}

/// One line it can't check is no reason to stop, with its own revisions there to say whose they are.
#[test]
fn a_line_that_names_no_document_beside_its_own_lines_changes_nothing() {
    let (root, nb) = logged_notebook("stranded-unnamed-beside-own");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    add_line_that_cannot_be_checked(&a);
    let mut session = Session::open(&a).unwrap();
    session.insert(" More.");
    let outcome = sync(&mut session);
    assert!(!outcome.others_logs);
    assert!(outcome.written > 0);
    drop(session);
    assert!(!delete_document(&a).unwrap());
    assert!(!log_folder(&a).exists());
    assert!(listed(&nb).is_empty());
}

/// It has no history to bring, so its first sync merges with them as ever. Until then it hasn't
/// been shown to be the same document, so those logs aren't its to take away.
#[test]
fn a_database_that_never_synced_keeps_its_hands_off_another_documents_logs() {
    let (root, nb) = logged_notebook("stranded-never-synced-others");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&other, "A different document.");
    move_logs(&other, &a);
    std::fs::remove_file(&other).unwrap();
    Session::open(&a).unwrap().insert("Never synced.");

    let copy = root.join("copy.kame");
    std::fs::copy(&a, &copy).unwrap();
    copy_logs(&a, &copy);
    let mut session = Session::open(&copy).unwrap();
    let outcome = sync(&mut session);
    assert!(!outcome.others_logs);
    assert!(outcome.merged);
    drop(session);

    assert_logs_are_kept(&nb, &a, "another document's");
}

#[test]
fn logs_that_cannot_be_read_are_kept() {
    let (root, _nb) = logged_notebook("stranded-unreadable-kept");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let log = only_log(&a);
    set_mode(&log, 0o000);
    let left = delete_document(&a);
    set_mode(&log, 0o644);
    assert!(left.unwrap(), "it says it left the logs");
    assert!(!a.exists());
    assert!(log.is_file());
}

/// With the file there and unreadable, nothing can show the logs are its. With no file at all,
/// they're the document, and a delete of that row takes them.
#[test]
fn a_database_that_cannot_be_read_leaves_its_logs_and_a_row_of_only_logs_does_not() {
    let (root, _nb) = logged_notebook("stranded-garbage-delete");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    std::fs::write(&a, "not a database").unwrap();
    assert!(delete_document(&a).unwrap());
    assert!(log_folder(&a).is_dir());

    assert!(!delete_document(&a).unwrap());
    assert!(!log_folder(&a).exists());
}

/// The whole of what's on disk before places were recorded is like this, so it isn't left until each
/// document happens to be opened on each machine.
#[test]
fn a_pass_stamps_a_database_with_its_logs_and_losing_them_then_strands_it() {
    let (root, nb) = logged_notebook("stamp-with-logs");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    as_an_older_kame_left_it(&a);

    let steps = decide(&nb, &[]);
    assert_eq!(place_in(&a), None, "deciding changes nothing");
    assert!(
        apply(&nb, steps, None).is_empty(),
        "and there's nothing to say"
    );
    assert_eq!(place_in(&a), place_of(&a));

    remove_logs(&a);
    assert_eq!(find_stranded(&nb, &mut Healthy::default()), just(&a));
}

#[test]
fn a_hand_copy_with_no_logs_beside_it_is_not_stamped() {
    let (root, nb) = logged_notebook("stamp-hand-copy");
    let (a, copy) = (root.join("a.kame"), root.join("copy.kame"));
    synced(&a, "Words.");
    std::fs::copy(&a, &copy).unwrap();
    as_an_older_kame_left_it(&copy);

    reconcile(&nb, &[], None);
    assert_eq!(place_in(&copy), None);
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
}

#[test]
fn a_database_whose_revision_is_not_beside_it_is_not_stamped() {
    let (root, nb) = logged_notebook("stamp-not-beside");
    let (a, other) = (root.join("a.kame"), root.join("other.kame"));
    synced(&a, "Old words.");
    synced(&other, "A different document.");
    remove_logs(&a);
    move_logs(&other, &a);
    std::fs::remove_file(&other).unwrap();
    as_an_older_kame_left_it(&a);

    reconcile(&nb, &[], None);
    assert_eq!(place_in(&a), None);
    assert!(find_stranded(&nb, &mut Healthy::default()).is_empty());
}

#[test]
fn an_older_line_that_could_hold_the_revision_counts_as_beside_it() {
    let (root, nb) = logged_notebook("stamp-older-line");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    let state = Session::open(&a).unwrap().sync_state().unwrap();
    let aged = age_logs(&log_folder(&a), &state);
    Session::open(&a).unwrap().set_sync_state(&aged);
    as_an_older_kame_left_it(&a);

    reconcile(&nb, &[], None);
    assert_eq!(place_in(&a), place_of(&a));
}

#[test]
fn a_database_that_never_synced_is_not_stamped() {
    let (root, nb) = logged_notebook("stamp-never-synced");
    let a = root.join("a.kame");
    Session::open(&a).unwrap().insert("Never synced.");
    reconcile(&nb, &[], None);
    assert_eq!(place_in(&a), None);
}

#[test]
fn the_open_document_is_never_stamped() {
    let (root, nb) = logged_notebook("stamp-open");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    as_an_older_kame_left_it(&a);

    reconcile(&nb, &[], Some(&a));
    assert_eq!(place_in(&a), None);
    reconcile(&nb, &[], None);
    assert_eq!(place_in(&a), place_of(&a));
}

#[test]
fn a_stamp_is_skipped_if_what_it_found_has_changed() {
    let (root, nb) = logged_notebook("stamp-changed");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    as_an_older_kame_left_it(&a);

    let steps = decide(&nb, &[]);
    Session::open(&a).unwrap().set_sync_state("something else");
    apply(&nb, steps, None);
    assert_eq!(place_in(&a), None);

    let (root, nb) = logged_notebook("stamp-changed-place");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    as_an_older_kame_left_it(&a);
    let steps = decide(&nb, &[]);
    Session::open(&a).unwrap().set_sync_place("elsewhere");
    apply(&nb, steps, None);
    assert_eq!(place_in(&a).as_deref(), Some("elsewhere"));
}

/// Then it's as it was, and merely not stranded.
#[test]
fn a_stamp_that_cannot_be_written_is_no_trouble() {
    let (root, nb) = logged_notebook("stamp-unwritable");
    let a = root.join("a.kame");
    synced(&a, "Words.");
    as_an_older_kame_left_it(&a);
    set_mode(&a, 0o444);
    let done = reconcile(&nb, &[], None);
    set_mode(&a, 0o644);
    assert!(done.is_empty());
    assert_eq!(place_in(&a), None);
}
