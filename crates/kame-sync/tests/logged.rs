//! Logs meeting an open document, with the sync tool played by copying log files.

mod common;

use std::path::{Path, PathBuf};

#[path = "shared/age_logs.rs"]
mod aged;
use aged::age_logs;

use kame_core::edit::Cursor;
use kame_core::session::Session;
use kame_core::storage::Db;
use kame_sync::logs::{
    build_from_logs, device_name, display_device, find_or_build, is_stranded, log_folder,
    logging_root, orphaned_logs, place_of, Logged, MARKER, WAITING_LOGS,
};
use kame_sync::revlog::{log_reads_here, Store};

/// A machine's copy of an opted-in folder, holding one document both started from.
struct Machine {
    path: PathBuf,
    device: String,
}

fn two_machines() -> (Machine, Machine) {
    let root = common::test_folder("logged");
    let (talos, titan) = (root.join("talos"), root.join("titan"));
    std::fs::create_dir_all(&talos).unwrap();
    std::fs::create_dir_all(&titan).unwrap();
    std::fs::write(talos.join(MARKER), "").unwrap();
    std::fs::write(titan.join(MARKER), "").unwrap();
    let one = talos.join("essay.kame");
    {
        let mut session = Session::open(&one).unwrap();
        session.insert("A shared opening. It has words.");
        session.split();
        session.insert("A shared ending. It has more.");
    }
    let other = titan.join("essay.kame");
    std::fs::copy(&one, &other).unwrap();
    (
        Machine {
            path: one,
            device: "talos-aaaa".to_string(),
        },
        Machine {
            path: other,
            device: "titan-bbbb".to_string(),
        },
    )
}

impl Machine {
    fn session(&self) -> Session {
        Session::open(&self.path).unwrap()
    }

    fn logged(&self, session: &Session) -> Logged {
        Logged::open(session, &self.device).expect("the folder is opted in")
    }

    fn append(&self, session: &mut Session, block: usize, words: &str) {
        let block_id = session.document().blocks[block].id;
        let offset = session.document().blocks[block].text.len();
        session.set_caret(Cursor { block_id, offset });
        session.insert(words);
    }

    /// Writes down what's here, merging whatever has arrived.
    fn sync(&self, session: &mut Session, clock: i64) -> kame_sync::logs::Outcome {
        self.logged(session).sync(session, clock).unwrap()
    }

    fn receive_from(&self, other: &Machine) {
        let (from, to) = (log_folder(&other.path), log_folder(&self.path));
        std::fs::create_dir_all(&to).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

fn revisions(path: &Path) -> usize {
    Store::load(&log_folder(path)).unwrap().revision_count()
}

#[test]
fn a_documents_logs_are_in_a_hidden_folder_beside_it() {
    assert_eq!(
        log_folder(Path::new("/notes/deep/essay.kame")),
        Path::new("/notes/deep/.essay.kame.sync")
    );
    assert_eq!(
        log_folder(Path::new("essay.kame")),
        Path::new(".essay.kame.sync")
    );
}

#[test]
fn a_document_is_logged_only_under_a_marked_folder() {
    let root = common::test_folder("marker");
    let deep = root.join("a/b");
    std::fs::create_dir_all(&deep).unwrap();
    let file = deep.join("essay.kame");
    assert_eq!(logging_root(&file), None);
    std::fs::write(root.join("a").join(MARKER), "").unwrap();
    assert_eq!(
        logging_root(&file).map(|found| found.canonicalize().unwrap()),
        Some(root.join("a").canonicalize().unwrap())
    );
    let session = Session::open(&root.join("outside.kame")).unwrap();
    assert!(Logged::open(&session, "talos-aaaa").is_none());
}

#[test]
fn logs_with_no_marked_folder_are_reported_since_the_document_may_have_stopped_moving() {
    let root = common::test_folder("orphan");
    let file = root.join("essay.kame");
    assert!(!orphaned_logs(&file));
    std::fs::create_dir_all(log_folder(&file)).unwrap();
    assert!(orphaned_logs(&file));
    std::fs::write(root.join(MARKER), "").unwrap();
    assert!(!orphaned_logs(&file));
}

#[test]
fn an_edit_reaches_the_other_machine_as_one_undo_step_and_undoing_it_sticks() {
    let (talos, titan) = two_machines();
    let mut there = talos.session();
    talos.append(&mut there, 0, " From talos.");
    talos.sync(&mut there, 1);

    let mut here = titan.session();
    titan.sync(&mut here, 2);
    titan.receive_from(&talos);
    let outcome = titan.sync(&mut here, 3);
    assert!(outcome.merged, "the words should have arrived");
    assert_eq!(outcome.from.as_deref(), Some("talos"));
    assert!(here.document().blocks[0].text.ends_with(" From talos."));

    assert!(here.undo());
    assert!(!here.document().blocks[0].text.contains("From talos"));
    let outcome = titan.sync(&mut here, 4);
    assert!(!outcome.merged);
    assert!(
        !here.document().blocks[0].text.contains("From talos"),
        "undoing a merge is the next revision, so the words stay gone"
    );
}

#[test]
fn syncing_with_nothing_new_writes_nothing() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    let before = revisions(&talos.path);
    let outcome = talos.sync(&mut session, 2);
    assert_eq!(outcome.written, 0);
    assert!(!outcome.merged);
    assert_eq!(revisions(&talos.path), before);
}

#[test]
fn the_last_revision_survives_closing_the_document() {
    let (talos, _) = two_machines();
    {
        let mut session = talos.session();
        talos.append(&mut session, 0, " One.");
        talos.sync(&mut session, 1);
    }
    let after_first = revisions(&talos.path);
    let mut session = talos.session();
    talos.append(&mut session, 0, " Two.");
    talos.sync(&mut session, 2);
    let store = Store::load(&log_folder(&talos.path)).unwrap();
    assert_eq!(store.revision_count(), after_first + 1);
    assert_eq!(store.heads().len(), 1, "the second builds on the first");
}

#[test]
fn two_machines_starting_from_the_same_text_add_no_notes() {
    let (talos, titan) = two_machines();
    let (mut there, mut here) = (talos.session(), titan.session());
    talos.sync(&mut there, 1);
    titan.sync(&mut here, 2);
    talos.receive_from(&titan);
    titan.receive_from(&talos);
    assert_eq!(talos.sync(&mut there, 3).notes, 0);
    assert_eq!(titan.sync(&mut here, 4).notes, 0);
    assert_eq!(there.document(), here.document());
    assert!(there.document().annotations.is_empty());
}

#[test]
fn edits_to_different_paragraphs_merge_and_both_machines_converge() {
    let (talos, titan) = two_machines();
    let (mut there, mut here) = (talos.session(), titan.session());
    talos.sync(&mut there, 1);
    titan.receive_from(&talos);
    titan.sync(&mut here, 2);
    talos.receive_from(&titan);
    talos.sync(&mut there, 3);

    talos.append(&mut there, 0, " Talos adds this.");
    titan.append(&mut here, 1, " Titan adds that.");
    talos.sync(&mut there, 4);
    titan.sync(&mut here, 5);
    titan.receive_from(&talos);
    talos.receive_from(&titan);
    talos.sync(&mut there, 6);
    titan.sync(&mut here, 7);
    assert!(there.document().blocks[0]
        .text
        .ends_with("Talos adds this."));
    assert!(there.document().blocks[1]
        .text
        .ends_with("Titan adds that."));
    assert_eq!(there.document(), here.document());
    assert!(there.document().annotations.is_empty());
}

#[test]
fn a_conflict_leaves_a_note_after_a_snapshot_and_loses_no_words() {
    let (talos, titan) = two_machines();
    let (mut there, mut here) = (talos.session(), titan.session());
    talos.sync(&mut there, 1);
    titan.receive_from(&talos);
    titan.sync(&mut here, 2);
    talos.receive_from(&titan);
    talos.sync(&mut there, 3);

    talos.append(&mut there, 0, " Mine.");
    titan.append(&mut here, 0, " Yours.");
    talos.sync(&mut there, 4);
    titan.sync(&mut here, 5);
    titan.receive_from(&talos);
    let outcome = titan.sync(&mut here, 6);
    assert_eq!(outcome.notes, 1);
    let snapshots = here.snapshots().unwrap();
    assert_eq!(
        snapshots.len(),
        1,
        "a snapshot first, as a merge adds a note"
    );
    let all: String = here
        .document()
        .blocks
        .iter()
        .map(|block| block.text.clone())
        .chain(here.document().annotations.iter().map(|n| n.body.clone()))
        .collect();
    assert!(all.contains("Mine."), "{all}");
    assert!(all.contains("Yours."), "{all}");
}

#[test]
fn the_caret_keeps_its_place_when_words_arrive_before_it() {
    let (talos, titan) = two_machines();
    let (mut there, mut here) = (talos.session(), titan.session());
    talos.sync(&mut there, 1);
    titan.receive_from(&talos);
    titan.sync(&mut here, 2);

    let block_id = there.document().blocks[0].id;
    there.set_caret(Cursor {
        block_id,
        offset: 0,
    });
    there.insert("New start. ");
    talos.sync(&mut there, 3);

    let word = "It has";
    let at = here.document().blocks[0].text.find(word).unwrap();
    here.set_caret(Cursor {
        block_id,
        offset: at,
    });
    titan.receive_from(&talos);
    titan.sync(&mut here, 4);
    let text = &here.document().blocks[0].text;
    assert!(text.starts_with("New start. "));
    assert_eq!(here.caret.offset, text.find(word).unwrap());
}

#[test]
fn document_properties_travel_and_merge_field_by_field() {
    let (talos, titan) = two_machines();
    let (mut there, mut here) = (talos.session(), titan.session());
    talos.sync(&mut there, 1);
    titan.receive_from(&talos);
    titan.sync(&mut here, 2);
    talos.receive_from(&titan);
    talos.sync(&mut there, 3);

    let mut metadata = there.metadata.clone();
    metadata.title = "From talos".to_string();
    metadata.words.push("talosword".to_string());
    there.set_metadata(metadata);
    let mut metadata = here.metadata.clone();
    metadata.author = "From titan".to_string();
    metadata.words.push("titanword".to_string());
    here.set_metadata(metadata);

    talos.sync(&mut there, 4);
    titan.sync(&mut here, 5);
    titan.receive_from(&talos);
    talos.receive_from(&titan);
    talos.sync(&mut there, 6);
    titan.sync(&mut here, 7);
    for session in [&there, &here] {
        assert_eq!(session.metadata.title, "From talos");
        assert_eq!(session.metadata.author, "From titan");
        assert!(session.metadata.words.contains(&"talosword".to_string()));
        assert!(session.metadata.words.contains(&"titanword".to_string()));
    }
    assert_eq!(there.metadata, here.metadata);
}

#[test]
fn news_is_only_what_arrived_since_the_last_read() {
    let (talos, titan) = two_machines();
    let (mut there, mut here) = (talos.session(), titan.session());
    talos.append(&mut there, 0, " One.");
    talos.sync(&mut there, 1);
    let mut logged = titan.logged(&here);
    logged.sync(&mut here, 2).unwrap();
    assert!(!logged.has_news());
    titan.receive_from(&talos);
    assert!(logged.has_news());
    logged.sync(&mut here, 3).unwrap();
    assert!(!logged.has_news());
}

#[test]
fn a_document_that_cannot_save_is_left_alone() {
    let (talos, titan) = two_machines();
    let mut there = talos.session();
    talos.append(&mut there, 0, " One.");
    talos.sync(&mut there, 1);
    let mut here = titan.session();
    titan.sync(&mut here, 2);
    titan.receive_from(&talos);
    here.save_failed = true;
    let before = here.document().clone();
    let outcome = titan.sync(&mut here, 3);
    assert_eq!(here.document(), &before);
    assert_eq!(outcome.written, 0);
    assert!(!outcome.merged);
}

#[test]
fn a_device_name_is_the_host_and_a_few_characters_and_shows_as_the_host() {
    let name = device_name("Titan.local");
    let (host, tail) = name.rsplit_once('-').unwrap();
    assert_eq!(host, "titan");
    assert_eq!(tail.len(), 4);
    assert_ne!(device_name("titan"), device_name("titan"));
    assert_eq!(display_device(&name), "titan");
    assert_eq!(display_device("my-host-3f9a"), "my-host");
    assert_eq!(display_device("plain"), "plain");
    assert_eq!(
        display_device("host-name"),
        "host-name",
        "not four hex digits"
    );
    let odd = device_name("a b/c");
    assert!(
        odd.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "{odd}"
    );
    assert!(device_name("").starts_with("machine-"));
}

#[test]
fn machines_sharing_a_past_share_a_root_so_an_edit_before_the_first_sync_is_no_conflict() {
    let (talos, titan) = two_machines();
    let (mut there, mut here) = (talos.session(), titan.session());
    talos.append(&mut there, 0, " Edited before opting in.");
    talos.sync(&mut there, 1);
    titan.append(&mut here, 1, " Also edited first.");
    titan.sync(&mut here, 2);
    titan.receive_from(&talos);
    talos.receive_from(&titan);
    assert_eq!(titan.sync(&mut here, 3).notes, 0);
    assert_eq!(talos.sync(&mut there, 4).notes, 0);
    assert_eq!(there.document(), here.document());
    assert!(there.document().blocks[0]
        .text
        .ends_with("Edited before opting in."));
    assert!(there.document().blocks[1]
        .text
        .ends_with("Also edited first."));
}

#[test]
fn histories_that_do_not_match_keep_every_word_in_notes() {
    let (talos, titan) = two_machines();
    {
        // Compacting joins titan's history differently from talos's, as a separate past would.
        let mut session = titan.session();
        session.replace_document({
            let mut doc = session.document().clone();
            doc.blocks[0].text = "Rewritten by titan alone.".to_string();
            doc
        });
    }
    std::fs::remove_file(&titan.path).unwrap();
    {
        let mut session = Session::open(&titan.path).unwrap();
        let mut doc = talos.session().document().clone();
        doc.blocks[0].text = "Rewritten by titan alone.".to_string();
        session.replace_document(doc);
    }
    let (mut there, mut here) = (talos.session(), titan.session());
    talos.sync(&mut there, 1);
    titan.sync(&mut here, 2);
    titan.receive_from(&talos);
    titan.sync(&mut here, 3);
    let all: String = here
        .document()
        .blocks
        .iter()
        .map(|block| block.text.clone())
        .chain(here.document().annotations.iter().map(|n| n.body.clone()))
        .collect();
    assert!(all.contains("Rewritten by titan alone."), "{all}");
    assert!(all.contains("A shared opening."), "{all}");
}

#[test]
fn a_fresh_machine_builds_the_document_from_its_logs_with_nothing_lost() {
    let (talos, titan) = two_machines();
    let mut there = talos.session();
    talos.append(&mut there, 0, " Written on talos.");
    let mut metadata = there.metadata.clone();
    metadata.title = "The title".to_string();
    there.set_metadata(metadata);
    talos.sync(&mut there, 1);

    std::fs::remove_file(&titan.path).unwrap();
    titan.receive_from(&talos);
    assert!(build_from_logs(&titan.path).unwrap());
    assert!(titan.path.is_file());

    let mut here = titan.session();
    assert_eq!(here.document(), there.document());
    assert_eq!(here.metadata.title, "The title");
    let outcome = titan.sync(&mut here, 2);
    assert_eq!(outcome.written, 0, "it already stands at that revision");
    assert!(!outcome.merged);
    titan.append(&mut here, 1, " And on titan.");
    titan.sync(&mut here, 3);
    talos.receive_from(&titan);
    talos.sync(&mut there, 4);
    assert!(there.document().blocks[1].text.ends_with("And on titan."));
}

#[test]
fn nothing_is_built_without_logs_or_a_free_path() {
    let root = common::test_folder("build");
    let file = root.join("essay.kame");
    assert!(!build_from_logs(&file).unwrap(), "no logs");
    std::fs::write(root.join(MARKER), "").unwrap();
    assert!(!build_from_logs(&file).unwrap(), "no logs");
    assert!(!file.exists());
    std::fs::create_dir_all(log_folder(&file)).unwrap();
    std::fs::write(log_folder(&file).join("a.log"), "half a line").unwrap();
    assert!(!build_from_logs(&file).unwrap(), "nothing whole in them");
    assert!(!file.exists());
    std::fs::write(&file, "already here").unwrap();
    assert!(!build_from_logs(&file).unwrap());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "already here");
}

/// What's left at a document's old name after it was moved on one machine while another edited it:
/// the other machine's log, whose revisions build on a log that went with the move. Opened, an
/// empty document would be made there, and its first sync would send it to every machine, so it
/// isn't built or opened until what it builds on arrives.
#[test]
fn a_document_with_only_logs_that_all_wait_on_a_missing_log_is_refused() {
    let (talos, titan) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    talos.append(&mut session, 0, " Written offline.");
    titan.logged(&session).sync(&mut session, 2).unwrap();
    drop(session);
    let (talos_log, titan_log) = (
        log_folder(&talos.path).join("talos-aaaa.log"),
        log_folder(&talos.path).join("titan-bbbb.log"),
    );
    assert!(talos_log.is_file() && titan_log.is_file());

    let left = talos.path.with_file_name("left.kame");
    let folder = log_folder(&left);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::copy(&titan_log, folder.join("titan-bbbb.log")).unwrap();
    let store = Store::load(&folder).unwrap();
    assert!(store.revision_count() > 0 && store.heads().is_empty());
    let before = std::fs::read(folder.join("titan-bbbb.log")).unwrap();

    let err = build_from_logs(&left).unwrap_err().to_string();
    assert_eq!(err, WAITING_LOGS);
    assert!(!left.exists());
    assert!(!left.with_file_name("left.kame.building").exists());
    assert_eq!(names_in(&folder), ["titan-bbbb.log"]);
    assert_eq!(
        std::fs::read(folder.join("titan-bbbb.log")).unwrap(),
        before
    );
    assert_eq!(
        find_or_build("left", &talos.path).unwrap_err().to_string(),
        WAITING_LOGS
    );
    assert!(!left.exists());

    // What it builds on arriving, it's built.
    std::fs::copy(&talos_log, folder.join("talos-aaaa.log")).unwrap();
    assert!(build_from_logs(&left).unwrap());
    let session = Session::open(&left).unwrap();
    assert!(session.document().blocks[0]
        .text
        .ends_with(" Written offline."));
}

/// An empty folder holds no document to wait for, and nothing is built, as ever.
#[test]
fn a_document_with_only_an_empty_log_folder_builds_nothing_and_is_not_refused() {
    let (talos, _) = two_machines();
    let left = talos.path.with_file_name("left.kame");
    std::fs::create_dir_all(log_folder(&left)).unwrap();
    assert!(!build_from_logs(&left).unwrap());
    assert!(!left.exists());
}

fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_long_shared_history_gives_a_near_ancestor_so_one_sentence_each_is_no_conflict() {
    let (talos, titan) = two_machines();
    {
        let mut session = talos.session();
        for at in 0..130 {
            talos.append(&mut session, 0, &format!(" Sentence {at} is here."));
        }
    }
    std::fs::copy(&talos.path, &titan.path).unwrap();
    let (mut there, mut here) = (talos.session(), titan.session());
    // Each changes a different sentence of one paragraph, well after the last checkpoint.
    let edit = |session: &mut Session, sentence: &str, words: &str| {
        let block_id = session.document().blocks[0].id;
        let offset = session.document().blocks[0].text.find(sentence).unwrap();
        session.set_caret(Cursor { block_id, offset });
        session.insert(words);
    };
    edit(&mut there, "Sentence 3 is", "Talos says ");
    edit(&mut here, "Sentence 120 is", "Titan says ");
    talos.sync(&mut there, 1);
    titan.sync(&mut here, 2);
    titan.receive_from(&talos);
    talos.receive_from(&titan);
    assert_eq!(titan.sync(&mut here, 3).notes, 0);
    assert_eq!(talos.sync(&mut there, 4).notes, 0);
    assert_eq!(there.document(), here.document());
    let text = &there.document().blocks[0].text;
    assert!(text.contains("Talos says Sentence 3"), "{text}");
    assert!(text.contains("Titan says Sentence 120"), "{text}");
}

#[test]
fn a_sync_is_only_needed_for_an_edit_new_properties_or_news() {
    let (talos, titan) = two_machines();
    let (mut there, mut here) = (talos.session(), titan.session());
    let mut logged = titan.logged(&here);
    assert!(logged.needs_sync(&here), "nothing is known yet");
    logged.sync(&mut here, 1).unwrap();
    assert!(!logged.needs_sync(&here));

    titan.append(&mut here, 0, " An edit.");
    assert!(logged.needs_sync(&here));
    logged.sync(&mut here, 2).unwrap();
    assert!(!logged.needs_sync(&here));

    let mut metadata = here.metadata.clone();
    metadata.title = "New".to_string();
    here.set_metadata(metadata);
    assert!(logged.needs_sync(&here));
    logged.sync(&mut here, 3).unwrap();
    assert!(!logged.needs_sync(&here));

    talos.sync(&mut there, 4);
    titan.receive_from(&talos);
    assert!(logged.needs_sync(&here));
    logged.sync(&mut here, 5).unwrap();
    assert!(!logged.needs_sync(&here));
}

#[test]
fn a_link_to_a_document_with_only_logs_builds_it() {
    let (talos, titan) = two_machines();
    let mut there = talos.session();
    talos.sync(&mut there, 1);
    std::fs::remove_file(&titan.path).unwrap();
    titan.receive_from(&talos);
    let found = find_or_build("essay", &titan.path.with_file_name("linking.kame")).unwrap();
    assert_eq!(found, Some(titan.path.clone()));
    assert!(titan.path.is_file());
    let none = find_or_build("nothing", &titan.path).unwrap();
    assert_eq!(none, None);
}

#[test]
fn a_remembered_revision_missing_from_the_logs_starts_from_the_history_again() {
    let (original, titan) = two_machines();
    let mut there = original.session();
    original.sync(&mut there, 1);
    let checkpoints = revisions(&original.path);
    assert!(checkpoints > 1);
    drop(there);
    // A copy has logs of its own, which the original's don't stand in for.
    let talos = Machine {
        path: original.path.with_file_name("copy.kame"),
        device: original.device.clone(),
    };
    std::fs::copy(&original.path, &talos.path).unwrap();
    let mut there = talos.session();
    assert!(there.sync_state().is_some());
    assert!(!talos.sync(&mut there, 2).stranded);
    assert_eq!(
        revisions(&talos.path),
        checkpoints,
        "the same checkpoints again"
    );

    let mut here = titan.session();
    titan.sync(&mut here, 3);
    titan.receive_from(&talos);
    assert_eq!(titan.sync(&mut here, 4).notes, 0);
}

/// Save As leaves nothing remembered, and its logs start the same way.
#[test]
fn a_save_as_starts_from_the_history_again() {
    let (original, _) = two_machines();
    let mut there = original.session();
    original.sync(&mut there, 1);
    let checkpoints = revisions(&original.path);
    let copy = Machine {
        path: original.path.with_file_name("copy.kame"),
        device: original.device.clone(),
    };
    there.save_as(&copy.path, None, |_, _| false).unwrap();
    assert_eq!(there.sync_state(), None);
    assert_eq!(there.sync_place(), None);
    copy.sync(&mut there, 2);
    assert_eq!(revisions(&copy.path), checkpoints);
    assert_eq!(there.sync_place(), place_of(&copy.path));
}

/// The main use: left open on one machine while writing on another.
#[test]
fn an_idle_copy_following_along_does_not_pile_up_undo_steps_or_grow() {
    let (talos, titan) = two_machines();
    let mut idle = talos.session();
    talos.sync(&mut idle, 1);
    titan.receive_from(&talos);
    let mut writer = titan.session();
    titan.sync(&mut writer, 2);
    talos.receive_from(&titan);
    talos.sync(&mut idle, 3);
    let start = idle.history().0;
    let filler =
        "A long paragraph of words that are here to give the document some weight. ".repeat(40);
    titan.append(&mut writer, 0, &filler);
    titan.sync(&mut writer, 4);
    talos.receive_from(&titan);
    talos.sync(&mut idle, 5);
    let after_one = std::fs::metadata(&talos.path).unwrap().len();
    for at in 0..60 {
        titan.append(&mut writer, 1, &format!(" Sentence {at}."));
        titan.sync(&mut writer, 6 + at);
        talos.receive_from(&titan);
        talos.sync(&mut idle, 106 + at);
    }
    assert_eq!(idle.document(), writer.document(), "it has followed along");
    assert_eq!(idle.history().0, start + 1, "one undo step for all of it");
    drop(idle);
    let grown = std::fs::metadata(&talos.path).unwrap().len();
    eprintln!("after one {after_one}, after sixty more {grown}");
    assert!(grown < after_one * 3, "{after_one} grew to {grown}");
    let idle = talos.session();
    assert_eq!(
        idle.document(),
        writer.document(),
        "and reads back the same"
    );
}

#[test]
fn undo_takes_an_idle_copy_back_before_everything_that_arrived_and_an_edit_ends_the_run() {
    let (talos, titan) = two_machines();
    let mut idle = talos.session();
    talos.sync(&mut idle, 1);
    titan.receive_from(&talos);
    let mut writer = titan.session();
    titan.sync(&mut writer, 2);
    talos.receive_from(&titan);
    talos.sync(&mut idle, 3);
    let before = idle.document().clone();
    let arrive = |writer: &mut Session, idle: &mut Session, words: &str, at: i64| {
        titan.append(writer, 0, words);
        titan.sync(writer, at);
        talos.receive_from(&titan);
        talos.sync(idle, at + 1);
    };
    arrive(&mut writer, &mut idle, " One.", 10);
    arrive(&mut writer, &mut idle, " Two.", 20);
    assert!(idle.undo());
    assert_eq!(idle.document(), &before);
    assert!(idle.redo());
    assert!(idle.document().blocks[0].text.ends_with(" One. Two."));

    // An edit here ends the run, so the next arrival can be undone apart from it.
    talos.append(&mut idle, 1, " Here.");
    talos.sync(&mut idle, 30);
    titan.receive_from(&talos);
    titan.sync(&mut writer, 31);
    arrive(&mut writer, &mut idle, " Three.", 40);
    let with_three = idle.document().clone();
    assert!(with_three.blocks[0].text.ends_with("Three."));
    assert!(idle.undo());
    assert!(!idle.document().blocks[0].text.contains("Three"));
    assert!(idle.document().blocks[1].text.ends_with(" Here."));
}

/// Whether a database at `path` that last synced there, and remembers `state`, is stranded.
fn stranded_there(path: &Path, state: &str) -> bool {
    is_stranded(path, Some(state), place_of(path).as_deref())
}

/// A path in a marked folder, with no logs.
fn path_with_no_logs() -> PathBuf {
    let root = common::test_folder("stranded-missing");
    std::fs::write(root.join(MARKER), "").unwrap();
    root.join("gone.kame")
}

/// A second document for the same machine, with logs of its own.
fn another_document(machine: &Machine, name: &str) -> PathBuf {
    let path = machine.path.with_file_name(name);
    let mut session = Session::open(&path).unwrap();
    session.insert("Nothing like the other one.");
    machine.sync(&mut session, 2);
    path
}

#[test]
fn a_database_without_a_sync_state_is_never_stranded() {
    let path = path_with_no_logs();
    assert!(!is_stranded(&path, None, place_of(&path).as_deref()));
}

#[test]
fn a_recorded_revision_with_no_logs_is_stranded() {
    assert!(stranded_there(&path_with_no_logs(), "r"));
}

#[test]
fn a_recorded_revision_with_no_logs_and_no_place_or_another_is_not_stranded() {
    let path = path_with_no_logs();
    assert!(!is_stranded(&path, Some("r"), None));
    assert!(!is_stranded(&path, Some("r"), Some("elsewhere.kame")));
}

#[test]
fn nothing_is_stranded_with_nothing_above_it_turning_logs_on() {
    let path = common::test_folder("stranded-no-marker").join("a.kame");
    assert!(!is_stranded(&path, Some("r"), Some("a.kame")));
}

#[test]
fn a_recorded_revision_in_the_logs_beside_it_is_not_stranded() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    let state = session.sync_state().expect("a sync leaves its state");
    assert!(!stranded_there(&talos.path, &state));
}

#[test]
fn logs_that_belong_to_another_document_leave_it_stranded() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    let state = session.sync_state().unwrap();
    let other = another_document(&talos, "different.kame");
    assert!(stranded_there(&other, &state));
}

#[test]
fn logs_that_cannot_be_read_do_not_count_as_stranded() {
    use std::os::unix::fs::PermissionsExt;
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    let state = session.sync_state().unwrap();
    let folder = log_folder(&talos.path);
    let logs: Vec<PathBuf> = std::fs::read_dir(&folder)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert!(!logs.is_empty());
    for log in &logs {
        std::fs::set_permissions(log, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let stranded = stranded_there(&talos.path, &state);
    for log in &logs {
        std::fs::set_permissions(log, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert!(!stranded, "nothing is done on a guess");
}

#[test]
fn logs_from_a_newer_version_do_not_count_as_stranded() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    let state = session.sync_state().unwrap();
    let folder = log_folder(&talos.path);
    // The same lines, as a newer kame would write them: with a kind this one doesn't know.
    let mut newer = String::new();
    for entry in std::fs::read_dir(&folder).unwrap().flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        std::fs::remove_file(entry.path()).unwrap();
        for line in text
            .lines()
            .filter(|line| line.contains("\"kind\":\"Paragraph\""))
        {
            newer.push_str(&line.replace("\"kind\":\"Paragraph\"", "\"kind\":\"Table\""));
            newer.push('\n');
        }
    }
    assert!(newer.contains("Table"));
    std::fs::write(folder.join("future-cccc.log"), newer).unwrap();
    let store = Store::load(&folder).unwrap();
    assert!(store.holds_newer());
    assert!(!store.contains(&state));
    assert!(!stranded_there(&talos.path, &state));
}

/// Titan with no database, only talos's logs as a newer kame would have written them, every line
/// holding a kind this build doesn't know, so that none of their revisions is one it can use.
fn only_a_newer_kame_s_logs() -> (Machine, Vec<(PathBuf, String)>) {
    let (talos, titan) = two_machines();
    let mut session = talos.session();
    let mut metadata = session.metadata.clone();
    metadata.author = "Kyle".to_string();
    metadata.publish = true;
    session.set_metadata(metadata);
    talos.sync(&mut session, 1);
    drop(session);
    std::fs::remove_file(&titan.path).unwrap();
    let folder = log_folder(&titan.path);
    std::fs::create_dir_all(&folder).unwrap();
    for entry in std::fs::read_dir(log_folder(&talos.path))
        .unwrap()
        .flatten()
    {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        let newer = text.replace(
            "\"kind\":\"Paragraph\"",
            "\"kind\":{\"Table\":{\"rows\":2}}",
        );
        std::fs::write(folder.join(entry.file_name()), newer).unwrap();
    }
    let store = Store::load(&folder).unwrap();
    assert!(store.holds_newer());
    assert!(store.heads().is_empty(), "no revision this build can use");
    let logs = logs_in(&folder);
    (titan, logs)
}

/// Every file in a log folder, with what it holds.
fn logs_in(folder: &Path) -> Vec<(PathBuf, String)> {
    let mut logs: Vec<(PathBuf, String)> = std::fs::read_dir(folder)
        .unwrap()
        .flatten()
        .map(|entry| {
            let text = std::fs::read_to_string(entry.path()).unwrap();
            (entry.path(), text)
        })
        .collect();
    logs.sort();
    logs
}

/// A document with only logs, none of which this build can use since a newer kame wrote them, isn't
/// made again as an empty document, which its first sync would send back as the latest revision of
/// a document of that name. Building or following a link to it is refused, saying to update kame,
/// and nothing is written at its path or to its logs.
#[test]
fn a_document_with_only_a_newer_kame_s_logs_is_neither_built_nor_made_again() {
    let (titan, logs) = only_a_newer_kame_s_logs();
    let said = build_from_logs(&titan.path)
        .expect_err("there's nothing here to build it from")
        .to_string();
    assert!(said.contains("newer version of kame"), "{said}");
    assert!(said.contains("until kame is updated"), "{said}");
    let linking = titan.path.with_file_name("other.kame");
    let followed = find_or_build("essay", &linking).expect_err("a link to it is refused");
    assert_eq!(followed.to_string(), said);
    assert!(!titan.path.exists());
    let building = titan.path.with_file_name("essay.kame.building");
    assert!(!building.exists(), "nothing half-built is left");
    assert_eq!(logs_in(&log_folder(&titan.path)), logs);
}

/// A newer machine's revision holding a block of a kind this build doesn't know. Its line can't be
/// read here, so nothing of it is merged into the document, lossily or otherwise: what was typed
/// here is written down, and the document goes read-only, as it was.
#[test]
fn a_revision_with_a_kind_this_build_does_not_know_is_not_merged_and_stops_the_document() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    let mut logged = talos.logged(&session);
    logged.sync(&mut session, 1).unwrap();
    let folder = log_folder(&talos.path);
    let mut newer = String::new();
    for entry in std::fs::read_dir(&folder).unwrap().flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        for line in text
            .lines()
            .filter(|line| line.contains("A shared ending."))
        {
            let line = line
                .replace(
                    "\"kind\":\"Paragraph\"",
                    "\"kind\":{\"Table\":{\"rows\":2}}",
                )
                .replace("\"device\":\"talos-aaaa\"", "\"device\":\"future-cccc\"")
                .replace("A shared ending.", "A newer ending.");
            newer.push_str(&line);
            newer.push('\n');
        }
    }
    assert!(newer.contains("Table"));
    std::fs::write(folder.join("future-cccc.log"), newer).unwrap();

    talos.append(&mut session, 0, " Typed here.");
    let before = session.document().clone();
    let outcome = logged.sync(&mut session, 2).unwrap();
    assert!(!outcome.merged);
    assert!(session.newer, "read-only until kame is updated");
    assert!(!session.lossy, "what's on the screen is all its own");
    assert_eq!(session.document(), &before);
    let store = Store::load(&folder).unwrap();
    let heads = store.heads();
    assert_eq!(
        heads.len(),
        1,
        "the newer revision isn't one this build can use"
    );
    assert_eq!(
        store.document(&heads[0]),
        before,
        "what was typed is in the logs"
    );
}

/// A revision as a newer kame writes one, on `parent`, of `doc` with its first block saying `text`:
/// `block_extra` in that block and `metadata_extra` in its metadata, each `,"name":value` or empty,
/// with the block's digest and the revision's id taken over what it wrote, as that kame takes them.
fn line_from_a_newer_kame(
    doc: &kame_core::model::Document,
    parent: &str,
    text: &str,
    block_extra: &str,
    metadata_extra: &str,
) -> String {
    let with = |json: String, extra: &str| format!("{}{extra}}}", &json[..json.len() - 1]);
    let (mut blocks, mut order) = (Vec::new(), Vec::new());
    for (at, block) in doc.blocks.iter().enumerate() {
        let mut block = block.clone();
        let json = if at == 0 {
            block.text = text.to_string();
            with(serde_json::to_string(&block).unwrap(), block_extra)
        } else {
            serde_json::to_string(&block).unwrap()
        };
        order.push(format!("\"{}\"", kame_sync::digest(json.as_bytes())));
        blocks.push(json);
    }
    let metadata = with(
        serde_json::to_string(&kame_core::model::Metadata::default()).unwrap(),
        metadata_extra,
    );
    let revision = format!(
        r#"{{"parents":["{parent}"],"device":"future-cccc","when":3,"document":"{}","title":{},"order":[{}],"annotations":{},"metadata":{metadata}}}"#,
        doc.id,
        serde_json::to_string(&doc.title).unwrap(),
        order.join(","),
        serde_json::to_string(&doc.annotations).unwrap(),
    );
    let id = kame_sync::digest(revision.as_bytes());
    format!(
        "{{\"id\":\"{id}\",\"revision\":{revision},\"blocks\":[{}]}}\n",
        blocks.join(",")
    )
}

/// A newer machine's revision with a field this build doesn't know, in a block or in its metadata.
/// This build reads it without the field, so its digest isn't the one the newer kame wrote: with the
/// field in a block, the revision waits on a block it never finds, and with it in metadata, the line
/// fails its check. That's the rule, not a fault: what was typed here goes in the logs, nothing of
/// the newer revision is merged, and the document goes read-only, saying to update kame.
#[test]
fn a_revision_with_a_field_this_build_does_not_know_is_not_merged_and_stops_the_document() {
    // Without the field it's a line this build would have written, and it merges.
    let (talos, _) = two_machines();
    let mut session = talos.session();
    let mut logged = talos.logged(&session);
    logged.sync(&mut session, 1).unwrap();
    let last = session.sync_state().unwrap();
    let line = line_from_a_newer_kame(session.document(), &last, "A newer opening.", "", "");
    std::fs::write(log_folder(&talos.path).join("future-cccc.log"), line).unwrap();
    assert!(logged.sync(&mut session, 2).unwrap().merged);
    assert_eq!(session.document().blocks[0].text, "A newer opening.");
    assert!(!session.newer);

    for (case, block_extra, metadata_extra) in [
        ("block", r#","future":true"#, ""),
        ("metadata", "", r#","future":1"#),
    ] {
        let (talos, _) = two_machines();
        let mut session = talos.session();
        let mut logged = talos.logged(&session);
        logged.sync(&mut session, 1).unwrap();
        let last = session.sync_state().unwrap();
        let folder = log_folder(&talos.path);
        std::fs::write(
            folder.join("future-cccc.log"),
            line_from_a_newer_kame(
                session.document(),
                &last,
                "A newer opening.",
                block_extra,
                metadata_extra,
            ),
        )
        .unwrap();

        talos.append(&mut session, 1, " Typed here.");
        let typed = session.document().clone();
        let outcome = logged.sync(&mut session, 2).unwrap();

        assert!(!outcome.merged, "{case}");
        assert!(outcome.written > 0, "{case}: what was typed went out");
        assert!(session.newer, "{case}: read-only until kame is updated");
        assert!(
            !session.lossy,
            "{case}: what's on the screen is all its own"
        );
        let said = session.last_error.clone().unwrap_or_default();
        assert!(said.contains("newer version of kame"), "{case}: {said}");
        assert!(said.contains("until kame is updated"), "{case}: {said}");
        assert_eq!(session.document(), &typed, "{case}");
        let store = Store::load(&folder).unwrap();
        assert!(store.holds_newer(), "{case}");
        let heads = store.heads();
        assert_eq!(heads, vec![session.sync_state().unwrap()], "{case}");
        assert_eq!(store.document(&heads[0]), typed, "{case}");
        assert!(!session.insert("Refused."), "{case}");
    }
}

#[test]
fn logs_going_while_open_stop_all_writing() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    let mut logged = talos.logged(&session);
    logged.sync(&mut session, 1).unwrap();
    let folder = log_folder(&talos.path);
    std::fs::remove_dir_all(&folder).unwrap();
    talos.append(&mut session, 0, " More words.");
    let outcome = logged.sync(&mut session, 2).unwrap();
    assert!(outcome.stranded);
    assert_eq!(outcome.written, 0);
    assert!(!folder.exists(), "nothing was written back");
}

#[test]
fn opening_stranded_writes_nothing() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    drop(session);
    let folder = log_folder(&talos.path);
    std::fs::remove_dir_all(&folder).unwrap();
    let mut session = talos.session();
    talos.append(&mut session, 0, " More words.");
    let outcome = talos.sync(&mut session, 2);
    assert!(outcome.stranded);
    assert!(!folder.exists());
}

#[test]
fn opening_stranded_with_another_documents_logs_writes_nothing() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    drop(session);
    let folder = log_folder(&talos.path);
    std::fs::remove_dir_all(&folder).unwrap();
    let other = another_document(&talos, "different.kame");
    std::fs::rename(log_folder(&other), &folder).unwrap();
    let before = Store::load(&folder).unwrap().revision_count();

    let mut session = talos.session();
    talos.append(&mut session, 0, " Old words.");
    let outcome = talos.sync(&mut session, 3);

    assert!(outcome.stranded);
    assert_eq!(Store::load(&folder).unwrap().revision_count(), before);
}

#[test]
fn logs_replaced_while_open_stop_all_writing() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    let mut logged = talos.logged(&session);
    logged.sync(&mut session, 1).unwrap();
    let folder = log_folder(&talos.path);
    std::fs::remove_dir_all(&folder).unwrap();
    // Another document's logs take the place.
    let other = another_document(&talos, "different.kame");
    std::fs::rename(log_folder(&other), &folder).unwrap();
    let before = Store::load(&folder).unwrap().revision_count();

    talos.append(&mut session, 0, " Old words.");
    let outcome = logged.sync(&mut session, 3).unwrap();
    assert!(outcome.stranded);
    let store = Store::load(&folder).unwrap();
    assert_eq!(store.revision_count(), before, "nothing was added");
    for head in store.heads() {
        for block in store.document(&head).blocks {
            assert!(!block.text.contains("Old words") && !block.text.contains("shared opening"));
        }
    }
}

#[test]
fn a_first_sync_is_not_stranded() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    assert_eq!(session.sync_state(), None);
    let outcome = talos.sync(&mut session, 1);
    assert!(!outcome.stranded);
    assert!(revisions(&talos.path) > 0);
}

#[test]
fn a_place_is_the_path_from_the_logging_roots_folder() {
    let root = common::test_folder("place").join("essays");
    let deep = root.join("a/b");
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(place_of(&deep.join("essay.kame")), None);
    std::fs::write(root.join(MARKER), "").unwrap();
    assert_eq!(
        place_of(&deep.join("essay.kame")).as_deref(),
        Some("essays/a/b/essay.kame")
    );
    assert_eq!(
        place_of(&root.join("essay.kame")).as_deref(),
        Some("essays/essay.kame")
    );
    assert_eq!(
        place_of(&deep.join("../essay.kame")).as_deref(),
        Some("essays/a/essay.kame")
    );
    assert_eq!(
        place_of(&root.join("a/../essay.kame")).as_deref(),
        Some("essays/essay.kame"),
        "the root's name, however it's reached"
    );
    std::fs::write(deep.join(MARKER), "").unwrap();
    assert_eq!(
        place_of(&deep.join("essay.kame")).as_deref(),
        Some("b/essay.kame"),
        "the nearest marker"
    );
}

#[test]
fn a_sync_and_a_build_record_the_place() {
    let (talos, titan) = two_machines();
    let mut there = talos.session();
    assert_eq!(there.sync_place(), None);
    talos.sync(&mut there, 1);
    assert_eq!(there.sync_place(), place_of(&talos.path));

    std::fs::remove_file(&titan.path).unwrap();
    titan.receive_from(&talos);
    assert!(build_from_logs(&titan.path).unwrap());
    assert_eq!(titan.session().sync_place(), place_of(&titan.path));
}

/// As an older kame leaves one.
fn forget_the_place(path: &Path) {
    let state = Db::open_read_only(path).unwrap().load_sync_state().unwrap();
    let mut db = Db::open(path).unwrap();
    db.clear_sync_state().unwrap();
    db.save_sync_state(&state).unwrap();
}

/// Synced, and left as an older kame leaves one, with its logs put aside, where they can be brought
/// back from.
fn with_no_place_and_its_logs_aside(talos: &Machine) -> PathBuf {
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    drop(session);
    forget_the_place(&talos.path);
    let aside = talos.path.with_file_name("aside");
    std::fs::rename(log_folder(&talos.path), &aside).unwrap();
    aside
}

/// It may have been moved or deleted on another machine, and with no place, that can't be told
/// from a copy made by hand. Its history written here would bring it back on every machine, so
/// nothing is written. Its logs coming back beside it let it sync, and it records its place.
#[test]
fn a_database_with_no_place_and_no_logs_writes_nothing() {
    let (talos, _) = two_machines();
    let aside = with_no_place_and_its_logs_aside(&talos);
    let folder = log_folder(&talos.path);

    let mut session = talos.session();
    let mut logged = talos.logged(&session);
    let outcome = logged.sync(&mut session, 2).unwrap();
    assert!(outcome.unplaced, "{outcome:?}");
    assert!(!outcome.stranded && !outcome.others_logs, "{outcome:?}");
    assert_eq!(outcome.written, 0);
    assert!(!folder.exists());
    assert_eq!(session.sync_place(), None);
    talos.append(&mut session, 0, " Typed since.");
    assert!(logged.sync(&mut session, 3).unwrap().unplaced);
    assert!(!folder.exists());

    std::fs::rename(&aside, &folder).unwrap();
    let outcome = logged.sync(&mut session, 4).unwrap();
    assert!(
        !outcome.unplaced && !outcome.stranded && !outcome.others_logs,
        "{outcome:?}"
    );
    assert!(outcome.written > 0);
    assert_eq!(session.sync_place(), place_of(&talos.path));
}

/// A folder with no line in it, or only one torn by a crash, holds nothing either.
#[test]
fn a_database_with_no_place_beside_logs_holding_nothing_writes_nothing() {
    for torn in [false, true] {
        let (talos, _) = two_machines();
        with_no_place_and_its_logs_aside(&talos);
        let folder = log_folder(&talos.path);
        std::fs::create_dir_all(&folder).unwrap();
        if torn {
            std::fs::write(folder.join("talos-aaaa.log"), "{\"id\":\"half").unwrap();
        }
        let before = Store::log_sizes(&folder);

        let mut session = talos.session();
        let outcome = talos.sync(&mut session, 2);
        assert!(outcome.unplaced, "torn: {torn}: {outcome:?}");
        assert_eq!(outcome.written, 0, "torn: {torn}");
        assert_eq!(Store::log_sizes(&folder), before, "torn: {torn}");
        assert_eq!(session.sync_place(), None, "torn: {torn}");
    }
}

/// Once it has synced here, it's somewhere it can be left behind.
#[test]
fn logs_going_after_a_first_sync_at_a_new_place_stop_the_writing() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    drop(session);
    let copy = Machine {
        path: talos.path.with_file_name("copy.kame"),
        device: talos.device.clone(),
    };
    std::fs::copy(&talos.path, &copy.path).unwrap();

    let mut session = copy.session();
    let mut logged = copy.logged(&session);
    assert!(!logged.sync(&mut session, 2).unwrap().stranded);
    let folder = log_folder(&copy.path);
    std::fs::remove_dir_all(&folder).unwrap();
    copy.append(&mut session, 0, " More words.");
    assert!(logged.sync(&mut session, 3).unwrap().stranded);
    assert!(!folder.exists());
}

fn hand_copy_on_another_documents_logs(talos: &Machine) -> (Machine, PathBuf) {
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    drop(session);
    let other = another_document(talos, "different.kame");
    std::fs::remove_file(&other).unwrap();
    std::fs::copy(&talos.path, &other).unwrap();
    let folder = log_folder(&other);
    (
        Machine {
            path: other,
            device: talos.device.clone(),
        },
        folder,
    )
}

/// Not stranded, since it never synced here, and these logs still aren't its.
#[test]
fn a_hand_copy_on_another_documents_logs_writes_nothing() {
    let (talos, _) = two_machines();
    let (copy, folder) = hand_copy_on_another_documents_logs(&talos);
    let before = revisions(&copy.path);

    let mut session = copy.session();
    copy.append(&mut session, 0, " Typed in the copy.");
    let outcome = copy.sync(&mut session, 3);

    assert!(!outcome.stranded);
    assert!(outcome.others_logs);
    assert_eq!(outcome.written, 0);
    assert!(!outcome.merged);
    assert_eq!(revisions(&copy.path), before);
    assert_eq!(session.sync_place(), place_of(&talos.path), "as it was");
    let store = Store::load(&folder).unwrap();
    for head in store.heads() {
        assert!(!store.document(&head).blocks[0].text.contains("shared"));
    }
}

/// The same with no place at all, as a database last synced by an older kame has.
#[test]
fn a_database_with_no_place_on_another_documents_logs_writes_nothing() {
    let (talos, _) = two_machines();
    let (copy, _) = hand_copy_on_another_documents_logs(&talos);
    forget_the_place(&copy.path);
    let before = revisions(&copy.path);

    let mut session = copy.session();
    let outcome = copy.sync(&mut session, 3);

    assert!(!outcome.stranded);
    assert!(outcome.others_logs);
    assert_eq!(revisions(&copy.path), before);
}

/// Logs of the same document have nothing foreign to bring in, as when another machine started
/// them again.
#[test]
fn a_database_with_no_place_on_logs_of_its_own_document_merges_with_them() {
    let (talos, titan) = two_machines();
    let mut there = talos.session();
    talos.append(&mut there, 0, " From talos.");
    talos.sync(&mut there, 1);
    let mut here = titan.session();
    titan.append(&mut here, 1, " From titan.");
    titan.sync(&mut here, 2);
    drop(here);
    forget_the_place(&titan.path);
    std::fs::remove_dir_all(log_folder(&titan.path)).unwrap();
    titan.receive_from(&talos);

    let mut here = titan.session();
    let outcome = titan.sync(&mut here, 3);
    assert!(!outcome.stranded);
    assert!(!outcome.others_logs);
    assert_eq!(outcome.notes, 0);
    assert!(here.document().blocks[0].text.ends_with(" From talos."));
    assert!(here.document().blocks[1].text.ends_with(" From titan."));
}

/// What's remembered may be in a line this build can't check, so nothing can be said. It carries on,
/// and its history starts the logs again.
#[test]
fn a_document_last_synced_before_a_field_was_added_is_not_stranded_and_heals() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    let state = session.sync_state().unwrap();
    let folder = log_folder(&talos.path);
    let aged = age_logs(&folder, &state);
    session.set_sync_state(&aged);
    drop(session);

    let store = Store::load(&folder).unwrap();
    assert!(!store.corrupt_lines());
    assert!(!store.holds_newer());
    assert!(!store.contains(&aged));
    assert!(store.could_hold(&aged));
    assert!(!stranded_there(&talos.path, &aged));

    let mut session = talos.session();
    talos.append(&mut session, 0, " Edited after the upgrade.");
    let outcome = talos.sync(&mut session, 2);
    assert!(!outcome.stranded);
    assert!(outcome.written > 0);
    assert!(!session.newer);
    let store = Store::load(&folder).unwrap();
    assert!(store.contains(&session.sync_state().unwrap()));
    assert!(store
        .heads()
        .iter()
        .any(|head| store.document(head).blocks[0]
            .text
            .ends_with(" Edited after the upgrade.")));
}

#[test]
fn half_a_line_says_nothing_so_a_revision_missing_beside_it_is_missing() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    let state = session.sync_state().unwrap();
    let folder = log_folder(&talos.path);
    for entry in std::fs::read_dir(&folder).unwrap().flatten() {
        std::fs::write(entry.path(), "{\"id\":\"half").unwrap();
    }
    assert!(stranded_there(&talos.path, &state));
}

/// It says what it held, and that isn't what's remembered.
#[test]
fn another_documents_aged_logs_leave_a_database_at_its_place_stranded() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    let state = session.sync_state().unwrap();
    drop(session);
    let other = another_document(&talos, "different.kame");
    let other_state = Session::open(&other).unwrap().sync_state().unwrap();
    let folder = log_folder(&talos.path);
    std::fs::remove_dir_all(&folder).unwrap();
    std::fs::rename(log_folder(&other), &folder).unwrap();
    age_logs(&folder, &other_state);
    let before = std::fs::read_dir(&folder).unwrap().count();
    assert!(stranded_there(&talos.path, &state));

    let mut session = talos.session();
    assert!(talos.sync(&mut session, 3).stranded);
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), before);
}

/// Syncs a document held off its logs again and again, as the app's checks do, and then once more
/// after a log turns up beside it, which is read. Until then nothing is read, whatever is typed.
fn held_off_reads_only_what_changes(
    machine: &Machine,
    session: &mut Session,
    logged: &mut Logged,
    held_off: impl Fn(&kame_sync::logs::Outcome) -> bool,
) {
    let folder = log_folder(&machine.path);
    let before = Store::load(&folder).unwrap().revision_count();
    machine.append(session, 0, " Typed.");
    assert!(held_off(&logged.sync(session, 10).unwrap()));
    let reads = log_reads_here();
    for clock in 11..14 {
        machine.append(session, 0, " More.");
        let outcome = logged.sync(session, clock).unwrap();
        assert!(held_off(&outcome));
        assert_eq!(outcome.written, 0);
    }
    assert_eq!(log_reads_here(), reads, "nothing was read");

    let log = std::fs::read_dir(&folder).unwrap().next().unwrap().unwrap();
    std::fs::copy(log.path(), folder.join("arrived-cccc.log")).unwrap();
    assert!(held_off(&logged.sync(session, 14).unwrap()));
    assert!(log_reads_here() > reads, "what changed was read");
    assert_eq!(Store::load(&folder).unwrap().revision_count(), before);
}

#[test]
fn a_document_whose_logs_were_replaced_while_open_reads_them_only_when_they_change() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    let mut logged = talos.logged(&session);
    logged.sync(&mut session, 1).unwrap();
    let folder = log_folder(&talos.path);
    std::fs::remove_dir_all(&folder).unwrap();
    let other = another_document(&talos, "different.kame");
    std::fs::rename(log_folder(&other), &folder).unwrap();

    held_off_reads_only_what_changes(&talos, &mut session, &mut logged, |outcome| {
        outcome.stranded
    });
}

#[test]
fn a_document_opened_stranded_reads_its_logs_only_when_they_change() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    drop(session);
    let folder = log_folder(&talos.path);
    std::fs::remove_dir_all(&folder).unwrap();
    let other = another_document(&talos, "different.kame");
    std::fs::rename(log_folder(&other), &folder).unwrap();

    let mut session = talos.session();
    let mut logged = talos.logged(&session);
    held_off_reads_only_what_changes(&talos, &mut session, &mut logged, |outcome| {
        outcome.stranded
    });
}

#[test]
fn a_document_on_another_documents_logs_reads_them_only_when_they_change() {
    let (talos, _) = two_machines();
    let (copy, _) = hand_copy_on_another_documents_logs(&talos);
    let mut session = copy.session();
    let mut logged = copy.logged(&session);
    held_off_reads_only_what_changes(&copy, &mut session, &mut logged, |outcome| {
        outcome.others_logs && !outcome.stranded
    });
}

/// Two machines with nothing in common yet, each about to make `name` as a link to it would.
fn two_machines_without_it(name: &str) -> (Machine, Machine) {
    let root = common::test_folder("linked");
    let (talos, titan) = (root.join("talos"), root.join("titan"));
    for folder in [&talos, &titan] {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::write(folder.join(MARKER), "").unwrap();
    }
    (
        Machine {
            path: talos.join(name),
            device: "talos-aaaa".to_string(),
        },
        Machine {
            path: titan.join(name),
            device: "titan-bbbb".to_string(),
        },
    )
}

/// Both machines follow a link to a document that isn't there yet, so each makes one with an id of
/// its own, and each types and syncs before the other's logs arrive. Once they meet, they merge, keep
/// both sides' words, take one id, and say what happened, once.
#[test]
fn two_documents_made_from_one_link_on_two_machines_merge_and_say_so_once() {
    let (talos, titan) = two_machines_without_it("linked.kame");
    let mut there = talos.session();
    there.insert("Written on talos.");
    let mut here = titan.session();
    here.insert("Written on titan.");
    assert_ne!(there.document().id, here.document().id);
    talos.sync(&mut there, 1);
    titan.sync(&mut here, 2);

    titan.receive_from(&talos);
    talos.receive_from(&titan);
    let on_talos = talos.sync(&mut there, 3);
    let on_titan = titan.sync(&mut here, 4);

    for (outcome, from) in [(&on_talos, "titan"), (&on_titan, "talos")] {
        assert!(outcome.merged, "{outcome:?}");
        assert!(outcome.another_document, "{outcome:?}");
        assert_eq!(outcome.from.as_deref(), Some(from), "{outcome:?}");
        assert!(!outcome.others_logs, "{outcome:?}");
    }
    assert_eq!(there.document(), here.document(), "they converged");
    let words: Vec<&str> = here
        .document()
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect();
    assert!(words.contains(&"Written on talos."), "{words:?}");
    assert!(words.contains(&"Written on titan."), "{words:?}");

    titan.append(&mut here, 0, " More.");
    titan.sync(&mut here, 5);
    talos.receive_from(&titan);
    titan.receive_from(&talos);
    let on_talos = talos.sync(&mut there, 6);
    let on_titan = titan.sync(&mut here, 7);
    assert!(on_talos.merged, "the new words came");
    assert!(!on_talos.another_document, "once is enough: {on_talos:?}");
    assert!(!on_titan.another_document, "{on_titan:?}");
    assert_eq!(there.document(), here.document());
}

/// Undoing the merge on the machine whose id it changed brings that id back, with the words from
/// before. That's another revision, which the other machine follows, as for any undone merge. Both
/// ids are in what each has seen, so nobody is told of another document again, and both go on.
#[test]
fn undoing_a_merge_with_another_document_keeps_both_syncing() {
    let (talos, titan) = two_machines_without_it("linked.kame");
    let mut there = talos.session();
    there.insert("Written on talos.");
    let mut here = titan.session();
    here.insert("Written on titan.");
    let talos_id = there.document().id;
    talos.sync(&mut there, 1);
    titan.sync(&mut here, 2);
    titan.receive_from(&talos);
    talos.receive_from(&titan);
    talos.sync(&mut there, 3);
    titan.sync(&mut here, 4);
    assert_ne!(there.document().id, talos_id, "the later one's id won");

    assert!(there.undo());
    assert_eq!(there.document().id, talos_id);
    let on_talos = talos.sync(&mut there, 5);
    titan.receive_from(&talos);
    let on_titan = titan.sync(&mut here, 6);
    for outcome in [&on_talos, &on_titan] {
        assert!(!outcome.others_logs && !outcome.stranded, "{outcome:?}");
        assert!(!outcome.another_document, "{outcome:?}");
    }
    assert!(on_titan.merged, "it followed the undo");
    assert_eq!(there.document(), here.document());

    titan.append(&mut here, 0, " Still syncing.");
    titan.sync(&mut here, 7);
    talos.receive_from(&titan);
    let on_talos = talos.sync(&mut there, 8);
    assert!(
        on_talos.merged && !on_talos.another_document,
        "{on_talos:?}"
    );
    assert_eq!(there.document(), here.document());
}

/// A document deleted and made again at its name, with the old one's log come back beside the new
/// one's. They merge, so the old words come back into the new document, all of them, and it says so.
#[test]
fn another_documents_log_beside_its_own_merges_and_says_so() {
    let (talos, _) = two_machines();
    let mut old = talos.session();
    talos.append(&mut old, 0, " The old one's words.");
    talos.sync(&mut old, 5);
    drop(old);
    let new = Machine {
        path: another_document(&talos, "different.kame"),
        device: "titan-bbbb".to_string(),
    };
    std::fs::copy(
        log_folder(&talos.path).join("talos-aaaa.log"),
        log_folder(&new.path).join("old-cccc.log"),
    )
    .unwrap();

    let mut session = new.session();
    new.append(&mut session, 0, " Typed here.");
    let outcome = new.sync(&mut session, 9);

    assert!(outcome.merged, "{outcome:?}");
    assert!(outcome.another_document, "{outcome:?}");
    assert_eq!(outcome.from.as_deref(), Some("talos"));
    let text: String = session
        .document()
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Typed here."), "{text}");
    assert!(text.contains("The old one's words."), "{text}");

    assert!(!new.sync(&mut session, 10).another_document, "once");
}

/// What a sync held off is remembered only while the logs are as they were.
#[test]
fn a_document_whose_logs_come_back_syncs_again() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    let mut logged = talos.logged(&session);
    logged.sync(&mut session, 1).unwrap();
    let folder = log_folder(&talos.path);
    let away = folder.with_file_name("away");
    std::fs::rename(&folder, &away).unwrap();
    talos.append(&mut session, 0, " Typed while they were away.");
    assert!(logged.sync(&mut session, 2).unwrap().stranded);
    assert!(logged.sync(&mut session, 3).unwrap().stranded);

    std::fs::rename(&away, &folder).unwrap();
    let outcome = logged.sync(&mut session, 4).unwrap();
    assert!(!outcome.stranded);
    assert!(outcome.written > 0);
}

/// Talos sees titan's words arrive, with the reading of every log that takes done on another
/// thread, and the sync going by what it found.
#[test]
fn a_sync_with_news_goes_by_a_check_read_on_another_thread() {
    let (talos, titan) = two_machines();
    let mut here = talos.session();
    let mut logged = talos.logged(&here);
    logged.sync(&mut here, 1).unwrap();
    assert!(logged.stranded_check().is_none(), "nothing has arrived");
    titan.receive_from(&talos);
    let mut there = titan.session();
    titan.append(&mut there, 0, " From titan.");
    titan.sync(&mut there, 2);
    talos.receive_from(&titan);

    let check = logged.stranded_check().expect("words may have arrived");
    let answer = std::thread::spawn(move || check.run()).join().unwrap();
    logged.know(answer);
    assert!(logged.stranded_check().is_none(), "it's known");
    let reads = log_reads_here();
    let outcome = logged.sync(&mut here, 3).unwrap();

    assert!(outcome.merged);
    assert!(here.document().blocks[0].text.ends_with(" From titan."));
    assert_eq!(log_reads_here() - reads, 1, "only to bring in what arrived");
}

#[test]
fn a_check_from_before_the_logs_changed_again_is_not_gone_by() {
    let (talos, titan) = two_machines();
    let mut here = talos.session();
    let mut logged = talos.logged(&here);
    logged.sync(&mut here, 1).unwrap();
    titan.receive_from(&talos);
    let mut there = titan.session();
    titan.append(&mut there, 0, " From titan.");
    titan.sync(&mut there, 2);
    talos.receive_from(&titan);
    let answer = logged.stranded_check().unwrap().run();

    titan.append(&mut there, 0, " And more.");
    titan.sync(&mut there, 3);
    talos.receive_from(&titan);
    logged.know(answer);
    assert!(
        logged.stranded_check().is_some(),
        "what it rested on has changed"
    );
    let reads = log_reads_here();
    let outcome = logged.sync(&mut here, 4).unwrap();

    assert!(outcome.merged);
    assert!(here.document().blocks[0].text.ends_with(" And more."));
    assert_eq!(log_reads_here() - reads, 2, "it read every log itself");
}

#[test]
fn a_check_finding_the_logs_gone_holds_a_sync_off() {
    let (talos, _) = two_machines();
    let mut here = talos.session();
    let mut logged = talos.logged(&here);
    logged.sync(&mut here, 1).unwrap();
    let folder = log_folder(&talos.path);
    std::fs::remove_dir_all(&folder).unwrap();
    let other = another_document(&talos, "different.kame");
    std::fs::rename(log_folder(&other), &folder).unwrap();
    let before = Store::load(&folder).unwrap().revision_count();

    logged.know(logged.stranded_check().unwrap().run());
    talos.append(&mut here, 0, " Old words.");
    let reads = log_reads_here();
    assert!(logged.sync(&mut here, 2).unwrap().stranded);

    assert_eq!(log_reads_here(), reads);
    assert_eq!(Store::load(&folder).unwrap().revision_count(), before);
    assert!(logged.stranded_check().is_none(), "it's held off");
}

/// A damaged document, already in the logs as it is in the file, as a build that didn't fit offsets
/// wrote it. Both are read fitted, so they're the same, and opening it here writes nothing.
#[test]
fn opening_a_damaged_document_already_in_the_logs_writes_no_revision() {
    use kame_core::model::{Anchor, Annotation, AnnotationKind, Document, Span, SpanStyle};
    use kame_sync::revlog::Replica;
    let root = common::test_folder("damaged-logged");
    std::fs::write(root.join(MARKER), "").unwrap();
    let path = root.join("essay.kame");
    let mut doc = Document::new("Damaged");
    let block = &mut doc.blocks[0];
    block.text = "Café au lait.".to_string();
    block.spans = vec![Span {
        start: 6,
        end: 40,
        style: SpanStyle::Bold,
    }];
    doc.annotations = vec![Annotation {
        id: uuid::Uuid::new_v4(),
        kind: AnnotationKind::Note,
        anchor: Anchor::range(doc.blocks[0].id, 4, 8),
        body: "a note".to_string(),
    }];
    let metadata = kame_core::model::Metadata {
        title: "Damaged".to_string(),
        ..Default::default()
    };
    let mut db = Db::open(&path).unwrap();
    db.save_document(&doc).unwrap();
    db.save_metadata(&metadata).unwrap();
    drop(db);
    let older = Replica {
        folder: log_folder(&path),
        device: "older-cccc".to_string(),
    };
    let mut store = Store::load(&older.folder).unwrap();
    older.sync_in(&mut store, &doc, &metadata, None, 1).unwrap();
    older.flush(&mut store).unwrap();
    let logs_before = std::fs::read_dir(log_folder(&path)).unwrap().count();

    let mut session = Session::open(&path).unwrap();
    let outcome = Machine {
        path: path.clone(),
        device: "talos-aaaa".to_string(),
    }
    .sync(&mut session, 2);

    assert_eq!(outcome.written, 0);
    assert!(!outcome.merged);
    assert_eq!(revisions(&path), 1);
    assert_eq!(
        std::fs::read_dir(log_folder(&path)).unwrap().count(),
        logs_before,
        "no log of this machine's"
    );
}

/// An undo that brings back a note as an older history recorded it, starting inside "é", isn't
/// fitted until the document is next read. Writing it down isn't a merge, and nothing arrived.
#[test]
fn an_undo_that_brings_back_a_damaged_note_is_not_taken_for_a_merge() {
    use kame_core::edit::Op;
    use kame_core::model::{Anchor, Annotation, AnnotationKind, Document};
    let root = common::test_folder("damaged-undo");
    std::fs::write(root.join(MARKER), "").unwrap();
    let path = root.join("essay.kame");
    let mut doc = Document::new("Damaged");
    doc.blocks[0].text = "Café au lait.".to_string();
    let taken = Op::RemoveAnnotation {
        annotation: Annotation {
            id: uuid::Uuid::new_v4(),
            kind: AnnotationKind::Note,
            anchor: Anchor::range(doc.blocks[0].id, 4, 8),
            body: "a note".to_string(),
        },
    };
    Db::open(&path).unwrap().record_edit(&doc, &taken).unwrap();
    let machine = Machine {
        path: path.clone(),
        device: "talos-aaaa".to_string(),
    };
    let mut session = Session::open(&path).unwrap();
    let mut logged = machine.logged(&session);
    logged.sync(&mut session, 1).unwrap();

    assert!(session.undo());
    let outcome = logged.sync(&mut session, 2).unwrap();

    assert_eq!(outcome.written, 1);
    assert!(!outcome.merged);
    assert!(session.redo(), "the undo is still the last step");
    assert_eq!(logged.sync(&mut session, 3).unwrap().written, 1);
}
