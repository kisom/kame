//! Files written by every release, kept in `tests/samples/VERSION/`, read by this build. What 1.x
//! writes, every later 1.x reads ([document format, section 7](../../../docs/spec/document-format.md)),
//! so each folder is read as the release that wrote it read it, and its expected reading is what
//! that release saw. The generator, `cargo run -p kame-sync --example write_samples`, is run once
//! per release and never here, and nothing in a folder already there is ever changed.

use std::path::{Path, PathBuf};

use kame_core::edit::Cursor;
use kame_core::export::{to_html_with, to_markdown_with};
use kame_core::model::{Document, Metadata};
use kame_core::references::References;
use kame_core::session::Session;
use serde_json::Value;

/// Where the releases' folders are.
fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples")
}

/// Each release's folder, oldest name first.
fn versions() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(samples())
        .expect("the samples folder is there")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    found.sort();
    found
}

fn version_of(folder: &Path) -> String {
    folder.file_name().unwrap().to_string_lossy().into_owned()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn expected(folder: &Path) -> Value {
    serde_json::from_str(&read(&folder.join("essay.json"))).expect("the expected reading parses")
}

/// A copy of a release's document to open, since opening and undoing write to it.
fn copy_of(folder: &Path) -> PathBuf {
    let version = version_of(folder);
    let into = kame_test_dir::folder(&format!("written-by-{version}"));
    let copy = into.join("essay.kame");
    std::fs::copy(folder.join("essay.kame"), &copy).unwrap();
    copy
}

fn markdown(document: &Document, metadata: &Metadata) -> String {
    to_markdown_with(document, metadata, &References::new())
}

fn opened(path: &Path, version: &str) -> Session {
    let session = Session::open(path).unwrap_or_else(|err| panic!("{version}: {err}"));
    assert!(
        !session.newer && !session.lossy,
        "{version}: this build takes its own release's file for a newer one's"
    );
    assert_eq!(
        session.last_error, None,
        "{version}: it opened with a warning"
    );
    assert!(!session.save_failed, "{version}: it can't be written");
    session
}

#[test]
fn there_are_samples_from_a_release() {
    let found: Vec<String> = versions().iter().map(|folder| version_of(folder)).collect();
    assert!(
        !found.is_empty(),
        "no release has samples in {}",
        samples().display()
    );
}

#[test]
fn every_release_s_document_reads_as_it_did() {
    for folder in versions() {
        let version = version_of(&folder);
        let expected = expected(&folder);
        let session = opened(&copy_of(&folder), &version);

        assert_eq!(
            serde_json::to_value(session.document()).unwrap(),
            expected["document"],
            "{version}: the document"
        );
        assert_eq!(
            serde_json::to_value(&session.metadata).unwrap(),
            expected["metadata"],
            "{version}: the metadata"
        );
        let metadata = session.published_metadata();
        assert_eq!(
            markdown(session.document(), &metadata),
            read(&folder.join("essay.md")),
            "{version}: the Markdown export"
        );
        assert_eq!(
            to_html_with(session.document(), &metadata, &References::new()),
            read(&folder.join("essay.html")),
            "{version}: the HTML export"
        );

        let history = &expected["history"];
        assert_eq!(
            session.history(),
            (
                history["applied"].as_u64().unwrap() as usize,
                history["undone"].as_u64().unwrap() as usize
            ),
            "{version}: the steps applied and waiting to be redone"
        );

        let position = &expected["position"];
        let block = position["block"].as_u64().unwrap() as usize;
        assert_eq!(
            session.caret,
            Cursor {
                block_id: session.document().blocks[block].id,
                offset: position["offset"].as_u64().unwrap() as usize,
            },
            "{version}: where the writer was"
        );
        assert_eq!(
            session.restored_scroll,
            Some(position["scroll"].as_f64().unwrap() as f32),
            "{version}: how far the page was scrolled"
        );
    }
}

#[test]
fn every_release_s_snapshots_read_as_they_did() {
    for folder in versions() {
        let version = version_of(&folder);
        let expected = expected(&folder);
        let mut session = opened(&copy_of(&folder), &version);
        let snapshots = session.snapshots().expect("the snapshots list");
        let wanted = expected["snapshots"].as_array().unwrap();
        assert_eq!(
            snapshots.len(),
            wanted.len(),
            "{version}: how many snapshots"
        );
        let metadata = session.published_metadata();
        for (snapshot, wanted) in snapshots.iter().zip(wanted) {
            assert_eq!(
                snapshot.label.as_deref(),
                wanted["label"].as_str(),
                "{version}: a snapshot's label"
            );
            assert_eq!(
                snapshot.timestamp,
                wanted["timestamp"].as_i64().unwrap(),
                "{version}: when a snapshot was taken"
            );
            let document = session
                .db
                .restore_snapshot(snapshot.id)
                .unwrap_or_else(|err| panic!("{version}: a snapshot doesn't read: {err}"));
            assert_eq!(
                markdown(&document, &metadata),
                wanted["markdown"].as_str().unwrap(),
                "{version}: the snapshot labelled {:?}",
                snapshot.label
            );
        }
    }
}

#[test]
fn every_release_s_history_undoes_and_redoes_whole() {
    for folder in versions() {
        let version = version_of(&folder);
        let expected = expected(&folder);
        let copy = copy_of(&folder);
        let mut session = opened(&copy, &version);
        let head = session.document().clone();
        let (applied, undone) = session.history();

        for step in 0..applied {
            assert!(
                session.undo(),
                "{version}: undoing step {step} back from the head: {:?}",
                session.last_error
            );
        }
        assert_eq!(session.history(), (0, applied + undone));
        for step in 0..applied + undone {
            assert!(
                session.redo(),
                "{version}: redoing step {step}: {:?}",
                session.last_error
            );
        }
        assert_eq!(
            markdown(session.document(), &session.published_metadata()),
            expected["redone"].as_str().unwrap(),
            "{version}: the document with every step redone"
        );
        for step in 0..undone {
            assert!(
                session.undo(),
                "{version}: undoing redone step {step}: {:?}",
                session.last_error
            );
        }
        assert_eq!(session.document(), &head, "{version}: back where it opened");
        drop(session);

        // What this build wrote into the release's file reads back as it was.
        let session = opened(&copy, &version);
        assert_eq!(session.document(), &head, "{version}: reopened");
        assert_eq!(session.history(), (applied, undone), "{version}: reopened");
    }
}
