//! Bringing an image into a notebook, so that everything a document shows travels with it.

use std::path::{Path, PathBuf};

use crate::Notebook;

/// Where images from outside a notebook are kept, from its root. A folder for each document
/// goes under this, following where the document is. Unlike `static/`, nothing here publishes
/// unless a published document shows it.
pub const IMAGES: &str = "images";

/// The file a document in `notebook` should show for `file`. One already inside the notebook is
/// `file` itself. One outside is copied to `images/path/to/document/`, so that
/// `/n/maritain/essay.kame` gets `/n/images/maritain/essay/plate.jpg`.
///
/// Nothing already there is replaced: the same picture is used again, and a different one with
/// that name leaves this one as `plate-2.jpg`.
pub fn bring_in(notebook: &Notebook, document: &Path, file: &Path) -> Result<PathBuf, String> {
    let said = |err: std::io::Error| format!("could not copy {}: {err}", file.display());
    let real = file.canonicalize().map_err(said)?;
    if notebook.holds(&real) {
        return Ok(file.to_path_buf());
    }
    let folder = notebook
        .root
        .join(IMAGES)
        .join(document_folder(notebook, document)?);
    std::fs::create_dir_all(&folder).map_err(said)?;
    for candidate in names(file) {
        let target = folder.join(candidate);
        if !target.exists() {
            copy_whole(&real, &target).map_err(said)?;
            return Ok(target);
        }
        if same_contents(&real, &target).map_err(said)? {
            return Ok(target);
        }
    }
    unreachable!("names never run out")
}

/// The document's place in the notebook, with the name of its file as the last folder.
fn document_folder(notebook: &Notebook, document: &Path) -> Result<PathBuf, String> {
    let outside = || format!("{} isn't in this notebook", document.display());
    let folder = document
        .parent()
        .and_then(|folder| folder.canonicalize().ok())
        .ok_or_else(outside)?;
    let place = folder.strip_prefix(&notebook.root).map_err(|_| outside())?;
    let name = document.file_stem().ok_or_else(outside)?;
    Ok(place.join(name))
}

/// `plate.jpg`, then `plate-2.jpg`, and so on.
fn names(file: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let extension = file
        .extension()
        .map(|extension| extension.to_string_lossy());
    (1..).map(move |count| {
        let mut name = if count == 1 {
            stem.to_string()
        } else {
            format!("{stem}-{count}")
        };
        if let Some(extension) = &extension {
            name.push('.');
            name.push_str(extension);
        }
        PathBuf::from(name)
    })
}

/// Copies under a hidden name first, so a failure can't leave half a picture under the real one.
fn copy_whole(from: &Path, to: &Path) -> std::io::Result<()> {
    let name = to.file_name().unwrap_or_default().to_string_lossy();
    let partial = to.with_file_name(format!(".{name}.copying-{}", std::process::id()));
    let copied = std::fs::copy(from, &partial).and_then(|_| std::fs::rename(&partial, to));
    if copied.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    copied
}

fn same_contents(a: &Path, b: &Path) -> std::io::Result<bool> {
    if std::fs::metadata(a)?.len() != std::fs::metadata(b)?.len() {
        return Ok(false);
    }
    Ok(std::fs::read(a)? == std::fs::read(b)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        base: PathBuf,
        notebook: Notebook,
        outside: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn fixture(name: &str) -> Fixture {
        let base = kame_test_dir::folder(&format!("images-{name}"));
        std::fs::create_dir_all(base.join("notes/maritain/early")).unwrap();
        std::fs::create_dir_all(base.join("pictures")).unwrap();
        std::fs::write(base.join("notes").join(crate::SETTINGS), "").unwrap();
        let notebook = Notebook::open(&base.join("notes")).unwrap();
        let outside = base.join("pictures/plate.jpg");
        std::fs::write(&outside, "a plate").unwrap();
        Fixture {
            base,
            notebook,
            outside,
        }
    }

    #[test]
    fn an_image_from_outside_is_copied_under_the_documents_place() {
        let f = fixture("copied");
        let root = &f.notebook.root;
        let document = root.join("maritain/early/essay.kame");
        let brought = bring_in(&f.notebook, &document, &f.outside).unwrap();
        assert_eq!(brought, root.join("images/maritain/early/essay/plate.jpg"));
        assert_eq!(std::fs::read_to_string(&brought).unwrap(), "a plate");
        assert!(f.outside.exists(), "the original stays where it was");
    }

    #[test]
    fn a_document_in_the_root_gets_a_folder_of_its_name() {
        let f = fixture("root");
        let root = &f.notebook.root;
        let brought = bring_in(&f.notebook, &root.join("essay.kame"), &f.outside).unwrap();
        assert_eq!(brought, root.join("images/essay/plate.jpg"));
    }

    #[test]
    fn an_image_already_inside_is_left_where_it_is() {
        let f = fixture("inside");
        let root = &f.notebook.root;
        let inside = root.join("maritain/figure.png");
        std::fs::write(&inside, "a figure").unwrap();
        let brought = bring_in(&f.notebook, &root.join("essay.kame"), &inside).unwrap();
        assert_eq!(brought, inside);
        assert!(!root.join(IMAGES).exists());
    }

    #[test]
    fn a_link_leading_outside_copies_the_picture_under_the_links_name() {
        let f = fixture("linked");
        let root = &f.notebook.root;
        let linked = root.join("linked.jpg");
        std::os::unix::fs::symlink(&f.outside, &linked).unwrap();
        let brought = bring_in(&f.notebook, &root.join("essay.kame"), &linked).unwrap();
        assert_eq!(brought, root.join("images/essay/linked.jpg"));
        assert!(!brought.is_symlink());
        assert_eq!(std::fs::read_to_string(&brought).unwrap(), "a plate");
    }

    #[test]
    fn the_same_picture_again_is_not_copied_twice() {
        let f = fixture("again");
        let document = f.notebook.root.join("essay.kame");
        let first = bring_in(&f.notebook, &document, &f.outside).unwrap();
        let second = bring_in(&f.notebook, &document, &f.outside).unwrap();
        assert_eq!(first, second);
        let copies = std::fs::read_dir(first.parent().unwrap()).unwrap().count();
        assert_eq!(copies, 1);
    }

    #[test]
    fn a_different_picture_with_that_name_replaces_nothing() {
        let f = fixture("different");
        let document = f.notebook.root.join("essay.kame");
        let first = bring_in(&f.notebook, &document, &f.outside).unwrap();
        let other = f.base.join("pictures/other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("plate.jpg"), "another").unwrap();
        let second = bring_in(&f.notebook, &document, &other.join("plate.jpg")).unwrap();
        assert_eq!(second.file_name().unwrap(), "plate-2.jpg");
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "a plate");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "another");
    }

    #[test]
    fn a_file_that_cannot_be_read_says_so() {
        let f = fixture("gone");
        let document = f.notebook.root.join("essay.kame");
        let gone = f.base.join("pictures/gone.jpg");
        let said = bring_in(&f.notebook, &document, &gone).unwrap_err();
        assert!(said.contains("gone.jpg"), "{said}");
        assert!(!f.notebook.root.join(IMAGES).exists());
    }

    #[test]
    fn a_document_outside_the_notebook_copies_nothing() {
        let f = fixture("stray");
        let document = f.base.join("pictures/essay.kame");
        assert!(bring_in(&f.notebook, &document, &f.outside).is_err());
        assert!(!f.notebook.root.join(IMAGES).exists());
    }
}
