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
                if name.starts_with('.') {
                    continue;
                }
                // A document's logs. They stand for it only if it has no file here yet.
                if let Some(document) = name.strip_suffix(".sync") {
                    if documents_here
                        && document.to_lowercase().ends_with(".kame")
                        && !folder.join(document).exists()
                    {
                        self.push(found, folder.join(document), Kind::Document);
                    }
                    continue;
                }
                let at_root = folder == self.root;
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
