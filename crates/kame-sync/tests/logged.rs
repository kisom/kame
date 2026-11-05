//! Logs meeting an open document, with the sync tool played by copying log files.

mod common;

use std::path::{Path, PathBuf};

use kame_core::edit::Cursor;
use kame_core::session::Session;
use kame_sync::logs::{
    build_from_logs, device_name, display_device, find_or_build, is_stranded, log_folder,
    logging_root, orphaned_logs, Logged, MARKER,
};
use kame_sync::revlog::Store;

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
    // After Save As the copy has logs of its own, which the original's don't stand in for.
    let talos = Machine {
        path: original.path.with_file_name("copy.kame"),
        device: original.device.clone(),
    };
    there.save_as(&talos.path).unwrap();
    assert_eq!(there.sync_state(), None);
    talos.sync(&mut there, 2);
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

fn missing_folder() -> PathBuf {
    common::test_folder("stranded-missing").join(".gone.kame.sync")
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
    assert!(!is_stranded(None, &missing_folder()));
}

#[test]
fn a_recorded_revision_with_no_logs_is_stranded() {
    assert!(is_stranded(Some("r"), &missing_folder()));
}

#[test]
fn a_recorded_revision_in_the_logs_beside_it_is_not_stranded() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    let state = session.sync_state().expect("a sync leaves its state");
    assert!(!is_stranded(Some(&state), &log_folder(&talos.path)));
}

#[test]
fn logs_that_belong_to_another_document_leave_it_stranded() {
    let (talos, _) = two_machines();
    let mut session = talos.session();
    talos.sync(&mut session, 1);
    let state = session.sync_state().unwrap();
    let other = another_document(&talos, "different.kame");
    assert!(is_stranded(Some(&state), &log_folder(&other)));
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
    let stranded = is_stranded(Some(&state), &folder);
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
    assert!(!is_stranded(Some(&state), &folder));
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
