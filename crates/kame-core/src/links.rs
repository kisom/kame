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
    root.and_then(|root| way_within_notebook(file, document, root))
        .unwrap_or_else(|| file.display().to_string())
}

/// The way from the folder of `document` to `file`, if both are inside the notebook at `root`
/// and exist. It still holds on another machine or once the notebook has moved.
pub fn way_within_notebook(file: &Path, document: &Path, root: &Path) -> Option<String> {
    let root = root.canonicalize().ok()?;
    let folder = document.parent()?.canonicalize().ok()?;
    let file = file.canonicalize().ok()?;
    if !folder.starts_with(&root) || !file.starts_with(&root) {
        return None;
    }
    Some(path_between(&folder, &file))
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

/// Takes out `.` and `..` by what they say, since what's named may not exist.
fn tidied(path: &Path) -> PathBuf {
    let mut tidy = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                tidy.pop();
            }
            other => tidy.push(other),
        }
    }
    tidy
}

/// `target`, a link copied from the document at `from`, as it has to be written to lead to the
/// same place from the document at `to`. Only a relative one depends on where it is, and
/// `www.` starts a web address, as it does in a document's text. `#Heading` means the document
/// it's in, so elsewhere it names `from`. If either document has no file, or no folder that can
/// be found, there's no way to say, and it stays as it is.
pub fn retargeted(target: &str, from: &Path, to: &Path) -> String {
    let (path, _) = split_document_target(target);
    let has_scheme = path.split_once(':').is_some_and(|(before, _)| {
        before
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
            && before
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    if has_scheme || path.starts_with('/') || path.starts_with('~') || path.starts_with("www.") {
        return target.to_string();
    }
    let real_folder = |document: &Path| document.parent()?.canonicalize().ok();
    let (Some(from_folder), Some(to_folder)) = (real_folder(from), real_folder(to)) else {
        return target.to_string();
    };
    let (Some(from_name), Some(to_name)) = (from.file_name(), to.file_name()) else {
        return target.to_string();
    };
    let same_folder = from_folder == to_folder;
    if same_folder && (from_name == to_name || !path.is_empty()) {
        return target.to_string();
    }
    let file = if path.is_empty() {
        from_folder.join(from_name)
    } else {
        tidied(&from_folder.join(path))
    };
    // What follows the path, as it was written, which is the `#` and the heading if there are any.
    let after = &target[path.len()..];
    let way = path_between(&to_folder, &file);
    // A way of nothing is the document's own folder, and an empty path would name the document.
    if way.is_empty() {
        format!("./{after}")
    } else if path.ends_with('/') {
        format!("{way}/{after}")
    } else {
        format!("{way}{after}")
    }
}

/// What a link to a folder means, as kame_notebook::INDEX has it, which core can't name.
const FOLDER_INDEX: &str = "Notebook.kame";

/// `target`, a link to a document that is now the file `new_file`, written the way it was
/// before and as the document at `document_after` has to say it. Whatever follows the path,
/// like a heading, is kept as it was written. A `~/` path stays under the home folder, an
/// absolute one stays absolute, and a link to a folder stays one only while the file is that
/// folder's index. Any other is the way from the document's folder to the file.
pub fn moved_target(target: &str, new_file: &Path, document_after: &Path) -> String {
    let (path, _) = split_document_target(target);
    if path.is_empty() {
        return target.to_string();
    }
    let after = &target[path.len()..];
    let real_file = match (
        new_file
            .parent()
            .and_then(|folder| folder.canonicalize().ok()),
        new_file.file_name(),
    ) {
        (Some(folder), Some(name)) => folder.join(name),
        _ => new_file.to_path_buf(),
    };
    if path.starts_with("~/") {
        let home = std::env::var("HOME").ok().map(PathBuf::from);
        let inside = home.and_then(|home| {
            [new_file, real_file.as_path()]
                .into_iter()
                .find_map(|file| {
                    let relative = file
                        .strip_prefix(&home)
                        .ok()
                        .or_else(|| file.strip_prefix(home.canonicalize().ok()?).ok())?;
                    Some(relative.display().to_string())
                })
        });
        return match inside {
            Some(relative) => format!("~/{relative}{after}"),
            None => format!("{}{after}", new_file.display()),
        };
    }
    if path.starts_with('/') || path.starts_with('~') {
        return format!("{}{after}", new_file.display());
    }
    let Some(folder) = document_after.parent().map(|folder| {
        folder
            .canonicalize()
            .unwrap_or_else(|_| folder.to_path_buf())
    }) else {
        return target.to_string();
    };
    if path.ends_with('/')
        && real_file
            .file_name()
            .is_some_and(|name| name == FOLDER_INDEX)
    {
        let named = tidied(&folder.join(path));
        let named = named.canonicalize().unwrap_or(named);
        if real_file.parent() == Some(named.as_path()) {
            return target.to_string();
        }
    }
    format!("{}{after}", path_between(&folder, &real_file))
}

/// `target`, a link in the document that moves from `from` to `to`, as it has to be written
/// afterward. `#Heading` means the document itself, so it stays; any other is `retargeted`.
pub fn own_target_after_move(target: &str, from: &Path, to: &Path) -> String {
    let (path, _) = split_document_target(target);
    if path.is_empty() {
        target.to_string()
    } else {
        retargeted(target, from, to)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn notebook(name: &str) -> PathBuf {
        let root = kame_test_dir::folder(&format!("links-{name}"))
            .canonicalize()
            .unwrap();
        std::fs::create_dir_all(root.join("static/plates")).unwrap();
        std::fs::create_dir_all(root.join("maritain/early")).unwrap();
        std::fs::write(root.join("static/plates/a.png"), "a").unwrap();
        std::fs::write(root.join("maritain/b.png"), "b").unwrap();
        root
    }

    #[test]
    fn a_moved_target_keeps_its_form() {
        let root = kame_test_dir::folder("moved-target")
            .canonicalize()
            .unwrap();
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        let doc = root.join("a/doc.kame");
        let new = root.join("b/new.kame");
        assert_eq!(moved_target("old.kame", &new, &doc), "../b/new.kame");
        assert_eq!(moved_target("old", &new, &doc), "../b/new.kame");
        assert_eq!(
            moved_target("old.kame#Part two", &new, &doc),
            "../b/new.kame#Part two"
        );
        let absolute = new.display().to_string();
        assert_eq!(
            moved_target(root.join("a/old.kame").to_str().unwrap(), &new, &doc),
            absolute
        );
    }

    #[test]
    fn a_tilde_target_stays_under_the_home_folder() {
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let new = home.join("notes/new.kame");
        assert_eq!(
            moved_target("~/notes/old.kame", &new, Path::new("/x/doc.kame")),
            "~/notes/new.kame"
        );
    }

    #[test]
    fn a_folder_link_stays_a_folder_link_only_while_it_is_the_index() {
        let root = kame_test_dir::folder("moved-index").canonicalize().unwrap();
        std::fs::create_dir_all(root.join("a")).unwrap();
        let doc = root.join("doc.kame");
        assert_eq!(
            moved_target("a/", &root.join("a/Notebook.kame"), &doc),
            "a/"
        );
        assert_eq!(
            moved_target("a/", &root.join("a/other.kame"), &doc),
            "a/other.kame"
        );
    }

    #[test]
    fn a_link_to_a_folder_stays_one_from_anywhere() {
        let root = kame_test_dir::folder("retargeted-folder")
            .canonicalize()
            .unwrap();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        let (at_root, in_a) = (root.join("doc.kame"), root.join("a/doc.kame"));
        assert_eq!(retargeted("../", &in_a, &at_root), "./");
        assert_eq!(retargeted("..", &in_a, &at_root), "./");
        assert_eq!(retargeted("../#Part", &in_a, &at_root), "./#Part");
        assert_eq!(retargeted("b/", &in_a, &at_root), "a/b/");
        assert_eq!(retargeted("a/b/", &at_root, &in_a), "b/");
        assert_eq!(retargeted("./", &at_root, &in_a), "../");
        assert_eq!(retargeted("a/#Part", &at_root, &in_a), "./#Part");
        assert_eq!(
            own_target_after_move("../#Part", &in_a, &at_root),
            "./#Part"
        );
        // Without its slash, it's written as it was.
        assert_eq!(retargeted("a/b", &at_root, &in_a), "b");
    }

    /// The way to a file always has the file's name in it, so it's never empty.
    #[test]
    fn a_moved_target_beside_the_document_is_never_empty() {
        let root = kame_test_dir::folder("moved-target-beside")
            .canonicalize()
            .unwrap();
        let (doc, new) = (root.join("doc.kame"), root.join("new.kame"));
        assert_eq!(moved_target("./", &new, &doc), "new.kame");
        assert_eq!(moved_target("../#Part", &new, &doc), "new.kame#Part");
        assert_eq!(moved_target("old", &new, &new), "new.kame");
    }

    #[test]
    fn a_moved_documents_heading_link_still_means_itself() {
        let root = kame_test_dir::folder("own-target").canonicalize().unwrap();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        let (from, to) = (root.join("a/doc.kame"), root.join("a/b/doc.kame"));
        assert_eq!(own_target_after_move("#Part two", &from, &to), "#Part two");
        assert_eq!(
            own_target_after_move("other.kame", &from, &to),
            "../other.kame"
        );
        assert_eq!(
            own_target_after_move("https://example.com", &from, &to),
            "https://example.com"
        );
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
