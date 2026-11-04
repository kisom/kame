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

/// The file an image block's stored path names. A relative one is read from the document's
/// folder, the same as a site build reads it, and whatever it was started from doesn't come into it.
pub fn image_file(stored: &str, document: Option<&Path>) -> PathBuf {
    let path = Path::new(stored);
    match document.and_then(Path::parent) {
        Some(folder) if !path.is_absolute() && !stored.contains("://") => folder.join(path),
        _ => path.to_path_buf(),
    }
}

/// What an image block should store for `file`, shown by the document at `document`. Inside
/// the notebook at `root` it's the way there from the document's folder, which still holds on
/// another machine or once the notebook has moved. Anywhere else it's `file` itself.
pub fn stored_image_path(file: &Path, document: &Path, root: Option<&Path>) -> String {
    let relative = (|| {
        let root = root?.canonicalize().ok()?;
        let folder = document.parent()?.canonicalize().ok()?;
        let file = file.canonicalize().ok()?;
        if !folder.starts_with(&root) || !file.starts_with(&root) {
            return None;
        }
        Some(path_between(&folder, &file))
    })();
    relative.unwrap_or_else(|| file.display().to_string())
}

/// The way from `folder` to `file`, both of them whole paths with nothing like `..` in them.
fn path_between(folder: &Path, file: &Path) -> String {
    let shared = folder
        .components()
        .zip(file.components())
        .take_while(|(a, b)| a == b)
        .count();
    let climbs = folder.components().count() - shared;
    let down = file
        .components()
        .skip(shared)
        .map(|part| part.as_os_str().to_string_lossy().into_owned());
    std::iter::repeat_n("..".to_string(), climbs)
        .chain(down)
        .collect::<Vec<_>>()
        .join("/")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn notebook(name: &str) -> PathBuf {
        let root = std::env::temp_dir()
            .join(format!("kame-links-{name}-{}", uuid::Uuid::new_v4()))
            .canonicalize_or_made();
        std::fs::create_dir_all(root.join("static/plates")).unwrap();
        std::fs::create_dir_all(root.join("maritain/early")).unwrap();
        std::fs::write(root.join("static/plates/a.png"), "a").unwrap();
        std::fs::write(root.join("maritain/b.png"), "b").unwrap();
        root
    }

    trait Made {
        fn canonicalize_or_made(self) -> PathBuf;
    }

    impl Made for PathBuf {
        fn canonicalize_or_made(self) -> PathBuf {
            std::fs::create_dir_all(&self).unwrap();
            self.canonicalize().unwrap()
        }
    }

    #[test]
    fn an_image_in_the_notebook_is_stored_as_the_way_from_the_document() {
        let root = notebook("inside");
        let plate = root.join("static/plates/a.png");
        let stored = |document: &str, file: &Path| {
            stored_image_path(file, &root.join(document), Some(&root))
        };
        assert_eq!(stored("essay.kame", &plate), "static/plates/a.png");
        assert_eq!(
            stored("maritain/essay.kame", &plate),
            "../static/plates/a.png"
        );
        assert_eq!(
            stored("maritain/early/essay.kame", &plate),
            "../../static/plates/a.png"
        );
        assert_eq!(
            stored("maritain/essay.kame", &root.join("maritain/b.png")),
            "b.png"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn what_is_stored_leads_back_to_the_image() {
        let root = notebook("back");
        let plate = root.join("static/plates/a.png");
        let document = root.join("maritain/early/essay.kame");
        let stored = stored_image_path(&plate, &document, Some(&root));
        let file = image_file(&stored, Some(&document));
        assert_eq!(file.canonicalize().unwrap(), plate);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_image_outside_the_notebook_keeps_its_whole_path() {
        let root = notebook("outside");
        let outside = root.with_extension("png");
        std::fs::write(&outside, "o").unwrap();
        let document = root.join("essay.kame");
        assert_eq!(
            stored_image_path(&outside, &document, Some(&root)),
            outside.display().to_string()
        );
        std::fs::remove_file(&outside).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_link_leading_out_of_the_notebook_counts_as_outside() {
        let root = notebook("linked");
        let outside = root.with_extension("png");
        std::fs::write(&outside, "o").unwrap();
        let linked = root.join("linked.png");
        std::os::unix::fs::symlink(&outside, &linked).unwrap();
        assert_eq!(
            stored_image_path(&linked, &root.join("essay.kame"), Some(&root)),
            linked.display().to_string()
        );
        std::fs::remove_file(&outside).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn without_a_notebook_or_a_file_the_whole_path_stays() {
        let root = notebook("none");
        let plate = root.join("static/plates/a.png");
        let document = root.join("essay.kame");
        assert_eq!(
            stored_image_path(&plate, &document, None),
            plate.display().to_string()
        );
        let gone = root.join("static/gone.png");
        assert_eq!(
            stored_image_path(&gone, &document, Some(&root)),
            gone.display().to_string()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_stored_path_is_read_from_the_documents_folder() {
        let document = Path::new("/notes/maritain/essay.kame");
        assert_eq!(
            image_file("../static/a.png", Some(document)),
            PathBuf::from("/notes/maritain/../static/a.png")
        );
        assert_eq!(
            image_file("/elsewhere/a.png", Some(document)),
            PathBuf::from("/elsewhere/a.png")
        );
        assert_eq!(
            image_file("https://e.com/a.png", Some(document)),
            PathBuf::from("https://e.com/a.png")
        );
        assert_eq!(image_file("a.png", None), PathBuf::from("a.png"));
    }
}
