//! Writes this build's samples, for every later build's tests to read: a document holding
//! everything a document can, and the revision logs of a document two machines wrote, each with
//! what reading it gives. Run once per release, from the release's commit, before it's tagged:
//!
//! ```sh
//! cargo run -p kame-sync --example write_samples
//! ```
//!
//! It writes `crates/kame-core/tests/samples/VERSION/`, VERSION being this build's, or into the
//! folder named as its one argument, and refuses a version that already has one, since nothing a
//! release wrote is ever changed. `crates/kame-core/tests/written_by_releases.rs` and
//! `crates/kame-sync/tests/logs_from_releases.rs` read every folder there.

use std::path::{Path, PathBuf};

use kame_core::edit::{Cursor, Op, Selection};
use kame_core::export::{to_html_with, to_markdown_with};
use kame_core::model::{
    AcceptedMark, Anchor, AnnotationId, AnnotationKind, Block, BlockId, BlockKind, ImageAlign,
    Metadata, Span, SpanStyle,
};
use kame_core::references::References;
use kame_core::session::Session;
use kame_sync::logs::{build_from_logs, log_folder, Logged, MARKER};
use kame_sync::revlog::Store;
use serde_json::{json, Value};
use uuid::Uuid;

/// The device the tests sync as, which no release's logs name. The tests use the same one, so that
/// the merge they write is the one recorded here.
const READER: &str = "reader-cccc";
/// When the tests' merge happens, by the reader's clock.
const READ_AT: i64 = 2_000_000_000;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let samples = match std::env::args_os().nth(1) {
        Some(folder) => PathBuf::from(folder),
        None => Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("kame-sync is in the workspace's crates folder")
            .join("kame-core/tests/samples"),
    };
    let out = samples.join(version);
    if out.exists() {
        return Err(format!(
            "{} is already there, and what a release wrote is never changed",
            out.display()
        )
        .into());
    }
    let scratch = kame_test_dir::folder("write-samples");
    let written = scratch.join(version);
    std::fs::create_dir_all(&written)?;
    write_document(&scratch.join("essay"), &written)?;
    write_logs(&scratch.join("logs"), &written)?;
    // Only once all of it is there, so that a failure leaves no half a folder to be taken for one.
    std::fs::create_dir_all(&samples)?;
    copy_folder(&written, &out)?;
    println!("wrote {}", out.display());
    Ok(())
}

// --- the document --------------------------------------------------------

/// A document being written, through the commands the app uses.
struct Writer {
    session: Session,
}

impl Writer {
    fn doc(&self) -> &kame_core::model::Document {
        self.session.document()
    }

    fn index(&self, id: BlockId) -> usize {
        self.session.block_index(id).expect("the block is there")
    }

    fn text(&self, id: BlockId) -> String {
        self.doc().blocks[self.index(id)].text.clone()
    }

    /// The byte range of `words` in the block, which has to hold them.
    fn find(&self, id: BlockId, words: &str) -> (usize, usize) {
        let start = self
            .text(id)
            .find(words)
            .unwrap_or_else(|| panic!("no {words:?} in the block"));
        (start, start + words.len())
    }

    fn at(&self, id: BlockId, offset: usize) -> Cursor {
        Cursor {
            block_id: id,
            offset,
        }
    }

    fn end_of(&self, id: BlockId) -> Cursor {
        self.at(id, self.text(id).len())
    }

    fn check(&self, landed: bool, what: &str) {
        assert!(landed, "{what}: {:?}", self.session.last_error);
    }

    fn apply(&mut self, op: Op, what: &str) {
        let op = self.session.editor.apply_edit(op);
        let landed = self.session.record(&op);
        self.check(landed, what);
    }

    /// Types `text` at the end of a block, as one step.
    fn type_at_end(&mut self, id: BlockId, text: &str) {
        let caret = self.end_of(id);
        self.session.set_caret(caret);
        let landed = self.session.insert(text);
        self.check(landed, "typing");
    }

    /// Presses Enter at the end of `after` and types `text` into the new block, which is then made
    /// `kind` if it isn't already.
    fn split_after(&mut self, after: BlockId, kind: BlockKind, text: &str) -> BlockId {
        let caret = self.end_of(after);
        self.session.set_caret(caret);
        let landed = self.session.split();
        self.check(landed, "Enter");
        let id = self.doc().blocks[self.index(after) + 1].id;
        self.make(id, kind);
        if !text.is_empty() {
            self.type_at_end(id, text);
        }
        id
    }

    /// Adds a block of `kind` holding `text` at the end, as the app's commands for an image or a
    /// rule do, and types into it the way verbatim text is typed, line feeds and all.
    fn append(&mut self, kind: BlockKind, text: &str) -> BlockId {
        let block = Block {
            id: Uuid::new_v4(),
            kind,
            text: String::new(),
            spans: Vec::new(),
        };
        let id = block.id;
        let index = self.doc().blocks.len();
        self.apply(Op::InsertBlock { index, block }, "adding a block");
        if !text.is_empty() {
            let result = self.session.editor.insert_text(&self.at(id, 0), text);
            let landed = self.session.commit(result);
            self.check(landed, "typing a verbatim block");
        }
        id
    }

    fn make(&mut self, id: BlockId, kind: BlockKind) {
        if self.doc().blocks[self.index(id)].kind != kind {
            let result = self.session.editor.set_block_kind(id, kind);
            let landed = self.session.commit(result);
            self.check(landed, "changing a block's kind");
        }
    }

    fn styled(&mut self, id: BlockId, styles: &[(&str, SpanStyle)]) {
        let spans = styles
            .iter()
            .map(|(words, style)| {
                let (start, end) = self.find(id, words);
                Span {
                    start,
                    end,
                    style: style.clone(),
                }
            })
            .collect();
        let result = self.session.editor.set_spans_of(id, spans);
        let landed = self.session.commit(result);
        self.check(landed, "styling");
    }

    fn annotate(&mut self, kind: AnnotationKind, anchor: Anchor, body: &str) -> AnnotationId {
        let op = self
            .session
            .editor
            .add_annotation(kind, anchor, body)
            .expect("the anchor fits");
        let Op::AddAnnotation { annotation } = &op else {
            unreachable!("an annotation is added by AddAnnotation");
        };
        let id = annotation.id;
        let landed = self.session.record(&op);
        self.check(landed, "annotating");
        id
    }

    fn on(&mut self, kind: AnnotationKind, id: BlockId, words: &str, body: &str) -> AnnotationId {
        let (start, end) = self.find(id, words);
        self.annotate(kind, Anchor::range(id, start, end), body)
    }

    fn delete(&mut self, id: BlockId, words: &str) {
        let (start, end) = self.find(id, words);
        let selection = Selection {
            start: self.at(id, start),
            end: self.at(id, end),
        };
        let landed = self.session.delete_range(selection, self.at(id, start));
        self.check(landed, "deleting");
    }
}

/// Writes `essay.kame`, holding every kind of block, style and annotation, with links, images at
/// relative paths, metadata with words and accepted marks, snapshots, a history of every kind of
/// step that ends in steps undone, and where the writer was, and what reading it gives.
fn write_document(scratch: &Path, out: &Path) -> Result<()> {
    std::fs::create_dir_all(scratch)?;
    let path = scratch.join("essay.kame");
    let mut w = Writer {
        session: open(&path)?,
    };

    let title = w.doc().blocks[0].id;
    w.type_at_end(title, "On marginalia");
    w.make(
        title,
        BlockKind::Heading {
            level: 1,
            centered: false,
        },
    );
    let landed = w.session.set_heading_centered(title, true);
    w.check(landed, "centering the title");
    let old = w.doc().title.clone();
    w.apply(
        Op::SetTitle {
            old,
            new: "On marginalia".to_string(),
        },
        "naming it",
    );

    let opening = w.split_after(
        title,
        BlockKind::Paragraph,
        "Marginalia are what a reader leaves beside the text: bold words, italic ones, \
         inline code, a link, another document, a heading here, and a citation [knuth84]. \
         Café, naïve, 日本語.",
    );
    w.styled(
        opening,
        &[
            ("bold words", SpanStyle::Bold),
            ("italic ones", SpanStyle::Italic),
            ("inline code", SpanStyle::Code),
            (
                "a link",
                SpanStyle::Link {
                    url: "https://example.com/marginalia".to_string(),
                },
            ),
            (
                "another document",
                SpanStyle::DocumentLink {
                    target: "notes/other.kame".to_string(),
                },
            ),
            (
                "a heading here",
                SpanStyle::DocumentLink {
                    target: "#Lists".to_string(),
                },
            ),
        ],
    );
    let marginal = w.annotate(
        AnnotationKind::Marginal,
        Anchor::block(opening),
        "A marginal, beside the whole paragraph.",
    );
    let landed = w.session.set_annotation_body(
        marginal,
        "A marginal, beside the whole paragraph, written again.",
    );
    w.check(landed, "writing in a marginal");
    w.on(
        AnnotationKind::Note,
        opening,
        "beside the text",
        "A note, which is private.",
    );

    let lists = w.split_after(
        opening,
        BlockKind::Heading {
            level: 2,
            centered: false,
        },
        "Lists",
    );
    let bullet = w.split_after(
        lists,
        BlockKind::ListItem {
            ordered: false,
            depth: 0,
        },
        "A bullet",
    );
    let nested = w.split_after(
        bullet,
        BlockKind::ListItem {
            ordered: false,
            depth: 0,
        },
        "A nested bullet",
    );
    let caret = w.at(nested, 0);
    w.session.set_caret(caret);
    let landed = w.session.indent_list_item(true);
    w.check(landed, "indenting");
    let first = w.split_after(
        nested,
        BlockKind::ListItem {
            ordered: true,
            depth: 0,
        },
        "First",
    );
    let second = w.split_after(
        first,
        BlockKind::ListItem {
            ordered: true,
            depth: 0,
        },
        "Second",
    );
    w.session.snapshot(Some("before the quotation"));

    let quote = w.split_after(
        second,
        BlockKind::BlockQuote,
        "A quotation, set in italics.",
    );
    // A marginal anchored as files did before whole-block anchors, which still reads.
    w.on(
        AnnotationKind::Marginal,
        quote,
        "quotation",
        "An older marginal, on a range.",
    );
    let sub = w.split_after(
        quote,
        BlockKind::Heading {
            level: 3,
            centered: false,
        },
        "Pictures and code",
    );

    let result = w
        .session
        .editor
        .insert_image_block(&w.at(sub, 0), "images/figure.png".to_string());
    let landed = w.session.commit(result);
    w.check(landed, "adding an image");
    let figure = w.doc().blocks[w.index(sub) + 1].id;
    let landed = w.session.set_block_kind(
        figure,
        BlockKind::Image {
            path: "images/figure.png".to_string(),
            link: Some("https://example.com/figure".to_string()),
            align: ImageAlign::Left,
        },
    );
    w.check(landed, "linking the image");
    let landed = w.session.set_image_align(figure, ImageAlign::Center);
    w.check(landed, "centering the image");
    w.append(
        BlockKind::Image {
            path: "../pictures/photo.jpg".to_string(),
            link: None,
            align: ImageAlign::Right,
        },
        "",
    );
    w.append(BlockKind::Rule, "");
    w.append(
        BlockKind::Code {
            language: Some("rust".to_string()),
        },
        "fn main() {\n    println!(\"marginalia\");\n}",
    );
    w.append(
        BlockKind::Code { language: None },
        "plain   code,\n  exactly",
    );
    w.append(
        BlockKind::Prose,
        "Verse keeps its lines,\n    and its spacing,\nexactly.",
    );

    // Words that come and go, so that the history holds deletes that took notes and spans with
    // them and cut a note short, and a block deleted.
    let scratch_block = w.append(BlockKind::Paragraph, "");
    w.type_at_end(scratch_block, "Scratch words to cut, and keep.");
    w.styled(scratch_block, &[("Scratch words", SpanStyle::Bold)]);
    w.on(
        AnnotationKind::Note,
        scratch_block,
        "words",
        "Inside what's cut.",
    );
    w.on(
        AnnotationKind::Note,
        scratch_block,
        "cut, and",
        "Cut short.",
    );
    w.delete(scratch_block, "words to cut");
    let all = w.text(scratch_block);
    w.delete(scratch_block, &all);
    let caret = w.at(scratch_block, 0);
    w.session.set_caret(caret);
    let landed = w.session.backspace();
    w.check(landed, "joining the empty block to the one before");

    let last = w.append(
        BlockKind::Paragraph,
        "The end, with a footnote, and a closing word.",
    );
    // Added after the one below, and numbered after it, since numbers go by reading order.
    w.on(
        AnnotationKind::Footnote,
        last,
        "a footnote",
        "The second footnote.",
    );
    w.on(
        AnnotationKind::Footnote,
        opening,
        "a citation",
        "The first footnote, citing [knuth84].",
    );
    let gone = w.annotate(AnnotationKind::Marginal, Anchor::block(last), "Taken away.");
    let result = w.session.editor.remove_annotation(gone);
    let landed = w.session.commit(result);
    w.check(landed, "removing a marginal");

    // As a merge from another machine arrives.
    let mut arrived = w.doc().clone();
    let at = w.index(last);
    arrived.blocks[at].text.push_str(" Merged in.");
    let landed = w.session.replace_document(arrived);
    w.check(landed, "replacing the document");

    w.session.set_metadata(Metadata {
        title: "On marginalia".to_string(),
        author: "A. Writer".to_string(),
        created: "2026-11-06".to_string(),
        published: "2026-11-07".to_string(),
        tags: vec!["format".to_string(), "samples".to_string()],
        words: vec!["marginalia".to_string(), "knuth".to_string()],
        accepted: vec![AcceptedMark {
            rule: "Compounds".to_string(),
            text: "naïve".to_string(),
        }],
        publish: true,
    });
    assert_eq!(w.session.last_error, None);
    w.session.snapshot(None);

    // Two steps undone, waiting to be redone.
    w.type_at_end(last, " An afterthought.");
    w.make(
        first,
        BlockKind::ListItem {
            ordered: false,
            depth: 0,
        },
    );
    for _ in 0..2 {
        let landed = w.session.undo();
        w.check(landed, "undoing");
    }
    let (start, _) = w.find(opening, "Café");
    let caret = w.at(opening, start + "Café".len());
    w.session.set_caret(caret);
    w.session.save_position(240.5);

    let kinds: Vec<&str> = w
        .doc()
        .blocks
        .iter()
        .map(|block| match block.kind {
            BlockKind::Paragraph => "paragraph",
            BlockKind::Heading { .. } => "heading",
            BlockKind::ListItem { .. } => "list item",
            BlockKind::BlockQuote => "quotation",
            BlockKind::Image { .. } => "image",
            BlockKind::Rule => "rule",
            BlockKind::Code { .. } => "code",
            BlockKind::Prose => "prose",
        })
        .collect();
    for kind in [
        "paragraph",
        "heading",
        "list item",
        "quotation",
        "image",
        "rule",
        "code",
        "prose",
    ] {
        assert!(kinds.contains(&kind), "no {kind} in the sample");
    }
    assert_eq!(w.session.history().1, 2, "two steps to redo");
    drop(w);
    for suffix in ["-wal", "-shm"] {
        assert!(
            !scratch.join(format!("essay.kame{suffix}")).exists(),
            "closing left a {suffix}"
        );
    }
    std::fs::copy(&path, out.join("essay.kame"))?;

    // What reading it gives, from a copy, as the tests read one.
    let copy = scratch.join("reading.kame");
    std::fs::copy(&path, &copy)?;
    let mut session = open(&copy)?;
    assert!(!session.newer && session.last_error.is_none());
    let metadata = session.published_metadata();
    let markdown = |session: &Session, document: &kame_core::model::Document| {
        to_markdown_with(document, &session.published_metadata(), &References::new())
    };
    std::fs::write(out.join("essay.md"), markdown(&session, session.document()))?;
    std::fs::write(
        out.join("essay.html"),
        to_html_with(session.document(), &metadata, &References::new()),
    )?;
    let mut snapshots = Vec::new();
    for snapshot in session.snapshots().expect("the snapshots list") {
        let document = session.db.restore_snapshot(snapshot.id)?;
        snapshots.push(json!({
            "label": snapshot.label,
            "timestamp": snapshot.timestamp,
            "markdown": markdown(&session, &document),
        }));
    }
    let (applied, undone) = session.history();
    let position = session.db.load_position().expect("a position");
    let head = session.document().clone();
    for _ in 0..applied {
        assert!(session.undo(), "undo: {:?}", session.last_error);
    }
    for _ in 0..applied + undone {
        assert!(session.redo(), "redo: {:?}", session.last_error);
    }
    let redone = markdown(&session, session.document());
    let expected = json!({
        "document": head,
        "metadata": session.metadata,
        "history": { "applied": applied, "undone": undone },
        "position": {
            "block": head.blocks.iter().position(|block| block.id == position.block_id),
            "offset": position.offset,
            "scroll": position.scroll,
        },
        "snapshots": snapshots,
        "redone": redone,
    });
    write_json(&out.join("essay.json"), &expected)
}

// --- the logs -------------------------------------------------------------

/// A machine with its own copy of a logged folder.
struct Machine {
    path: PathBuf,
    device: &'static str,
}

impl Machine {
    fn new(root: &Path, name: &str, device: &'static str) -> Result<Machine> {
        let folder = root.join(name);
        std::fs::create_dir_all(&folder)?;
        std::fs::write(folder.join(MARKER), "")?;
        Ok(Machine {
            path: folder.join("essay.kame"),
            device,
        })
    }

    fn sync(&self, session: &mut Session, now: i64) -> Result<()> {
        let mut logged = Logged::open(session, self.device).ok_or("the folder isn't logged")?;
        logged.sync(session, now)?;
        Ok(())
    }

    fn receive_from(&self, other: &Machine) -> Result<()> {
        let (from, to) = (log_folder(&other.path), log_folder(&self.path));
        std::fs::create_dir_all(&to)?;
        for entry in std::fs::read_dir(from)?.flatten() {
            std::fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
        Ok(())
    }

    fn own_log(&self) -> PathBuf {
        log_folder(&self.path).join(format!("{}.log", self.device))
    }
}

fn append_to(session: &mut Session, index: usize, words: &str) {
    let block = &session.document().blocks[index];
    let caret = Cursor {
        block_id: block.id,
        offset: block.text.len(),
    };
    session.set_caret(caret);
    assert!(session.insert(words), "{:?}", session.last_error);
}

/// Writes the logs of a document two machines wrote, which hold a merge and end in two heads that
/// change the same sentence, the earlier also moving a list item, and what merging them gives.
fn write_logs(scratch: &Path, out: &Path) -> Result<()> {
    let talos = Machine::new(scratch, "talos", "talos-aaaa")?;
    let titan = Machine::new(scratch, "titan", "titan-bbbb")?;
    let clock = 1_790_000_000;

    let mut there = open(&talos.path)?;
    {
        let mut w = Writer { session: there };
        let heading = w.doc().blocks[0].id;
        w.type_at_end(heading, "Two machines");
        w.make(
            heading,
            BlockKind::Heading {
                level: 1,
                centered: false,
            },
        );
        let one = w.split_after(
            heading,
            BlockKind::Paragraph,
            "Talos writes this sentence. Both share this one.",
        );
        w.styled(one, &[("Talos", SpanStyle::Bold)]);
        w.on(
            AnnotationKind::Footnote,
            one,
            "this sentence",
            "A footnote that travels.",
        );
        let two = w.split_after(
            one,
            BlockKind::Paragraph,
            "Titan writes here later. The end.",
        );
        w.annotate(
            AnnotationKind::Marginal,
            Anchor::block(two),
            "A marginal that travels.",
        );
        let item = BlockKind::ListItem {
            ordered: false,
            depth: 0,
        };
        let first = w.split_after(two, item.clone(), "First item.");
        w.split_after(first, item, "Second item.");
        w.session.set_metadata(Metadata {
            title: "Two machines".to_string(),
            author: "A. Writer".to_string(),
            created: "2026-11-06".to_string(),
            words: vec!["talos".to_string()],
            ..Metadata::default()
        });
        there = w.session;
    }
    talos.sync(&mut there, clock)?;

    titan.receive_from(&talos)?;
    assert!(build_from_logs(&titan.path)?, "titan built nothing");
    let mut here = open(&titan.path)?;
    titan.sync(&mut here, clock + 10)?;

    append_to(&mut there, 1, " Talos adds this.");
    talos.sync(&mut there, clock + 20)?;
    append_to(&mut here, 2, " Titan adds this.");
    titan.sync(&mut here, clock + 30)?;

    // The merge both then follow.
    titan.receive_from(&talos)?;
    titan.sync(&mut here, clock + 40)?;
    talos.receive_from(&titan)?;
    talos.sync(&mut there, clock + 50)?;
    assert_eq!(there.document().blocks, here.document().blocks);

    // Apart again, on the same sentence, with no sync between them.
    let changed = |session: &mut Session, who: &str| {
        let block = &session.document().blocks[1];
        let offset = block.text.find("Both share this one.").unwrap() + "Both share this one".len();
        session.set_caret(Cursor {
            block_id: block.id,
            offset,
        });
        assert!(session.insert(&format!(", says {who}")));
    };
    // The earlier machine also moves the second list item up, and the later one doesn't reorder
    // anything, so the merge keeps the move (merging.md section 4.1). A build that takes the later
    // side's order whatever the other did puts the items back, and fails the tests.
    let second = there.document().blocks[4].id;
    assert_eq!(there.document().blocks[4].text, "Second item.");
    there.set_caret(Cursor {
        block_id: second,
        offset: 0,
    });
    assert!(there.move_list_item(true), "{:?}", there.last_error);
    changed(&mut there, "talos");
    talos.sync(&mut there, clock + 60)?;
    changed(&mut here, "titan");
    let mut metadata = here.metadata.clone();
    metadata.words.push("titan".to_string());
    here.set_metadata(metadata);
    titan.sync(&mut here, clock + 70)?;
    drop((there, here));

    // Each machine's own log, as a sync tool would bring them together.
    let logged = out.join("logged");
    let logs = log_folder(&logged.join("essay.kame"));
    std::fs::create_dir_all(&logs)?;
    std::fs::write(logged.join(MARKER), "")?;
    for machine in [&talos, &titan] {
        std::fs::copy(
            machine.own_log(),
            logs.join(machine.own_log().file_name().unwrap()),
        )?;
    }

    // What reading and merging them gives, from a copy, as the tests do.
    let reading = scratch.join("reading");
    copy_folder(&logged, &reading)?;
    let path = reading.join("essay.kame");
    let store = Store::load(&log_folder(&path))?;
    assert!(!store.holds_newer() && !store.corrupt_lines());
    let heads: Vec<Value> = store
        .heads()
        .iter()
        .map(|id| {
            json!({
                "id": id,
                "document": store.document(id),
                "metadata": store.metadata(id),
            })
        })
        .collect();
    assert_eq!(heads.len(), 2, "two heads to merge");
    assert!(build_from_logs(&path)?);
    let mut session = open(&path)?;
    let mut logged = Logged::open(&session, READER).ok_or("the copy isn't logged")?;
    let outcome = logged.sync(&mut session, READ_AT)?;
    assert!(outcome.merged && outcome.notes > 0, "{outcome:?}");
    let items: Vec<&str> = session.document().blocks[3..]
        .iter()
        .map(|block| block.text.as_str())
        .collect();
    assert_eq!(items, ["Second item.", "First item."], "the move is kept");
    let merged = Store::load(&log_folder(&path))?.heads();
    assert_eq!(merged.len(), 1);
    let expected = json!({
        "heads": heads,
        "merged": {
            "id": merged[0],
            "document": session.document(),
            "markdown": to_markdown_with(
                session.document(),
                &session.published_metadata(),
                &References::new(),
            ),
        },
    });
    write_json(&out.join("logged.json"), &expected)
}

// --- files ----------------------------------------------------------------

fn open(path: &Path) -> Result<Session> {
    Session::open(path).map_err(|err| err.to_string().into())
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    std::fs::write(path, text)?;
    Ok(())
}

fn copy_folder(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)?.flatten() {
        let into = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_folder(&entry.path(), &into)?;
        } else {
            std::fs::copy(entry.path(), into)?;
        }
    }
    Ok(())
}
