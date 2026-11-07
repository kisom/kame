//! The `kame-sync` command line, run as it is from a shell.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use kame_core::edit::Cursor;
use kame_core::model::BlockKind;
use kame_core::session::Session;
use kame_core::storage::Db;

/// A document typed in two paragraphs, copied, and each copy edited: two machines that drifted.
fn diverged(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = common::test_folder(name);
    let (one, other) = (dir.join("talos.kame"), dir.join("titan.kame"));
    {
        let mut session = Session::open(&one).unwrap();
        session.insert("A shared opening. It has words.");
        session.split();
        session.insert("A shared ending. It has more.");
    }
    std::fs::copy(&one, &other).unwrap();
    for (path, block, words) in [(&one, 0, " fine"), (&other, 1, " grand")] {
        let mut session = Session::open(path).unwrap();
        let block_id = session.document().blocks[block].id;
        session.set_caret(Cursor {
            block_id,
            offset: 1,
        });
        session.insert(words);
    }
    (one, other, dir.join("merged.kame"))
}

fn merge(one: &Path, other: &Path, out: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kame-sync"))
        .arg("merge")
        .args([one, other, out])
        .output()
        .unwrap()
}

#[test]
fn two_versions_merge_into_a_new_file() {
    let (one, other, out) = diverged("cli-merge");
    let run = merge(&one, &other, &out);
    assert!(run.status.success(), "{run:?}");
    let merged = Session::open(&out).unwrap();
    let texts: Vec<&str> = merged
        .document()
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect();
    assert_eq!(
        texts,
        [
            "A fine shared opening. It has words.",
            "A grand shared ending. It has more."
        ]
    );
}

/// What this build doesn't know of a newer version's file would be left out of the merge, or
/// written fitted, so either file from one, as the first or the second, is refused.
#[test]
fn a_file_from_a_newer_version_is_not_merged() {
    let with_a_newer_field = |path: &Path| {
        let connection = rusqlite::Connection::open(path).unwrap();
        let head: String = connection
            .query_row("SELECT json FROM documents", [], |row| row.get(0))
            .unwrap();
        let newer = head.replacen(
            "\"kind\":\"Paragraph\"",
            "\"kind\":\"Paragraph\",\"tilt\":3",
            1,
        );
        connection
            .execute("UPDATE documents SET json = ?1", [newer])
            .unwrap();
    };
    let with_a_newer_depth = |path: &Path| {
        let mut doc = Session::open(path).unwrap().document().clone();
        doc.blocks[1].kind = BlockKind::ListItem {
            ordered: false,
            depth: 7,
        };
        Db::open(path).unwrap().save_document(&doc).unwrap();
    };
    for (name, newer) in [
        ("cli-newer-field", &with_a_newer_field as &dyn Fn(&Path)),
        ("cli-newer-depth", &with_a_newer_depth),
    ] {
        for first in [true, false] {
            let (one, other, out) = diverged(name);
            newer(if first { &one } else { &other });
            assert!(Db::open(if first { &one } else { &other })
                .unwrap()
                .holds_newer());

            let run = merge(&one, &other, &out);

            let said = String::from_utf8_lossy(&run.stderr);
            assert!(!run.status.success(), "{name}, first {first}: merged");
            assert!(said.contains("newer version of kame"), "{name}: {said}");
            assert!(!out.exists(), "{name}: a merge was written");
        }
    }
}
