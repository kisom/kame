//! A notebook: a folder of documents with a `notebook.yaml` in it, to move through, search and
//! publish. Nothing derived is stored, so documents are read as needed, one at a time, and this
//! only ever reads them. See `docs/superpowers/specs/2026-11-03-notebook-design.md`.
//!
//! - [`Notebook`] finds a notebook and reads its settings
//! - [`list`] says what's in one
//! - [`search`] finds text across its documents
//! - [`links`] works out what links where, and where a link can create a document

pub mod links;
pub mod list;
pub mod search;

pub use kame_sync::logs::log_folder;

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The file that marks a folder as a notebook.
pub const SETTINGS: &str = "notebook.yaml";
/// The index document of a folder. In a notebook's root it is the contents page.
pub const INDEX: &str = "Notebook.kame";

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
        let Some(root) = absolute
            .ancestors()
            .find(|folder| folder.join(SETTINGS).is_file())
        else {
            return Ok(None);
        };
        Notebook::open(root).map(Some)
    }

    /// The notebook whose `notebook.yaml` is in `root`.
    pub fn open(root: &Path) -> Result<Notebook, String> {
        let file = root.join(SETTINGS);
        let yaml = std::fs::read_to_string(&file)
            .map_err(|err| format!("could not read {}: {err}", file.display()))?;
        // An empty file is a fine notebook, with every setting left out.
        let settings = if yaml.trim().is_empty() {
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

/// A document's text and properties, read and never changed, whatever form it's in: a database,
/// Markdown, or only revision logs, as on a machine that hasn't opened it yet.
pub fn read_document(
    path: &Path,
) -> Option<(kame_core::model::Document, kame_core::model::Metadata)> {
    if path.is_file() {
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        {
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
