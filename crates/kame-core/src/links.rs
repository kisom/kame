//! Where a link between documents leads, by the rules a click in kame follows, so that
//! anything publishing documents agrees with it.

use std::path::{Path, PathBuf};

/// Resolves a path typed into the Open prompt. A leading `~` expands to the
/// home directory, and a relative path resolves against the current document's
/// directory, so `foo.kame` opens beside the document already on screen.
pub fn resolve_open_path(typed: &str, base: &Path) -> PathBuf {
    let trimmed = typed.trim();
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let path = if let Some(rest) = trimmed.strip_prefix("~/") {
        home.map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(trimmed))
    } else if trimmed == "~" {
        home.unwrap_or_else(|| PathBuf::from(trimmed))
    } else {
        PathBuf::from(trimmed)
    };
    if path.is_absolute() {
        path
    } else if let Some(dir) = base.parent() {
        dir.join(path)
    } else {
        path
    }
}

/// A document link's target as its path and the heading it names, if any.
pub fn split_document_target(target: &str) -> (&str, Option<&str>) {
    match target.split_once('#') {
        Some((path, heading)) => (path, (!heading.is_empty()).then_some(heading)),
        None => (target, None),
    }
}

/// The file a document link's path names, relative to the document at `base`. A link can
/// leave off the extension, so `.kame` and then `.md` are tried after what was entered.
/// Only an existing file counts: opening a missing path would create it.
pub fn find_document(path: &str, base: &Path) -> Option<PathBuf> {
    let entered = resolve_open_path(path, base);
    if entered.is_file() {
        return Some(entered);
    }
    [".kame", ".md"].into_iter().find_map(|extension| {
        let mut candidate = entered.clone().into_os_string();
        candidate.push(extension);
        let candidate = PathBuf::from(candidate);
        candidate.is_file().then_some(candidate)
    })
}
