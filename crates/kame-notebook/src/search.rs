//! Finding text across a notebook, the same way Ctrl-F does on the page.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use kame_core::model::{Anchor, BlockId, Document};
use kame_core::text::find_in;

use crate::{read_document, Notebook};

/// One match.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub path: PathBuf,
    pub relative: String,
    pub title: String,
    /// Where it leads: the block, and the offset in it. A match in a note leads to where it hangs.
    pub block: BlockId,
    pub offset: usize,
    /// The line around the match.
    pub line: String,
}

/// How much of a line to show either side of a match, in characters.
const AROUND: usize = 40;

fn excerpt(text: &str, start: usize, end: usize) -> String {
    let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let line_end = text[end..].find('\n').map_or(text.len(), |at| end + at);
    let before: Vec<(usize, char)> = text[line_start..start].char_indices().collect();
    let from = before
        .len()
        .checked_sub(AROUND)
        .map_or(line_start, |at| line_start + before[at].0);
    let to = text[end..line_end]
        .char_indices()
        .nth(AROUND)
        .map_or(line_end, |(at, _)| end + at);
    let mut line = String::new();
    if from > line_start {
        line.push('…');
    }
    line.push_str(text[from..to].trim());
    if to < line_end {
        line.push('…');
    }
    line
}

/// Calls `found` for every match of `needle` in the notebook's documents, as it finds them, in
/// the order of the list, and returns how many there were. Blocks, margin notes, footnotes and
/// private notes are all read. `open` is what's on the screen, read in place of its file, and
/// `cancel` stops the search at the next document.
pub fn search(
    notebook: &Notebook,
    needle: &str,
    open: Option<(&Path, &Document)>,
    cancel: &AtomicBool,
    mut found: impl FnMut(Hit),
) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    for entry in notebook.documents() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let read;
        let (doc, title) = match open.filter(|(path, _)| same_file(path, &entry.path)) {
            Some((_, doc)) => (doc, crate::list::title_of(&entry.path)),
            None => {
                let Some((doc, metadata)) = read_document(&entry.path) else {
                    continue;
                };
                let title = [&metadata.title, &doc.title]
                    .into_iter()
                    .find(|title| !title.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| crate::list::title_of(&entry.path));
                read = doc;
                (&read, title)
            }
        };
        let mut hit = |block: BlockId, offset: usize, line: String| {
            count += 1;
            found(Hit {
                path: entry.path.clone(),
                relative: entry.relative.clone(),
                title: title.clone(),
                block,
                offset,
                line,
            });
        };
        for block in &doc.blocks {
            for (start, end) in find_in(&block.text, needle) {
                hit(block.id, start, excerpt(&block.text, start, end));
            }
        }
        for note in &doc.annotations {
            let (block, offset) = match &note.anchor {
                Anchor::Range {
                    block_id, start, ..
                } => (*block_id, *start),
                Anchor::Block(id) => (*id, 0),
            };
            for (start, end) in find_in(&note.body, needle) {
                hit(block, offset, excerpt(&note.body, start, end));
            }
        }
    }
    count
}

pub(crate) fn same_file(one: &Path, other: &Path) -> bool {
    match (one.canonicalize(), other.canonicalize()) {
        (Ok(one), Ok(other)) => one == other,
        _ => one == other,
    }
}
