//! How long the page takes over a real document, to keep kame a responsive place to write. These
//! are ignored, as they take a while and need a document of some size. Run them, in release, with
//!
//! ```sh
//! KAME_PERF_DOC=path/to/doc.kame cargo test --release --bin kame perf -- --ignored --nocapture
//! ```
//!
//! The document is copied first, so the one named is never changed. `KAME_PERF_KEEP=path` also keeps
//! the copy ten times over, before it's typed in, to open in the app.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use kame_core::edit::Cursor;
use kame_core::model::{BlockKind, Document};
use kame_core::session::Session;

use crate::{test_temp_dir, Headless};

/// The document to measure, if one was named.
fn named_document() -> Option<PathBuf> {
    std::env::var_os("KAME_PERF_DOC").map(PathBuf::from)
}

/// A copy of `from` to measure, in a folder of its own.
fn copy_of(from: &Path, name: &str) -> PathBuf {
    let folder = test_temp_dir().join(format!("perf-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&folder).unwrap();
    let to = folder.join("doc.kame");
    std::fs::copy(from, &to).unwrap();
    to
}

/// `document` with its blocks `times` over, each copy with blocks of its own. Annotations stay on
/// the first copy.
fn repeated(document: &Document, times: usize) -> Document {
    let mut bigger = document.clone();
    for _ in 1..times {
        for block in &document.blocks {
            let mut copy = block.clone();
            copy.id = uuid::Uuid::new_v4();
            bigger.blocks.push(copy);
        }
    }
    bigger
}

/// Durations, summed up.
struct Spread {
    what: String,
    times: Vec<Duration>,
}

impl Spread {
    fn new(what: &str) -> Spread {
        Spread {
            what: what.to_string(),
            times: Vec::new(),
        }
    }

    fn time(&mut self, run: impl FnOnce()) {
        let start = Instant::now();
        run();
        self.times.push(start.elapsed());
    }

    fn report(&self) -> String {
        let mut sorted = self.times.clone();
        sorted.sort();
        let at = |fraction: f64| {
            let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
            sorted[index].as_secs_f64() * 1000.0
        };
        let mean = sorted.iter().sum::<Duration>().as_secs_f64() * 1000.0 / sorted.len() as f64;
        format!(
            "{:<34} n={:<4} mean {:>7.2} ms  p50 {:>7.2}  p95 {:>7.2}  max {:>7.2}",
            self.what,
            sorted.len(),
            mean,
            at(0.5),
            at(0.95),
            at(1.0)
        )
    }
}

/// The caret in the middle of the longest paragraph halfway down, where typing makes the page
/// work hardest.
fn middle_of_a_long_paragraph(document: &Document) -> Cursor {
    let half = document.blocks.len() / 2;
    let block = document.blocks[half..]
        .iter()
        .chain(&document.blocks[..half])
        .filter(|block| block.kind == BlockKind::Paragraph)
        .max_by_key(|block| block.text.len())
        .expect("a paragraph");
    let mut offset = block.text.len() / 2;
    while !block.text.is_char_boundary(offset) {
        offset += 1;
    }
    Cursor {
        block_id: block.id,
        offset,
    }
}

fn words_in(document: &Document) -> usize {
    document
        .blocks
        .iter()
        .map(|block| block.text.split_whitespace().count())
        .sum()
}

/// Measures typing, deleting, scrolling and idle frames over the document at `path`.
fn measure(path: &Path, label: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut opening = Spread::new("open the file");
    let mut session = None;
    opening.time(|| session = Some(Session::open(path).unwrap()));
    let mut session = session.unwrap();
    let words = words_in(session.document());
    let caret = middle_of_a_long_paragraph(session.document());
    session.set_caret(caret);
    lines.push(format!(
        "{label}: {words} words, {} blocks",
        session.document().blocks.len()
    ));
    lines.push(opening.report());

    let mut page = Headless::new(session);
    page.app.check_spelling = true;
    let mut first = Spread::new("first frame");
    first.time(|| page.frame(Vec::new()));
    lines.push(first.report());
    for _ in 0..5 {
        page.frame(Vec::new());
    }

    let mut idle = Spread::new("a frame with nothing new");
    for _ in 0..100 {
        idle.time(|| page.frame(Vec::new()));
    }
    lines.push(idle.report());

    let mut typing = Spread::new("a frame typing one letter");
    for letter in "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(1200)
    {
        typing.time(|| page.frame(vec![egui::Event::Text(letter.to_string())]));
    }
    lines.push(typing.report());
    let median = {
        let mut sorted = typing.times.clone();
        sorted.sort();
        sorted[sorted.len() / 2]
    };
    let slow: Vec<String> = typing
        .times
        .iter()
        .enumerate()
        .filter(|(_, time)| **time > median * 2)
        .map(|(at, time)| format!("#{at} {:.1}ms", time.as_secs_f64() * 1000.0))
        .collect();
    lines.push(format!("  slow typing frames: {}", slow.join(", ")));

    let mut deleting = Spread::new("a frame deleting one letter");
    for _ in 0..100 {
        deleting.time(|| {
            page.frame(vec![egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }])
        });
    }
    lines.push(deleting.report());

    let mut scrolling = Spread::new("a frame scrolling");
    page.frame(vec![egui::Event::PointerMoved(egui::pos2(500.0, 400.0))]);
    for step in 0..120 {
        let down = if step < 60 { -3.0 } else { 3.0 };
        scrolling.time(|| {
            page.frame(vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, down),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
            }])
        });
    }
    lines.push(scrolling.report());

    let mut storing = Spread::new("store one letter, no page");
    let mut session = Session::open(path).unwrap();
    session.set_caret(middle_of_a_long_paragraph(session.document()));
    for letter in "a sentence typed straight into the file "
        .chars()
        .cycle()
        .take(200)
    {
        storing.time(|| {
            session.insert(&letter.to_string());
        });
    }
    lines.push(storing.report());
    lines
}

#[test]
#[ignore = "needs KAME_PERF_DOC, and a while"]
fn how_long_the_page_takes() {
    let Some(named) = named_document() else {
        eprintln!("KAME_PERF_DOC names no document, so there is nothing to measure");
        return;
    };
    let mut lines = measure(&copy_of(&named, "as-is"), "as it is");

    let bigger = copy_of(&named, "ten-times");
    {
        let mut session = Session::open(&bigger).unwrap();
        let ten = repeated(session.document(), 10);
        assert!(session.replace_document(ten));
    }
    // To try the bigger one in the app as well.
    if let Some(keep) = std::env::var_os("KAME_PERF_KEEP") {
        std::fs::copy(&bigger, keep).unwrap();
    }
    lines.extend(measure(&bigger, "ten times over"));
    eprintln!("\n{}\n", lines.join("\n"));
}
