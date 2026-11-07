//! Revision logs and the merge, fuzzed. The releases' sample logs have lines damaged, cut, repeated,
//! moved and made up, some signed again so that they pass their check and reach the merge, then
//! read, merged and synced into an open document. And documents that read are damaged, typed into
//! and merged directly. Nothing may panic, a merge keeps every word either side added, and what's
//! typed survives a sync. See `kame_test_dir::fuzz` for running it longer.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use kame_core::edit::{Cursor, Editor, Selection};
use kame_core::export::markdown::to_markdown;
use kame_core::export::to_html;
use kame_core::model::{Anchor, AnnotationKind, Block, BlockKind, Document};
use kame_core::session::Session;
use kame_sync::digest;
use kame_sync::logs::{build_from_logs, log_folder, Logged, MARKER};
use kame_sync::merge::{merge, Side};
use kame_sync::revlog::{Revision, Store};
use kame_test_dir::fuzz::{self, damage_bytes, damage_json, Random};
use serde_json::Value;
use uuid::Uuid;

const READER: &str = "reader-cccc";

static USABLE: AtomicUsize = AtomicUsize::new(0);
static DAMAGED: AtomicUsize = AtomicUsize::new(0);
static MERGED: AtomicUsize = AtomicUsize::new(0);
static SYNCED: AtomicUsize = AtomicUsize::new(0);

fn tally(counter: &AtomicUsize) {
    counter.fetch_add(1, Ordering::Relaxed);
}

fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../kame-core/tests/samples")
}

/// Each release's folder of logs.
fn releases() -> &'static Vec<PathBuf> {
    static FOUND: OnceLock<Vec<PathBuf>> = OnceLock::new();
    FOUND.get_or_init(|| {
        let mut found: Vec<PathBuf> = std::fs::read_dir(samples())
            .unwrap()
            .flatten()
            .map(|entry| entry.path().join("logged"))
            .filter(|path| path.is_dir())
            .collect();
        found.sort();
        assert!(!found.is_empty(), "no release's logs");
        found
    })
}

/// Every line of every release's logs.
fn every_line() -> &'static Vec<Vec<u8>> {
    static LINES: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    LINES.get_or_init(|| {
        let mut lines = Vec::new();
        for release in releases() {
            for (_, log) in logs_in(&release.join(".essay.kame.sync")) {
                lines.extend(log);
            }
        }
        lines
    })
}

/// The heads of every release's logs as they are.
fn sound_heads() -> &'static HashSet<String> {
    static HEADS: OnceLock<HashSet<String>> = OnceLock::new();
    HEADS.get_or_init(|| {
        releases()
            .iter()
            .flat_map(|release| {
                Store::load(&release.join(".essay.kame.sync"))
                    .unwrap()
                    .heads()
            })
            .collect()
    })
}

/// Documents to damage and merge: every release's sample document, and each head of its logs.
fn documents() -> &'static Vec<Document> {
    static DOCS: OnceLock<Vec<Document>> = OnceLock::new();
    DOCS.get_or_init(|| {
        let mut docs = Vec::new();
        for release in releases() {
            let folder = release.parent().unwrap();
            let essay: Value =
                serde_json::from_str(&std::fs::read_to_string(folder.join("essay.json")).unwrap())
                    .unwrap();
            docs.push(serde_json::from_value(essay["document"].clone()).unwrap());
            let store = Store::load(&release.join(".essay.kame.sync")).unwrap();
            for head in store.heads() {
                docs.push(store.document(&head));
            }
        }
        docs
    })
}

/// The lines of each log in a folder, by name.
fn logs_in(folder: &Path) -> Vec<(String, Vec<Vec<u8>>)> {
    let mut logs: Vec<(String, Vec<Vec<u8>>)> = std::fs::read_dir(folder)
        .unwrap()
        .flatten()
        .map(|entry| {
            let bytes = std::fs::read(entry.path()).unwrap();
            let lines = bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(<[u8]>::to_vec)
                .collect();
            (entry.file_name().to_string_lossy().into_owned(), lines)
        })
        .collect();
    logs.sort();
    logs
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

/// A block's digest, as a revision names it, if it reads as a block.
fn block_digest(block: &Value) -> Option<String> {
    let block: Block = serde_json::from_value(block.clone()).ok()?;
    Some(digest(&serde_json::to_vec(&block).ok()?))
}

/// A line damaged as JSON, and signed again if `sign`: each block it carries that still reads
/// takes its new digest in the revision's order, and the revision, if it still reads, its new id,
/// so that the line passes its check and what it holds goes on to be merged.
fn damaged_line(random: &mut Random, line: &[u8], donor: &[u8], sign: bool) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<Value>(line) else {
        let mut bytes = line.to_vec();
        damage_bytes(random, &mut bytes, donor);
        return bytes;
    };
    let donor: Value = serde_json::from_slice(donor).unwrap_or(Value::Null);
    let digests = |value: &Value| -> Vec<Option<String>> {
        value["blocks"]
            .as_array()
            .map(|blocks| blocks.iter().map(block_digest).collect())
            .unwrap_or_default()
    };
    let before = digests(&value);
    for _ in 0..random.between(1, 3) {
        // Mostly inside the revision or a block, where a damaged line still has a shape to read.
        let inside = match random.below(4) {
            0 => "",
            1 | 2 => "/revision",
            _ => "/blocks",
        };
        match value.pointer_mut(inside) {
            Some(part) => damage_json(random, part, &donor),
            None => damage_json(random, &mut value, &donor),
        }
    }
    if sign {
        let after = digests(&value);
        if let Some(Value::Array(order)) = value.pointer_mut("/revision/order") {
            for (old, new) in before.iter().zip(&after) {
                if let (Some(old), Some(new)) = (old, new) {
                    for named in order.iter_mut() {
                        if named.as_str() == Some(old) {
                            *named = Value::String(new.clone());
                        }
                    }
                }
            }
        }
        if let Some(revision) = value
            .get("revision")
            .and_then(|revision| serde_json::from_value::<Revision>(revision.clone()).ok())
        {
            let id = digest(&serde_json::to_vec(&revision).unwrap());
            if let Some(fields) = value.as_object_mut() {
                fields.insert("id".to_string(), Value::String(id));
            }
        }
    }
    serde_json::to_vec(&value).unwrap()
}

/// Damages the logs once: a line, which lines there are, or a log's end.
fn damage_logs(
    random: &mut Random,
    logs: &mut Vec<(String, Vec<Vec<u8>>)>,
    torn: &mut Vec<String>,
) {
    let donors = every_line();
    if logs.is_empty() || random.one_in(12) {
        logs.push((format!("zeta-{:04x}.log", random.below(4)), Vec::new()));
    }
    let which = random.below(logs.len());
    let (name, lines) = &mut logs[which];
    let donor = random.pick(donors).clone();
    let index = random.below(lines.len());
    match random.below(12) {
        0..=3 if !lines.is_empty() => {
            lines[index] = damaged_line(random, &lines[index], &donor, true)
        }
        4 if !lines.is_empty() => lines[index] = damaged_line(random, &lines[index], &donor, false),
        5 if !lines.is_empty() => damage_bytes(random, &mut lines[index], &donor),
        6 if !lines.is_empty() => match random.below(3) {
            0 => {
                lines.remove(index);
            }
            1 => {
                let copy = lines[index].clone();
                lines.insert(random.below(lines.len() + 1), copy);
            }
            _ => {
                let other = random.below(lines.len());
                lines.swap(index, other);
            }
        },
        7 => {
            let line = if random.one_in(2) {
                donor
            } else {
                let other = random.pick(donors).clone();
                damaged_line(random, &donor, &other, true)
            };
            lines.insert(random.below(lines.len() + 1), line);
        }
        8 => torn.push(name.clone()),
        9 => lines.push(fuzz::random_bytes(random, 200)),
        _ => {
            let line = random.pick(donors).clone();
            lines.push(damaged_line(random, &line, &donor, true));
        }
    }
}

fn write_logs(
    folder: &Path,
    logs: &[(String, Vec<Vec<u8>>)],
    torn: &[String],
    random: &mut Random,
) {
    std::fs::create_dir_all(folder).unwrap();
    for (name, lines) in logs {
        let mut bytes = Vec::new();
        for line in lines {
            bytes.extend_from_slice(line);
            bytes.push(b'\n');
        }
        if torn.contains(name) {
            let line = random.pick(every_line());
            bytes.extend_from_slice(&line[..random.below(line.len())]);
        }
        std::fs::write(folder.join(name), bytes).unwrap();
    }
}

/// Everything kame relies on about a document it holds.
fn check_valid(doc: &Document) {
    let mut ids: Vec<_> = doc.blocks.iter().map(|block| block.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), doc.blocks.len(), "block ids repeat");
    for block in &doc.blocks {
        let fits =
            |offset: usize| offset <= block.text.len() && block.text.is_char_boundary(offset);
        // An empty span fits its text, and reading leaves it as it is (`Document::fit`).
        for span in &block.spans {
            assert!(
                span.start <= span.end && fits(span.start) && fits(span.end),
                "{span:?}"
            );
        }
    }
    for note in &doc.annotations {
        if let Anchor::Range {
            block_id,
            start,
            end,
        } = &note.anchor
        {
            if let Some(block) = doc.blocks.iter().find(|block| block.id == *block_id) {
                let fits = |offset: usize| {
                    offset <= block.text.len() && block.text.is_char_boundary(offset)
                };
                assert!(start <= end && fits(*start) && fits(*end), "{note:?}");
            }
        }
    }
    assert_eq!(to_markdown(doc), to_markdown(doc));
    assert_eq!(to_html(doc), to_html(doc));
}

/// A document's words, its text and its notes', as one string to look for words in.
fn all_words(doc: &Document) -> String {
    let mut words = String::new();
    for block in &doc.blocks {
        words.push_str(&block.text);
        words.push('\n');
    }
    for note in &doc.annotations {
        words.push_str(&note.body);
        words.push('\n');
    }
    words
}

/// A document's words, as the merge takes words: runs of letters and digits. What's between
/// them can be shared by the words on either side of a conflict, and kept with the other.
fn word_set(doc: &Document) -> HashSet<String> {
    all_words(doc)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

/// Merges both ways round, checking that the order doesn't matter, the result is one kame can
/// hold, and every word either side has that the base doesn't is in its text or its notes.
fn merged_keeping_words(
    base: Option<&Document>,
    one: &Document,
    other: &Document,
    when: (i64, i64),
) {
    let forwards = merge(
        base,
        Side {
            doc: one,
            label: "one",
            when: when.0,
        },
        Side {
            doc: other,
            label: "other",
            when: when.1,
        },
    );
    let backwards = merge(
        base,
        Side {
            doc: other,
            label: "other",
            when: when.1,
        },
        Side {
            doc: one,
            label: "one",
            when: when.0,
        },
    );
    tally(&MERGED);
    assert_eq!(
        forwards.doc, backwards.doc,
        "the order of the sides matters"
    );
    check_valid(&forwards.doc);
    let shared = base.map(word_set).unwrap_or_default();
    let words = all_words(&forwards.doc);
    for side in [one, other] {
        let mut fitted = side.clone();
        fitted.fit();
        for word in word_set(&fitted).difference(&shared) {
            assert!(words.contains(word.as_str()), "lost {word:?}: {words}");
        }
    }
}

/// What reading and merging damaged logs must survive.
fn read_and_merge(folder: &Path) {
    let Ok(store) = Store::load(folder) else {
        return;
    };
    let heads = store.heads();
    for head in &heads {
        tally(&USABLE);
        if !sound_heads().contains(head) {
            tally(&DAMAGED);
        }
        let doc = store.document(head);
        check_valid(&doc);
        let _ = store.metadata(head);
    }
    for pair in heads.windows(2) {
        let base = store
            .shared_ancestor(&pair[0], &pair[1])
            .filter(|id| store.is_usable(id))
            .map(|id| store.document(&id));
        let (one, other) = (store.document(&pair[0]), store.document(&pair[1]));
        merged_keeping_words(
            base.as_ref(),
            &one,
            &other,
            (store.when(&pair[0]), store.when(&pair[1])),
        );
    }
}

fn type_at_end(session: &mut Session, words: &str) -> bool {
    let block = session.document().blocks.len() - 1;
    let block_id = session.document().blocks[block].id;
    let offset = session.document().blocks[block].text.len();
    session.set_caret(Cursor { block_id, offset });
    session.insert(words)
}

/// A document built from damaged logs, synced with them, typed into and synced again.
fn sync_with(path: &Path) {
    if build_from_logs(path).is_err() || !path.exists() {
        return;
    }
    let Ok(mut session) = Session::open(path) else {
        return;
    };
    let Some(mut logged) = Logged::open(&session, READER) else {
        return;
    };
    if logged.sync(&mut session, 2_000_000_000).is_err() {
        return;
    }
    tally(&SYNCED);
    check_valid(session.document());
    if session.newer || !type_at_end(&mut session, " zqsyncedq") {
        return;
    }
    if logged.sync(&mut session, 2_000_000_100).is_ok() {
        check_valid(session.document());
        assert!(
            all_words(session.document()).contains("zqsyncedq"),
            "what was typed went in a sync"
        );
    }
}

fn logs_one(seed: u64) {
    let mut random = Random::new(seed);
    let root = kame_test_dir::folder("fuzz-logs");
    copy_folder(random.pick(releases()), &root);
    assert!(root.join(MARKER).exists());
    let path = root.join("essay.kame");
    let folder = log_folder(&path);
    let mut logs = logs_in(&folder);
    let mut torn = Vec::new();
    for _ in 0..random.between(1, 4) {
        damage_logs(&mut random, &mut logs, &mut torn);
    }
    std::fs::remove_dir_all(&folder).unwrap();
    write_logs(&folder, &logs, &torn, &mut random);
    read_and_merge(&folder);
    sync_with(&path);
    let _ = std::fs::remove_dir_all(&root);
}

/// A document damaged as JSON for as long as it takes to find one that still reads.
fn readable(random: &mut Random, doc: &Document, times: usize) -> Document {
    let value = serde_json::to_value(doc).unwrap();
    let donor = serde_json::to_value(random.pick(documents())).unwrap();
    for _ in 0..20 {
        let mut damaged = value.clone();
        for _ in 0..times {
            damage_json(random, &mut damaged, &donor);
        }
        if let Ok(read) = serde_json::from_value::<Document>(damaged) {
            return read;
        }
    }
    doc.clone()
}

/// `doc` edited through kame's commands, as on the screen, with the words typed.
fn edited(random: &mut Random, doc: &Document, tag: &str) -> Document {
    let mut doc = doc.clone();
    doc.fit();
    if doc.blocks.is_empty() {
        return doc;
    }
    let before: HashSet<Uuid> = doc
        .blocks
        .iter()
        .map(|block| block.id)
        .chain(doc.annotations.iter().map(|note| note.id))
        .collect();
    let mut session = Session::anonymous().unwrap();
    session.editor = Editor::new(doc);
    for edit in 0..random.below(6) {
        let count = session.document().blocks.len();
        let block = random.below(count);
        let text = session.document().blocks[block].text.clone();
        let mut offset = random.below(text.len() + 1);
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        let block_id = session.document().blocks[block].id;
        session.set_caret(Cursor { block_id, offset });
        match random.below(6) {
            0 | 1 => {
                session.insert(&format!(" zq{tag}{edit}q "));
            }
            2 => {
                session.split();
            }
            3 => {
                let mut end = (offset + random.below(30)).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                let at = |offset| Cursor { block_id, offset };
                session.delete_range(
                    Selection {
                        start: at(offset),
                        end: at(end),
                    },
                    at(offset),
                );
            }
            4 => {
                session.set_block_kind(
                    block_id,
                    BlockKind::Heading {
                        level: 2,
                        centered: false,
                    },
                );
            }
            _ => {
                if let Some(note) = session.add_annotation(AnnotationKind::Note) {
                    session.set_annotation_body(note, &format!("zn{tag}{edit}q"));
                }
            }
        }
    }
    with_ids_from(random, session.document(), &before)
}

/// `doc` with each block and note id that `before` doesn't have, which kame's commands made at
/// random, taken from `random` instead, in the order they come in, so that a seed gives the same
/// input in every run.
fn with_ids_from(random: &mut Random, doc: &Document, before: &HashSet<Uuid>) -> Document {
    let made: Vec<Uuid> = doc
        .blocks
        .iter()
        .map(|block| block.id)
        .chain(doc.annotations.iter().map(|note| note.id))
        .filter(|id| !before.contains(id))
        .collect();
    let mut json = serde_json::to_string(doc).unwrap();
    for id in made {
        let fixed = Uuid::from_u64_pair(random.next_u64(), random.next_u64());
        json = json.replace(&id.to_string(), &fixed.to_string());
    }
    serde_json::from_str(&json).unwrap()
}

fn merge_one(seed: u64) {
    let mut random = Random::new(seed);
    let start = random.pick(documents()).clone();
    let times = random.between(0, 3);
    let base = readable(&mut random, &start, times);
    let times = random.between(0, 3);
    let one = readable(&mut random, &base, times);
    let one = edited(&mut random, &one, "a");
    let times = random.between(0, 3);
    let other = readable(&mut random, &base, times);
    let other = edited(&mut random, &other, "b");
    let when = (random.below(3) as i64, random.below(3) as i64);
    let base = (!random.one_in(5)).then_some(&base);
    merged_keeping_words(base, &one, &other, when);
}

fn counted() -> String {
    let read = |counter: &AtomicUsize| counter.load(Ordering::Relaxed);
    format!(
        "{} usable heads read ({} of them damaged), {} merges, {} documents synced",
        read(&USABLE),
        read(&DAMAGED),
        read(&MERGED),
        read(&SYNCED)
    )
}

#[test]
fn damaged_logs_read_merge_and_sync_without_losing_words() {
    fuzz::short("damaged_logs", 150, logs_one);
}

#[test]
fn damaged_documents_merge_without_losing_words() {
    fuzz::short("damaged_merges", 300, merge_one);
}

#[test]
#[ignore = "long: run with --ignored, KAME_FUZZ_ITERATIONS and KAME_FUZZ_SEED"]
fn fuzz_damaged_logs() {
    fuzz::long("fuzz_damaged_logs", logs_one);
    eprintln!("fuzz target fuzz_damaged_logs: {}", counted());
}

#[test]
#[ignore = "long: run with --ignored, KAME_FUZZ_ITERATIONS and KAME_FUZZ_SEED"]
fn fuzz_damaged_merges() {
    fuzz::long("fuzz_damaged_merges", merge_one);
    eprintln!("fuzz target fuzz_damaged_merges: {}", counted());
}
