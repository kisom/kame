//! Revision logs written by every release, kept with its other samples in
//! `crates/kame-core/tests/samples/VERSION/logged/`, read and merged by this build. The logs hold a
//! merge already and end in two heads, one from each machine, which this build merges as the
//! release that wrote them did ([merging, section 6.9](../../../docs/spec/merging.md)). The
//! generator, `cargo run -p kame-sync --example write_samples`, is run once per release and never
//! here, and nothing in a folder already there is ever changed.

use std::path::{Path, PathBuf};

use kame_core::export::to_markdown_with;
use kame_core::references::References;
use kame_core::session::Session;
use kame_sync::logs::{build_from_logs, log_folder, Logged};
use kame_sync::revlog::Store;
use serde_json::Value;

/// The device this build syncs as, which no release's logs name.
const READER: &str = "reader-cccc";

fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../kame-core/tests/samples")
}

/// Each release's folder of logs, oldest name first.
fn releases() -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(samples())
        .expect("the samples folder is there")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .map(|path| {
            let version = path.file_name().unwrap().to_string_lossy().into_owned();
            (version, path)
        })
        .collect();
    found.sort();
    found
}

fn copy_folder(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let into = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_folder(&entry.path(), &into);
        } else {
            std::fs::copy(entry.path(), into).unwrap();
        }
    }
}

/// A copy of a release's logged folder, since syncing writes to it, and where its document goes.
fn copy_of(folder: &Path, version: &str) -> PathBuf {
    let into = kame_test_dir::folder(&format!("logged-by-{version}"));
    copy_folder(&folder.join("logged"), &into);
    into.join("essay.kame")
}

fn expected(folder: &Path) -> Value {
    let path = folder.join("logged.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    serde_json::from_str(&text).expect("the expected reading parses")
}

fn markdown(session: &Session) -> String {
    to_markdown_with(
        session.document(),
        &session.published_metadata(),
        &References::new(),
    )
}

#[test]
fn every_release_s_logs_read_as_they_did() {
    let found = releases();
    assert!(!found.is_empty(), "no release has samples");
    for (version, folder) in found {
        let expected = expected(&folder);
        let path = copy_of(&folder, &version);
        let store = Store::load(&log_folder(&path)).unwrap();
        assert!(
            !store.holds_newer(),
            "{version}: this build takes its own release's logs for a newer one's"
        );
        assert!(!store.corrupt_lines(), "{version}: a line doesn't read");
        let heads: Vec<&str> = expected["heads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|head| head["id"].as_str().unwrap())
            .collect();
        assert_eq!(store.heads(), heads, "{version}: the heads");
        for head in expected["heads"].as_array().unwrap() {
            let id = head["id"].as_str().unwrap();
            assert_eq!(
                serde_json::to_value(store.document(id)).unwrap(),
                head["document"],
                "{version}: the head {id}"
            );
            assert_eq!(
                serde_json::to_value(store.metadata(id)).unwrap(),
                head["metadata"],
                "{version}: the head {id}'s metadata"
            );
        }
    }
}

#[test]
fn every_release_s_logs_merge_as_they_did() {
    for (version, folder) in releases() {
        let expected = expected(&folder);
        let path = copy_of(&folder, &version);
        assert!(
            build_from_logs(&path).unwrap(),
            "{version}: no document built"
        );
        let mut session = Session::open(&path).unwrap();
        assert!(!session.newer, "{version}: built as a newer one's");
        let mut logged = Logged::open(&session, READER).expect("the folder is logged");
        let outcome = logged.sync(&mut session, 2_000_000_000).unwrap();
        assert!(outcome.merged, "{version}: the heads didn't merge");
        assert!(!session.newer, "{version}: read-only after the merge");

        let store = Store::load(&log_folder(&path)).unwrap();
        let merged = expected["merged"]["id"].as_str().unwrap();
        assert_eq!(
            markdown(&session),
            expected["merged"]["markdown"].as_str().unwrap(),
            "{version}: the merged document"
        );
        assert_eq!(
            serde_json::to_value(session.document()).unwrap(),
            expected["merged"]["document"],
            "{version}: the merged document, notes and all"
        );
        // Two machines at different versions reach the same revision from the same heads, or
        // they'd go on merging each other's merges.
        assert_eq!(
            store.heads(),
            vec![merged.to_string()],
            "{version}: the merge this build wrote isn't the one {version} wrote"
        );

        // And this build writes on from there.
        let end = session.document().blocks.len() - 1;
        let block = &session.document().blocks[end];
        session.set_caret(kame_core::edit::Cursor {
            block_id: block.id,
            offset: block.text.len(),
        });
        assert!(session.insert(" Read on."));
        let outcome = logged.sync(&mut session, 2_000_000_100).unwrap();
        assert_eq!(
            outcome.written, 1,
            "{version}: the edit wasn't written down"
        );
        let store = Store::load(&log_folder(&path)).unwrap();
        let heads = store.heads();
        assert_eq!(heads.len(), 1, "{version}: one head after writing on");
        assert!(
            store
                .document(&heads[0])
                .blocks
                .last()
                .unwrap()
                .text
                .ends_with(" Read on."),
            "{version}: the edit isn't in the head"
        );
    }
}
