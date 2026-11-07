//! Markdown import fuzzed: the releases' own exports damaged, Markdown made up of the pieces that
//! open and close its constructs, and bytes of nothing in particular. Importing must never panic,
//! and must give a document kame can hold, which exports the same each time and imports again.
//! See `kame_test_dir::fuzz` for running it longer.

use std::path::Path;

use kame_core::export::{to_html_with, to_markdown_with};
use kame_core::import::markdown::from_markdown;
use kame_core::model::{Anchor, Document};
use kame_core::references::References;
use kame_test_dir::fuzz::{self, damage_bytes, Random};

fn samples() -> Vec<Vec<u8>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root).unwrap().flatten() {
        let path = entry.path().join("essay.md");
        if let Ok(bytes) = std::fs::read(path) {
            found.push(bytes);
        }
    }
    found.sort();
    assert!(!found.is_empty(), "no release's Markdown");
    found
}

/// Markdown made of the pieces that start and end its constructs, in no order.
fn made_up(random: &mut Random) -> String {
    const PIECES: &[&str] = &[
        "# ",
        "## ",
        "###### ",
        "####### ",
        "\n",
        "\n\n",
        "  \n",
        "- ",
        "* ",
        "+ ",
        "1. ",
        "2) ",
        "    ",
        "  ",
        "\t",
        "> ",
        ">> ",
        "```",
        "```rust\n",
        "~~~",
        "`",
        "**",
        "*",
        "__",
        "_",
        "~~",
        "[",
        "]",
        "(",
        ")",
        "](",
        "![",
        "[^1]",
        "[^1]: ",
        "[^x]",
        "<",
        ">",
        "</",
        "<b>",
        "<br>",
        "<!--",
        "-->",
        "&amp;",
        "&#0;",
        "|",
        "| --- |",
        "---",
        "***",
        "___",
        "\\",
        "http://example.com",
        "www.x.y",
        "[key]",
        "[@k]",
        "word",
        "words here",
        "é",
        "🙂",
        "\u{0}",
        "\r",
        "---\ntitle: T\n---\n",
        "title: x\n",
        "$",
        "{",
        "}",
        "#",
        "1.",
        "-",
    ];
    let mut text = String::new();
    for _ in 0..random.below(80) {
        if random.one_in(6) {
            text.push_str(&random.text(4));
        } else {
            text.push_str(random.pick(PIECES));
        }
    }
    text
}

/// Everything kame relies on about a document it holds.
fn check_holds(doc: &Document) {
    assert!(!doc.blocks.is_empty(), "no blocks");
    let mut ids: Vec<_> = doc.blocks.iter().map(|block| block.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), doc.blocks.len(), "block ids repeat");
    for block in &doc.blocks {
        let fits =
            |offset: usize| offset <= block.text.len() && block.text.is_char_boundary(offset);
        for span in &block.spans {
            assert!(
                span.start < span.end && fits(span.start) && fits(span.end),
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
}

fn import(source: &str) {
    let imported = from_markdown(source, Path::new("/nowhere"), "Fuzzed");
    let doc = &imported.document;
    check_holds(doc);
    let references = References::new();
    let markdown = to_markdown_with(doc, &imported.metadata, &references);
    assert_eq!(
        markdown,
        to_markdown_with(doc, &imported.metadata, &references)
    );
    let html = to_html_with(doc, &imported.metadata, &references);
    assert_eq!(html, to_html_with(doc, &imported.metadata, &references));
    // What kame exports, it can import again.
    check_holds(&from_markdown(&markdown, Path::new("/nowhere"), "Again").document);
}

fn one(seed: u64, samples: &[Vec<u8>]) {
    let mut random = Random::new(seed);
    let source = match random.below(4) {
        0 | 1 => {
            let mut bytes = random.pick(samples).clone();
            let other = random.pick(samples).clone();
            for _ in 0..random.between(1, 6) {
                if random.one_in(2) {
                    damage_bytes(&mut random, &mut bytes, &other);
                } else {
                    // Markdown's own pieces, put in anywhere.
                    let at = random.below(bytes.len() + 1);
                    let piece = made_up(&mut random);
                    bytes.splice(at..at, piece.into_bytes());
                }
            }
            String::from_utf8_lossy(&bytes).into_owned()
        }
        2 => made_up(&mut random),
        _ => String::from_utf8_lossy(&fuzz::random_bytes(&mut random, 400)).into_owned(),
    };
    import(&source);
}

#[test]
fn importing_damaged_and_made_up_markdown_never_panics() {
    let samples = samples();
    fuzz::short("importing_markdown", 400, |seed| one(seed, &samples));
}

#[test]
#[ignore = "long: run with --ignored, KAME_FUZZ_ITERATIONS and KAME_FUZZ_SEED"]
fn fuzz_importing_markdown() {
    let samples = samples();
    fuzz::long("fuzz_importing_markdown", |seed| one(seed, &samples));
}
