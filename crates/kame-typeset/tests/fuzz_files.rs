//! `.kame` files fuzzed: the releases' samples, and a document built here, with their head,
//! metadata, history, snapshots and `meta` damaged through SQL, then opened as the app opens one.
//! Opening must give a session or a clean error, never panic, and a refused file is left as it
//! was. A file that opens is laid out as the page lays it out, exported, undone all the way and
//! redone, typed in, cut from, saved under another name and opened again, and keeps its words
//! through all of it. See `kame_test_dir::fuzz` for running it longer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use cosmic_text::FontSystem;
use kame_core::edit::{Cursor, Selection};
use kame_core::export::{to_html_with, to_markdown_with};
use kame_core::model::{AnnotationKind, BlockKind, Document};
use kame_core::references::References;
use kame_core::session::Session;
use kame_test_dir::fuzz::{self, damage_bytes, damaged_json, Random};
use kame_typeset::layout::{
    layout_block_marked, place_footnotes, place_marginalia, reading_order, BlockLayout,
    LayoutConfig,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

/// How far the files got, to show the fuzzing reaches past opening.
static OPENED: AtomicUsize = AtomicUsize::new(0);
static REFUSED: AtomicUsize = AtomicUsize::new(0);
static NEWER: AtomicUsize = AtomicUsize::new(0);
static UNDONE: AtomicUsize = AtomicUsize::new(0);
static COPIED: AtomicUsize = AtomicUsize::new(0);

fn tally(counter: &AtomicUsize, by: usize) {
    counter.fetch_add(by, Ordering::Relaxed);
}

fn counted() -> String {
    let read = |counter: &AtomicUsize| counter.load(Ordering::Relaxed);
    format!(
        "{} opened ({} from a newer version), {} refused, {} steps undone, {} saved as and reopened",
        read(&OPENED),
        read(&NEWER),
        read(&REFUSED),
        read(&UNDONE),
        read(&COPIED)
    )
}

fn fonts() -> &'static Mutex<FontSystem> {
    static FONTS: OnceLock<Mutex<FontSystem>> = OnceLock::new();
    FONTS.get_or_init(|| Mutex::new(kame_typeset::render::egui_render::font_system()))
}

/// The files damage starts from: each release's sample, and one built here by kame's commands.
fn sources() -> &'static Vec<PathBuf> {
    static SOURCES: OnceLock<Vec<PathBuf>> = OnceLock::new();
    SOURCES.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../kame-core/tests/samples");
        let mut found: Vec<PathBuf> = std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .map(|entry| entry.path().join("essay.kame"))
            .filter(|path| path.exists())
            .collect();
        found.sort();
        assert!(!found.is_empty(), "no release's sample");
        found.push(built());
        found
    })
}

fn at(session: &mut Session, block: usize, offset: usize) {
    let block_id = session.document().blocks[block].id;
    session.set_caret(Cursor { block_id, offset });
}

/// A document written by kame's commands: every kind of block it makes, notes of each kind, a
/// history with a step to redo, and a snapshot.
fn built() -> PathBuf {
    let path = kame_test_dir::folder("fuzz-built").join("built.kame");
    let mut session = Session::open(&path).unwrap();
    session.insert("A heading");
    session.split();
    session.insert("Un été à Paris. 🙂 Words and more words.");
    session.split();
    session.insert("One item");
    session.split();
    session.insert("Deeper item");
    session.split();
    session.insert("let a = 1;");
    session.line_break();
    session.insert("let b = 2;");
    let ids: Vec<_> = session.document().blocks.iter().map(|b| b.id).collect();
    session.set_block_kind(
        ids[0],
        BlockKind::Heading {
            level: 2,
            centered: false,
        },
    );
    session.set_block_kind(
        ids[2],
        BlockKind::ListItem {
            ordered: true,
            depth: 0,
        },
    );
    session.set_block_kind(
        ids[3],
        BlockKind::ListItem {
            ordered: false,
            depth: 1,
        },
    );
    session.set_block_kind(ids[4], BlockKind::Code { language: None });
    for (block, offset, kind) in [
        (1, 3, AnnotationKind::Note),
        (1, 8, AnnotationKind::Footnote),
        (1, 0, AnnotationKind::Marginal),
    ] {
        at(&mut session, block, offset);
        if let Some(id) = session.add_annotation(kind) {
            session.set_annotation_body(id, "A note's words");
        }
    }
    session.snapshot(Some("built"));
    at(&mut session, 1, 0);
    session.insert("Undone ");
    session.undo();
    assert!(session.last_error.is_none(), "{:?}", session.last_error);
    drop(session);
    path
}

/// A stored value as bytes, whatever type SQLite holds it as.
fn stored(connection: &Connection, sql: &str) -> Option<Vec<u8>> {
    connection
        .query_row(sql, [], |row| row.get::<_, Option<Vec<u8>>>(0))
        .optional()
        .ok()
        .flatten()
        .flatten()
}

/// Writes `bytes` as text, which SQLite takes even when it isn't UTF-8.
fn store(connection: &Connection, sql: &str, bytes: &[u8], key: i64) {
    let _ = match std::str::from_utf8(bytes) {
        Ok(text) => connection.execute(sql, params![text, key]),
        Err(_) => connection.execute(
            &sql.replacen("?1", "CAST(?1 AS TEXT)", 1),
            params![bytes, key],
        ),
    };
}

fn damaged(random: &mut Random, bytes: &[u8], donor: &[u8]) -> Vec<u8> {
    let donor_value: Value = serde_json::from_slice(donor).unwrap_or(Value::Null);
    if random.below(10) < 7 {
        let times = random.between(1, 3);
        if let Some(json) = std::str::from_utf8(bytes)
            .ok()
            .and_then(|json| damaged_json(random, json, &donor_value, times))
        {
            return json.into_bytes();
        }
    }
    let mut bytes = bytes.to_vec();
    damage_bytes(random, &mut bytes, donor);
    bytes
}

/// The keys a query gives, none if its table has gone.
fn rows(connection: &Connection, sql: &str) -> Vec<i64> {
    let Ok(mut statement) = connection.prepare(sql) else {
        return Vec::new();
    };
    let Ok(rows) = statement.query_map([], |row| row.get(0)) else {
        return Vec::new();
    };
    rows.flatten().collect()
}

/// Damages one thing in the file: the head, the metadata, a step of history, a snapshot, a `meta`
/// value, or a row or table as a whole.
fn damage(random: &mut Random, connection: &Connection) {
    let head = stored(connection, "SELECT json FROM documents WHERE id = 1").unwrap_or_default();
    let ops = rows(connection, "SELECT seq FROM ops");
    let snapshots = rows(connection, "SELECT id FROM snapshots");
    match random.below(20) {
        0..=5 => {
            let donor = snapshots
                .first()
                .and_then(|id| {
                    stored(
                        connection,
                        &format!("SELECT json FROM snapshots WHERE id = {id}"),
                    )
                })
                .unwrap_or_else(|| head.clone());
            let bytes = damaged(random, &head, &donor);
            store(
                connection,
                "UPDATE documents SET json = ?1 WHERE id = ?2",
                &bytes,
                1,
            );
        }
        6 => {
            let metadata =
                stored(connection, "SELECT json FROM metadata WHERE id = 1").unwrap_or_default();
            let bytes = damaged(random, &metadata, &head);
            store(
                connection,
                "UPDATE metadata SET json = ?1 WHERE id = ?2",
                &bytes,
                1,
            );
        }
        7..=10 if !ops.is_empty() => {
            let seq = *random.pick(&ops);
            let other = *random.pick(&ops);
            let op = stored(
                connection,
                &format!("SELECT json FROM ops WHERE seq = {seq}"),
            )
            .unwrap_or_default();
            let donor = stored(
                connection,
                &format!("SELECT json FROM ops WHERE seq = {other}"),
            )
            .unwrap_or_default();
            let bytes = damaged(random, &op, &donor);
            match random.below(4) {
                0 => store(
                    connection,
                    "INSERT INTO ops (json, seq) VALUES (?1, (SELECT MAX(seq) FROM ops) + ?2)",
                    &bytes,
                    1 + random.below(3) as i64,
                ),
                1 => {
                    let _ = connection.execute("DELETE FROM ops WHERE seq = ?1", [seq]);
                }
                _ => store(
                    connection,
                    "UPDATE ops SET json = ?1 WHERE seq = ?2",
                    &bytes,
                    seq,
                ),
            }
        }
        11..=13 if !snapshots.is_empty() => {
            let id = *random.pick(&snapshots);
            let snapshot = stored(
                connection,
                &format!("SELECT json FROM snapshots WHERE id = {id}"),
            )
            .unwrap_or_default();
            match random.below(6) {
                0 => {
                    let label = random.text(8);
                    let _ = connection.execute(
                        "UPDATE snapshots SET label = ?1 WHERE id = ?2",
                        params![label, id],
                    );
                }
                1 => {
                    let when = *random.pick(&[-1i64, 0, i64::MAX, i64::MIN]);
                    let _ = connection.execute(
                        "UPDATE snapshots SET timestamp = ?1 WHERE id = ?2",
                        params![when, id],
                    );
                }
                _ => {
                    let bytes = damaged(random, &snapshot, &head);
                    store(
                        connection,
                        "UPDATE snapshots SET json = ?1 WHERE id = ?2",
                        &bytes,
                        id,
                    );
                }
            }
        }
        14 => {
            let cursor = random
                .pick(&[
                    "0",
                    "1",
                    "2",
                    "-1",
                    "999999",
                    "abc",
                    "",
                    "1.5",
                    "18446744073709551616",
                ])
                .to_string();
            let _ = connection.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('undo_cursor', ?1)",
                [cursor],
            );
        }
        15 => {
            let position =
                stored(connection, "SELECT value FROM meta WHERE key = 'position'").unwrap_or_else(
                    || br#"{"block_id":"00000000-0000-0000-0000-000000000000","offset":0,"scroll":0.0}"#.to_vec(),
                );
            let bytes = damaged(random, &position, &head);
            let _ = connection.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('position', ?1)",
                [String::from_utf8_lossy(&bytes).into_owned()],
            );
        }
        _ => {
            let sql = random.pick(&[
                "DELETE FROM documents",
                "DELETE FROM metadata",
                "DELETE FROM ops",
                "DELETE FROM snapshots",
                "DROP TABLE ops",
                "DROP TABLE snapshots",
                "DROP TABLE metadata",
                "UPDATE documents SET json = CAST(json AS BLOB)",
                "UPDATE documents SET json = 7",
                "UPDATE ops SET json = NULL",
                "UPDATE snapshots SET json = NULL",
                "UPDATE snapshots SET timestamp = NULL",
                "UPDATE snapshots SET json = (SELECT json FROM documents)",
                "INSERT INTO ops (json) SELECT json FROM ops",
                "PRAGMA user_version = 2",
            ]);
            let _ = connection.execute(sql, []);
        }
    }
}

/// The words of a document: its blocks' text in order, and its notes', which have no order.
fn words(doc: &Document) -> (Vec<String>, Vec<String>) {
    let texts = doc.blocks.iter().map(|block| block.text.clone()).collect();
    let mut notes: Vec<String> = doc
        .annotations
        .iter()
        .map(|note| note.body.clone())
        .collect();
    notes.sort();
    (texts, notes)
}

/// Lays the document out as the page does: every block shaped with room for its footnotes'
/// marks, then its notes placed in the margin and its footnotes below.
fn lay_out(doc: &Document) {
    let config = LayoutConfig::default();
    let mut marks: HashMap<_, Vec<usize>> = HashMap::new();
    for note in &doc.annotations {
        if note.kind == AnnotationKind::Footnote {
            if let Some((_, end)) = note.anchor.byte_range() {
                marks.entry(note.anchor.block_id()).or_default().push(end);
            }
        }
    }
    let mut fonts = fonts()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut layouts: Vec<BlockLayout> = Vec::new();
    let mut tops = Vec::new();
    let mut y = 0.0;
    for block in &doc.blocks {
        let mut marked = marks.get(&block.id).cloned().unwrap_or_default();
        marked.sort_unstable();
        let layout = layout_block_marked(&mut fonts, block, &config, &marked);
        tops.push(y);
        y += layout.lines.iter().map(|line| line.height).sum::<f32>() + 10.0;
        layouts.push(layout);
    }
    let ordered = reading_order(&doc.annotations, &doc.blocks);
    place_marginalia(&ordered, &doc.blocks, &layouts, &[], &tops, &config);
    place_footnotes(&ordered, y, &[]);
}

fn exports(session: &Session) {
    let doc = session.document();
    let metadata = session.published_metadata();
    let references = References::new();
    let markdown = to_markdown_with(doc, &metadata, &references);
    assert_eq!(markdown, to_markdown_with(doc, &metadata, &references));
    let html = to_html_with(doc, &metadata, &references);
    assert_eq!(html, to_html_with(doc, &metadata, &references));
}

/// A character boundary in block `block`, near `wanted`.
fn boundary(session: &Session, block: usize, wanted: usize) -> usize {
    let text = &session.document().blocks[block].text;
    let mut offset = wanted.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// What a file that opened must survive, keeping its words.
fn exercise(random: &mut Random, mut session: Session, folder: &Path) {
    let opened = session.document().clone();
    assert!(
        !opened.blocks.is_empty(),
        "a document with no blocks opened"
    );
    lay_out(&opened);
    exports(&session);

    // All the way back and forward again, to where it opened, then through whatever was waiting.
    let (applied, waiting) = session.history();
    let mut undone = 0;
    while undone < applied && session.undo() {
        undone += 1;
    }
    let mut redone = 0;
    while redone < undone {
        assert!(
            session.redo(),
            "a step undone can't be redone: {:?}",
            session.last_error
        );
        redone += 1;
    }
    tally(&UNDONE, undone);
    assert_eq!(
        words(session.document()),
        words(&opened),
        "undoing and redoing changed the words"
    );
    for _ in 0..waiting {
        if !session.redo() {
            break;
        }
    }
    exports(&session);

    // Typing, and taking some out.
    let count = session.document().blocks.len();
    let block = random.below(count);
    let offset = boundary(&session, block, random.below(200));
    at(&mut session, block, offset);
    if session.insert(" zqtypedq ") {
        assert!(
            words(session.document())
                .0
                .iter()
                .any(|text| text.contains("zqtypedq")),
            "what was typed isn't there"
        );
    }
    let block = random.below(count);
    let start = boundary(&session, block, random.below(100));
    let end = boundary(&session, block, start + random.below(40));
    let block_id = session.document().blocks[block].id;
    let cursor = |offset| Cursor { block_id, offset };
    session.delete_range(
        Selection {
            start: cursor(start),
            end: cursor(end),
        },
        cursor(start),
    );
    if random.one_in(3) {
        at(&mut session, block, 0);
        session.backspace();
    }
    if random.one_in(3) {
        session.undo();
    }
    lay_out(session.document());
    exports(&session);

    // Saved under another name, it opens with the same words.
    if !session.newer {
        let copy = folder.join("copy.kame");
        if session.save_as(&copy, None, |_, _| false).is_ok() {
            let mut reopened = Session::open(&copy).expect("a copy kame saved doesn't open");
            tally(&COPIED, 1);
            assert_eq!(
                words(reopened.document()),
                words(session.document()),
                "the copy has other words"
            );
            if let Some(snapshots) = reopened.snapshots() {
                if !snapshots.is_empty() {
                    let id = random.pick(&snapshots).id;
                    if reopened.restore(id) {
                        lay_out(reopened.document());
                        exports(&reopened);
                    }
                }
            }
        }
    }
}

fn one(seed: u64) {
    let mut random = Random::new(seed);
    let folder = kame_test_dir::folder("fuzz-file");
    let path = folder.join("damaged.kame");
    std::fs::copy(random.pick(sources()), &path).unwrap();
    {
        let connection = Connection::open(&path).unwrap();
        for _ in 0..random.between(1, 3) {
            damage(&mut random, &connection);
        }
    }
    let head = || {
        Connection::open(&path)
            .ok()
            .and_then(|connection| stored(&connection, "SELECT json FROM documents WHERE id = 1"))
    };
    let before = head();
    match Session::open(&path) {
        Err(_) => {
            tally(&REFUSED, 1);
            assert_eq!(head(), before, "a file that didn't open was written");
        }
        Ok(session) => {
            tally(&OPENED, 1);
            let newer = session.newer;
            tally(&NEWER, usize::from(newer));
            exercise(&mut random, session, &folder);
            if newer {
                assert_eq!(head(), before, "a newer version's file was written");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn damaged_files_open_or_are_refused_and_keep_their_words() {
    fuzz::short("damaged_files", 150, one);
}

#[test]
#[ignore = "long: run with --ignored, KAME_FUZZ_ITERATIONS and KAME_FUZZ_SEED"]
fn fuzz_damaged_files() {
    fuzz::long("fuzz_damaged_files", one);
    eprintln!("fuzz target fuzz_damaged_files: {}", counted());
}
