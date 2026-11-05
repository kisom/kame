//! Changing what's in a notebook: where a document may be put, and deleting one.

use std::path::{Path, PathBuf};

use crate::links::tidied;
use crate::search::same_file;
use crate::Notebook;
use kame_sync::logs::log_folder;

/// Where `typed` puts `document`, or why it can't go there. The path is taken from the notebook's
/// root, and gets the document's extension if it names none. Nothing is changed on disk.
pub fn check_new_path(
    notebook: &Notebook,
    document: &Path,
    typed: &str,
) -> Result<PathBuf, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Err("type a path for it".to_string());
    }
    if typed.starts_with('/') || typed.starts_with('~') {
        return Err("a path starts in the notebook, so it can't begin with / or ~".to_string());
    }
    if typed.ends_with('/') {
        return Err("that names a folder; end it with the document's name".to_string());
    }
    let mut path = tidied(&notebook.root.join(typed));
    if path == notebook.root || !path.starts_with(&notebook.root) {
        return Err("that leads outside the notebook".to_string());
    }
    let extension = document
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned())
        .unwrap_or_default();
    match path.extension() {
        None => {
            let mut named = path.into_os_string();
            named.push(".");
            named.push(&extension);
            path = PathBuf::from(named);
        }
        Some(given) => {
            if !given.to_string_lossy().eq_ignore_ascii_case(&extension) {
                return Err(format!("it has to stay a .{extension} document"));
            }
        }
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.contains(".sync-conflict-") {
        return Err("that name is the sync tool's, for a conflicting copy".to_string());
    }
    if !notebook.holds(&path) {
        return Err("that leads outside the notebook".to_string());
    }
    if !notebook.in_documents_area(&path) {
        return Err(
            "documents can't go in a hidden folder, or in static, images, theme or output"
                .to_string(),
        );
    }
    let taken = path.symlink_metadata().is_ok() || log_folder(&path).exists();
    if taken && !same_file(&path, document) {
        let relative = path.strip_prefix(&notebook.root).unwrap_or(&path);
        return Err(format!(
            "something is already at {}; delete it first",
            relative.display()
        ));
    }
    Ok(path)
}

/// The files kept for a document beside it: conflicting copies from a sync tool, each with the
/// copy a merge left of it, and databases retired from it. Names are matched whole, so a document
/// `XY` isn't taken for `X`.
pub fn belonging_files(path: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for copy in kame_sync::files::conflict_copies(path) {
        let mut merged = copy.clone().into_os_string();
        merged.push(".merged");
        let merged = PathBuf::from(merged);
        found.push(copy);
        if merged.symlink_metadata().is_ok() {
            found.push(merged);
        }
    }
    let (Some(stem), Some(folder)) = (path.file_stem(), path.parent()) else {
        return found;
    };
    let before = format!(".{}.retired", stem.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(folder) else {
        return found;
    };
    let mut retired: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|entry| {
            let name = entry
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Some(rest) = name
                .strip_prefix(&before)
                .and_then(|rest| rest.strip_suffix(".kame"))
            else {
                return false;
            };
            rest.is_empty()
                || rest
                    .strip_prefix('-')
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
        .collect();
    retired.sort();
    found.extend(retired);
    found
}

/// Deletes a document, and with it what belongs to it: what SQLite keeps beside the file, its
/// conflicting and retired copies, and its logs. A file that can't be removed is an error before
/// the logs are touched, so that a document left in place keeps them.
pub fn delete_document(path: &Path) -> std::io::Result<()> {
    let is_database = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("kame"));
    if !is_database {
        return std::fs::remove_file(path);
    }
    // Found first, since which copies are this document's is told by reading it.
    let belonging = belonging_files(path);
    std::fs::remove_file(path)?;
    let mut sqlite = Vec::new();
    for suffix in ["-wal", "-shm"] {
        let mut beside = path.as_os_str().to_owned();
        beside.push(suffix);
        sqlite.push(PathBuf::from(beside));
    }
    for file in sqlite.into_iter().chain(belonging) {
        match std::fs::remove_file(&file) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
    }
    let logs = log_folder(path);
    if logs.is_dir() {
        std::fs::remove_dir_all(&logs)?;
    }
    Ok(())
}
