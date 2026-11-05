//! What links where in a notebook, and where a link can make a new document.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use kame_core::links::{resolve_open_path, split_document_target};
use kame_core::model::{BlockKind, Document, SpanStyle};
use kame_core::storage::Db;

use crate::search::same_file;
use crate::{read_document, Notebook, INDEX};

/// Where following a link leads.
#[derive(Clone, Debug, PartialEq)]
pub enum Follow {
    /// A document to open.
    Open(PathBuf),
    /// A document that isn't there yet and may be created.
    Create(PathBuf),
    /// Some other file, for the system's viewer.
    View(PathBuf),
    /// Nowhere: it has rotted, or leads outside the notebook.
    Rotted,
}

/// Whether a link leaves for the web, by naming a scheme known to, or by `//`, another host. Anything
/// else that looks like a scheme may be a file with a colon in its name, so it isn't assumed to be
/// safe to publish as it stands.
pub fn is_web_link(link: &str) -> bool {
    if link.starts_with("//") {
        return true;
    }
    link.split_once(':').is_some_and(|(scheme, _)| {
        ["http", "https", "mailto", "ftp", "ftps", "tel", "sms"]
            .iter()
            .any(|known| scheme.eq_ignore_ascii_case(known))
    })
}

/// Whether a link is a path, rather than a web address or the like. A scheme is letters, digits
/// and a little punctuation before the first colon, so a colon further along a path doesn't make
/// one, and `//` leads to another host as it would in a browser.
pub fn is_relative(link: &str) -> bool {
    if link.is_empty() || link.starts_with('#') || link.starts_with("//") {
        return false;
    }
    let Some(colon) = link.find(':') else {
        return true;
    };
    let before = &link[..colon];
    let is_scheme = before
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && before
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    !is_scheme
}

/// Takes out `.` and `..` by what they say, since what's named may not exist yet.
fn tidied(path: &Path) -> PathBuf {
    let mut tidy = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                tidy.pop();
            }
            other => tidy.push(other),
        }
    }
    tidy
}

impl Notebook {
    /// Whether `path` is inside the notebook, going by the real location of as much of it as exists,
    /// so that a symbolic link can't lead out. A link pointing at nothing counts as existing, and
    /// has no real location, so that writing to it can't lead out either.
    pub fn holds(&self, path: &Path) -> bool {
        if !path.starts_with(&self.root) {
            return false;
        }
        path.ancestors()
            .find(|above| above.symlink_metadata().is_ok())
            .and_then(|above| above.canonicalize().ok())
            .is_some_and(|real| real.starts_with(&self.root))
    }

    /// Whether what's at `path`, inside the notebook, could be a document, going by where it is.
    fn in_documents_area(&self, path: &Path) -> bool {
        let Ok(inside) = path.strip_prefix(&self.root) else {
            return false;
        };
        let mut parts = inside.components().map(|part| part.as_os_str());
        let first = parts.next();
        if first.is_some_and(|first| {
            first == "static" || first == "images" || first == "theme" || first == "output"
        }) && inside.components().count() > 1
        {
            return false;
        }
        inside
            .components()
            .all(|part| !part.as_os_str().to_string_lossy().starts_with('.'))
    }

    /// Where a relative link leads from the document at `base`.
    pub fn follow(&self, target: &str, base: &Path) -> Follow {
        let (path, _heading) = split_document_target(target);
        if !is_relative(path) {
            return Follow::Rotted;
        }
        let is_folder = path.ends_with('/');
        let named = tidied(&resolve_open_path(path, base));
        if !self.holds(&named) {
            return Follow::Rotted;
        }
        if named.is_dir() || is_folder {
            let index = named.join(INDEX);
            if !self.holds(&index) {
                return Follow::Rotted;
            }
            // With only logs it's there already, as below.
            return if index.is_file() || kame_sync::logs::log_folder(&index).is_dir() {
                Follow::Open(index)
            } else if self.in_documents_area(&index) {
                Follow::Create(index)
            } else {
                Follow::Rotted
            };
        }
        if named.is_file() {
            let document = self.in_documents_area(&named)
                && named.extension().is_some_and(|extension| {
                    let extension = extension.to_string_lossy().to_lowercase();
                    extension == "kame" || extension == "md" || extension == "markdown"
                });
            return if document {
                Follow::Open(named)
            } else {
                Follow::View(named)
            };
        }
        if !self.in_documents_area(&named) {
            return Follow::Rotted;
        }
        for extension in [".kame", ".md"] {
            let mut candidate = named.clone().into_os_string();
            candidate.push(extension);
            let candidate = PathBuf::from(candidate);
            if !self.holds(&candidate) {
                return Follow::Rotted;
            }
            if candidate.is_file() {
                return Follow::Open(candidate);
            }
        }
        let with_kame = match named.extension() {
            None => {
                let mut with = named.into_os_string();
                with.push(".kame");
                PathBuf::from(with)
            }
            Some(extension) if extension.eq_ignore_ascii_case("kame") => named,
            Some(_) => return Follow::Rotted,
        };
        // With only logs it's there already, and building it is for whoever opens it.
        if kame_sync::logs::log_folder(&with_kame).is_dir() {
            return Follow::Open(with_kame);
        }
        Follow::Create(with_kame)
    }
}

/// What creating a document made, so that all of it can go again if it's left untouched.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Made {
    folders: Vec<PathBuf>,
}

/// Creates a document at `path` with any folders it needs. A heading named in the link becomes its
/// first heading, so that the link then leads somewhere. None of this is an edit.
pub fn create_document(path: &Path, heading: Option<&str>) -> std::io::Result<Made> {
    let mut made = Made::default();
    let mut missing: Vec<&Path> = path
        .ancestors()
        .skip(1)
        .take_while(|folder| !folder.exists())
        .collect();
    missing.reverse();
    for folder in missing {
        std::fs::create_dir(folder)?;
        made.folders.push(folder.to_path_buf());
    }
    let title = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut doc = Document::new(&title);
    if let Some(heading) = heading {
        doc.blocks[0].kind = BlockKind::Heading { level: 1 };
        doc.blocks[0].text = heading.to_string();
    }
    let mut db = Db::open(path).map_err(std::io::Error::other)?;
    db.save_document(&doc).map_err(std::io::Error::other)?;
    Ok(made)
}

/// Removes a document made by [`create_document`] and left unwritten, with what SQLite keeps beside
/// it, and the folders made for it if nothing else has gone in them. The caller vouches that it
/// was created this session and never edited, as this is the only thing in kame that deletes one.
pub fn remove_untouched(path: &Path, made: &Made) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let mut side = path.as_os_str().to_os_string();
        side.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(side));
    }
    for folder in made.folders.iter().rev() {
        // Only succeeds on an empty folder, which is the point.
        let _ = std::fs::remove_dir(folder);
    }
}

/// A document linking to another.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkFrom {
    pub path: PathBuf,
    pub relative: String,
    pub title: String,
}

/// The documents with a document link leading to `target`, not counting `target` itself. `open` is
/// what's on the screen, read in place of its file, and `cancel` stops at the next document.
pub fn linking_to(
    notebook: &Notebook,
    target: &Path,
    open: Option<(&Path, &Document)>,
    cancel: &AtomicBool,
) -> Vec<LinkFrom> {
    let mut found = Vec::new();
    for entry in notebook.documents() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if same_file(&entry.path, target) {
            continue;
        }
        let read;
        let doc = match open.filter(|(path, _)| same_file(path, &entry.path)) {
            Some((_, doc)) => doc,
            None => {
                let Some((doc, _)) = read_document(&entry.path) else {
                    continue;
                };
                read = doc;
                &read
            }
        };
        let leads_here = doc
            .blocks
            .iter()
            .flat_map(|block| block.spans.iter())
            .any(|span| match &span.style {
                SpanStyle::DocumentLink { target: link } => {
                    match notebook.follow(link, &entry.path) {
                        Follow::Open(file) => same_file(&file, target),
                        _ => false,
                    }
                }
                _ => false,
            });
        if leads_here {
            found.push(LinkFrom {
                path: entry.path.clone(),
                relative: entry.relative.clone(),
                title: crate::list::title_of(&entry.path),
            });
        }
    }
    found
}
