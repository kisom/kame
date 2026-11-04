//! What's in a notebook.

use std::path::{Path, PathBuf};

use crate::{read_document, Notebook};

/// One file a notebook holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    /// The path from the notebook's root, with `/` between folders.
    pub relative: String,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Something kame opens and search reads.
    Document,
    /// A paper, an image, anything else: it opens in the system's viewer.
    File,
}

/// Folders in the root that hold no documents. Everything in `static/` publishes, and `theme/` is
/// how a site looks.
const NOT_DOCUMENTS: [&str; 2] = ["static", "theme"];
/// The folder a build goes in, which isn't part of the notebook at all.
const OUTPUT: &str = "output";

fn is_document_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    (lower.ends_with(".kame") || lower.ends_with(".md") || lower.ends_with(".markdown"))
        && !name.contains(".sync-conflict-")
}

/// Files that are part of how kame or a sync tool works, which aren't for anyone to open.
fn is_working_file(name: &str) -> bool {
    name.starts_with('.')
        || name.ends_with("-wal")
        || name.ends_with("-shm")
        || name.ends_with(".merged")
        || name.ends_with(".retired")
        || name.ends_with(".building")
        || name.contains(".sync-conflict-")
}

impl Notebook {
    /// Everything in the notebook, in order by path. Symbolic links aren't followed, so nothing
    /// outside it is listed and a loop can't catch this.
    pub fn entries(&self) -> Vec<Entry> {
        let mut found = Vec::new();
        self.walk(&self.root, true, &mut found);
        found.sort_by(|one, other| one.relative.cmp(&other.relative));
        found
    }

    /// Only the documents.
    pub fn documents(&self) -> Vec<Entry> {
        self.entries()
            .into_iter()
            .filter(|entry| entry.kind == Kind::Document)
            .collect()
    }

    fn walk(&self, folder: &Path, documents_here: bool, found: &mut Vec<Entry>) {
        let Ok(entries) = std::fs::read_dir(folder) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                // A document's logs. They stand for it only if it has no file here yet.
                let logs_of = name
                    .strip_prefix('.')
                    .and_then(|rest| rest.strip_suffix(".sync"));
                if let Some(document) = logs_of {
                    if documents_here
                        && document.to_lowercase().ends_with(".kame")
                        && !folder.join(document).exists()
                    {
                        self.push(found, folder.join(document), Kind::Document);
                    }
                    continue;
                }
                if name.starts_with('.') {
                    continue;
                }
                let at_root = folder == self.root;
                if at_root && name == OUTPUT {
                    continue;
                }
                let leaves_documents = at_root && NOT_DOCUMENTS.contains(&name.as_str());
                self.walk(&path, documents_here && !leaves_documents, found);
            } else if !is_working_file(&name) {
                if name == crate::SETTINGS && folder == self.root {
                    continue;
                }
                let kind = if documents_here && is_document_name(&name) {
                    Kind::Document
                } else {
                    Kind::File
                };
                self.push(found, path, kind);
            }
        }
    }

    fn push(&self, found: &mut Vec<Entry>, path: PathBuf, kind: Kind) {
        let relative = path
            .strip_prefix(&self.root)
            .unwrap_or(&path)
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        found.push(Entry {
            path,
            relative,
            kind,
        });
    }
}

/// What a document goes by: its title, or its file's name.
pub fn title_of(path: &Path) -> String {
    let stem = || {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        name.split('.').next().unwrap_or(&name).to_string()
    };
    let Some((doc, metadata)) = read_document(path) else {
        return stem();
    };
    [metadata.title, doc.title]
        .into_iter()
        .find(|title| !title.trim().is_empty())
        .unwrap_or_else(stem)
}

/// An entry with what it goes by, for lists.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub entry: Entry,
    pub title: String,
}

/// Every entry with its title, which reads every document, so it's for a background thread.
pub fn listed(notebook: &Notebook) -> Vec<Listed> {
    notebook
        .entries()
        .into_iter()
        .map(|entry| {
            let title = match entry.kind {
                Kind::Document => title_of(&entry.path),
                Kind::File => entry.relative.clone(),
            };
            Listed { entry, title }
        })
        .collect()
}

/// The list again if what's in the notebook differs from `known`, as when something arrives from
/// another machine. Looking costs little, since only a change means reading every document.
pub fn listed_if_changed(notebook: &Notebook, known: &[Listed]) -> Option<Vec<Listed>> {
    let entries = notebook.entries();
    let same = entries.len() == known.len()
        && entries
            .iter()
            .zip(known)
            .all(|(entry, item)| *entry == item.entry);
    (!same).then(|| listed(notebook))
}

/// How well `query`'s letters appear, in order, in `text`: lower is better. It counts the gaps
/// between them and how far in they start.
fn subsequence(query: &str, text: &str) -> Option<u32> {
    let mut score = 0u32;
    let mut letters = query.chars().flat_map(char::to_lowercase).peekable();
    let mut last: Option<u32> = None;
    for (at, c) in text.chars().flat_map(char::to_lowercase).enumerate() {
        let Some(wanted) = letters.peek() else {
            break;
        };
        if c == *wanted {
            let at = at as u32;
            score += match last {
                Some(previous) => (at - previous - 1) * 4,
                None => at,
            };
            last = Some(at);
            letters.next();
        }
    }
    letters.peek().is_none().then_some(score)
}

/// How well `query` matches a document, by title or path, or `None` if it doesn't.
pub fn rank(query: &str, title: &str, relative: &str) -> Option<u32> {
    match (subsequence(query, title), subsequence(query, relative)) {
        (Some(one), Some(other)) => Some(one.min(other)),
        (one, other) => one.or(other),
    }
}

/// Which of `all` match `query`, as indices, best first, with recently opened documents ahead of
/// equals. `recent` runs from the latest.
pub fn narrow(query: &str, all: &[Listed], recent: &[PathBuf]) -> Vec<usize> {
    let mut found: Vec<(u32, usize, usize)> = all
        .iter()
        .enumerate()
        .filter_map(|(at, item)| {
            let score = rank(query, &item.title, &item.entry.relative)?;
            let how_recent = recent
                .iter()
                .position(|path| *path == item.entry.path)
                .unwrap_or(usize::MAX);
            Some((score, how_recent, at))
        })
        .collect();
    if query.is_empty() {
        found.sort_by_key(|(_, how_recent, at)| (*how_recent, *at));
    } else {
        // A short query's scores are close, and what was just open is the likelier wish.
        found.sort_by_key(|(score, how_recent, at)| (score / 8, *how_recent, *score, *at));
    }
    found.into_iter().map(|(_, _, at)| at).collect()
}

/// A row of the browser.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub depth: usize,
    pub name: String,
    pub what: RowKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RowKind {
    /// A folder, by its path from the root.
    Folder(String),
    /// An entry, by its index in what was listed.
    Entry(usize),
}

/// The rows of the browser for `entries`, in order by path, with `open` the folders that show what
/// is inside them.
pub fn rows(entries: &[Entry], open: &std::collections::HashSet<String>) -> Vec<Row> {
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (at, entry) in entries.iter().enumerate() {
        let parts: Vec<&str> = entry.relative.split('/').collect();
        let mut folder = String::new();
        let mut showing = true;
        for (depth, part) in parts[..parts.len() - 1].iter().enumerate() {
            if !folder.is_empty() {
                folder.push('/');
            }
            folder.push_str(part);
            if showing && seen.insert(folder.clone()) {
                found.push(Row {
                    depth,
                    name: (*part).to_string(),
                    what: RowKind::Folder(folder.clone()),
                });
            }
            showing = showing && open.contains(&folder);
        }
        if showing {
            found.push(Row {
                depth: parts.len() - 1,
                name: parts[parts.len() - 1].to_string(),
                what: RowKind::Entry(at),
            });
        }
    }
    found
}
