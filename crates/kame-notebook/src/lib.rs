//! A notebook: a folder of documents with a `notebook.yaml` in it, to move through, search and
//! publish. Nothing derived is stored, so documents are read as needed, one at a time, and this
//! reads them and, when asked, renames, moves and deletes them. See `docs/superpowers/specs/2026-11-03-notebook-design.md`.
//!
//! - [`Notebook`] finds a notebook and reads its settings
//! - [`list`] says what's in one
//! - [`search`] finds text across its documents
//! - [`links`] works out what links where, and where a link can create a document
//! - [`images`] brings an image from outside into the notebook
//! - [`manage`] checks where a document may go, and moves or deletes one
//! - [`stranded`] settles a database whose document was moved or deleted on another machine
//! - [`references`] keeps the sources its documents cite

pub mod images;
pub mod links;
pub mod list;
pub mod manage;
pub mod references;
pub mod search;
pub mod stranded;

pub use kame_sync::logs::log_folder;

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The file that marks a folder as a notebook.
pub const SETTINGS: &str = "notebook.yaml";
/// The index document of a folder. In a notebook's root it is the contents page.
pub const INDEX: &str = "Notebook.kame";

/// Whether `folder` is a notebook's root, holding a `notebook.yaml`. Finding a notebook stops at the
/// first going up, and every walk down from a root stops at any below it, since what's there is
/// that other notebook's and not this one's.
pub fn is_notebook_root(folder: &Path) -> bool {
    folder.join(SETTINGS).is_file()
}

/// What `notebook.yaml` holds. A typo in a key is refused, so it can't quietly do nothing.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    pub title: Option<String>,
    pub url: Option<String>,
    pub author: Option<String>,
    pub remote: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notebook {
    /// The folder holding `notebook.yaml`, with symbolic links resolved.
    pub root: PathBuf,
    pub settings: Settings,
}

impl Notebook {
    /// The notebook holding `path`, a document or a folder: the nearest `notebook.yaml` going up
    /// through its parents. `Err` says what's wrong with one that can't be read.
    pub fn find(path: &Path) -> Result<Option<Notebook>, String> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|err| err.to_string())?
                .join(path)
        };
        let Some(root) = absolute.ancestors().find(|folder| is_notebook_root(folder)) else {
            return Ok(None);
        };
        Notebook::open(root).map(Some)
    }

    /// The notebook whose `notebook.yaml` is in `root`.
    pub fn open(root: &Path) -> Result<Notebook, String> {
        let file = root.join(SETTINGS);
        let yaml = std::fs::read_to_string(&file)
            .map_err(|err| format!("could not read {}: {err}", file.display()))?;
        // An empty file is a fine notebook, with every setting left out, and so is one with
        // nothing but comments, as a new one has.
        let says_nothing = yaml
            .lines()
            .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'));
        let settings = if says_nothing {
            Settings::default()
        } else {
            yaml_serde::from_str(&yaml).map_err(|err| format!("{}: {err}", file.display()))?
        };
        let root = root.canonicalize().map_err(|err| err.to_string())?;
        Ok(Notebook { root, settings })
    }

    /// What the notebook is called: its `title`, or the folder's name.
    pub fn title(&self) -> String {
        self.settings
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .or_else(|| {
                self.root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "notebook".to_string())
    }

    /// The contents page, if it's there, which it is if it has only logs and is yet to be built.
    pub fn index(&self) -> Option<PathBuf> {
        let index = self.root.join(INDEX);
        (index.is_file() || kame_sync::logs::log_folder(&index).is_dir()).then_some(index)
    }
}

/// What a new `notebook.yaml` holds: only what could go in it, so that it starts with the folder's
/// name for a title and nothing set.
const NEW_SETTINGS: &str = "\
# All of these are optional. Take away the # to use one.
#
# title: A name, if not the folder's
#
# These are for publishing, with kame-site.
# url: https://example.net/
# author: Your Name
# remote: you@host:/srv/site/
";

/// Makes `root` a notebook, with any folders that takes, a `notebook.yaml`, and a contents page
/// unless it has one. A folder that's already a notebook is refused and left alone.
pub fn init(root: &Path) -> Result<Notebook, String> {
    let settings = root.join(SETTINGS);
    if settings.exists() {
        return Err(format!("{} is already a notebook", root.display()));
    }
    std::fs::create_dir_all(root)
        .map_err(|err| format!("could not make {}: {err}", root.display()))?;
    let index = root.join(INDEX);
    if !index.exists() {
        let root = root.canonicalize().map_err(|err| err.to_string())?;
        let title = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        links::create_document(&root.join(INDEX), title.as_deref())
            .map_err(|err| format!("could not make its contents page: {err}"))?;
    }
    // Last, since this is what makes it a notebook, so that a failure leaves it not one.
    std::fs::write(&settings, NEW_SETTINGS)
        .map_err(|err| format!("could not write {}: {err}", settings.display()))?;
    Notebook::open(root)
}

/// A document's text and properties, read and never changed, whatever form it's in: a database,
/// Markdown, or only revision logs, as on a machine that hasn't opened it yet.
pub fn read_document(
    path: &Path,
) -> Option<(kame_core::model::Document, kame_core::model::Metadata)> {
    if path.is_file() {
        if kame_core::session::is_markdown_path(path) {
            let source = std::fs::read_to_string(path).ok()?;
            let stem = path.file_stem()?.to_string_lossy();
            let imported = kame_core::import::markdown::from_markdown(
                &source,
                path.parent().unwrap_or(Path::new("")),
                &stem,
            );
            return Some((imported.document, imported.metadata));
        }
        let db = kame_core::storage::Db::open_read_only(path).ok()?;
        let doc = db.load_document().ok()?;
        let metadata = db.load_metadata().ok()?.unwrap_or_default();
        return Some((doc, metadata));
    }
    let store = kame_sync::revlog::Store::load(&kame_sync::logs::log_folder(path)).ok()?;
    let head = store.heads().into_iter().next()?;
    Some((store.document(&head), store.metadata(&head)))
}
